;;; requirement: a literal pair is immutable, and mutating one raises at every optimisation level
;;; found-in: vali
;;; kind: scheme

;; The optimiser folded (car '(1 2)) to 1 even after set-car! changed the
;; literal, so the answer depended on the optimisation level. Literal pairs
;; are now immutable (#60, ADR 0020): they live on frozen pages outside the
;; heap, and every write path checks. R7RS calls mutating a literal an error
;; without requiring it to be signalled; Vali does, as it does for strings,
;; bytevectors and vectors (literal-vectors-immutable). Run with VALI_NO_OPT=1
;; as well. A scheme test runs under every memory strategy.

(import (scheme base) (scheme read) (srfi 64))

(define (lit) '(1 2))
(define big '(5 4 3 2 1))
(define dotted '(1 . 2))

(test-begin "mutated-literals-same-at-every-opt-level")

;; The VM's inline set-car and set-cdr, through a variable, a call and a
;; procedure the optimiser cannot see through.
(test-error (set-car! (lit) 9))
(test-error (set-cdr! (lit) 9))
(test-error (let ((p '(1 2))) (set-car! p 9) (car p)))
(test-error (let ((p '(1 2))) (set-cdr! p '()) (cdr p)))
(test-error (let ((p '(1 . 2))) (set-cdr! p 5)))
(test-error (let ((f set-car!)) (f (lit) 9)))
(test-error (let ((f set-cdr!)) (f (lit) 9)))
(test-error (apply set-car! (list (lit) 9)))
(test-error (apply set-cdr! (list (lit) 9)))
(test-error (list-set! (lit) 0 9))
(test-error (list-set! (lit) 1 9))
(test-error (for-each (lambda (p) (set-car! p 0)) (list (lit))))
;; Every pair of a literal is frozen, however deep.
(test-error (set-car! (cdr (lit)) 9))
(test-error (set-cdr! (cdr big) 9))
(test-error (set-car! (car '((1) 2)) 9))
(test-error (set-car! (vector-ref '#((1 2) 3) 0) 9))
(test-error (set-car! (cdr '(1 . (2 3))) 9))
(test-error (set-cdr! dotted 9))
(test-error (set-car! '(()) 9))
;; A pair inside a vector, and a vector inside a pair, are both frozen.
(test-error (vector-set! (car '(#(1 2))) 0 9))
;; Two equal literals are separate objects, and a quote yields one object.
(test-assert (not (eq? (lit) '(1 2))))
(test-assert (eq? (lit) (lit)))
(test-assert (eq? big big))
(test-assert (eq? (cdr big) (cdr big)))
;; A failed write leaves the literal as it was, and what it still says
;; is what the optimiser may assume.
(test-equal '(1 2) (begin (guard (e (#t #f)) (set-car! (lit) 9)) (lit)))
(test-equal '(1 2) (begin (guard (e (#t #f)) (set-cdr! (lit) '())) (lit)))
(test-equal 1 (begin (guard (e (#t #f)) (set-car! (lit) 9)) (car (lit))))
(test-equal '(5 4 3 2 1)
            (begin (guard (e (#t #f)) (list-set! big 2 0)) big))
(test-equal 5 (let ((p '(5 4))) (guard (e (#t #f)) (set-car! p 9)) (car p)))

;; The error message says what is wrong and names the procedure.
(define (contains? s sub)
  (let ((n (string-length s)) (m (string-length sub)))
    (let loop ((i 0))
      (cond ((> (+ i m) n) #f)
            ((string=? (substring s i (+ i m)) sub) #t)
            (else (loop (+ i 1)))))))
(define-syntax message-of
  (syntax-rules ()
    ((_ expr) (guard (e ((error-object? e) (error-object-message e))) expr "no error"))))
(test-assert (contains? (message-of (set-car! (lit) 9)) "immutable"))
(test-assert (contains? (message-of (set-car! (lit) 9)) "set-car!"))
(test-assert (contains? (message-of (set-cdr! (lit) 9)) "immutable"))
(test-assert (contains? (message-of (set-cdr! (lit) 9)) "set-cdr!"))
(test-assert (contains? (message-of (let ((f set-car!)) (f (lit) 9))) "immutable"))
(test-assert (contains? (message-of (apply set-cdr! (list (lit) 9))) "set-cdr!"))
(test-assert (contains? (message-of (list-set! (lit) 0 9)) "immutable"))

;; A value that is not a pair is a type error, before immutability, however
;; the write is made.
(test-assert (not (contains? (message-of (set-car! 5 9)) "immutable")))
(test-assert (not (contains? (message-of (set-cdr! '() 9)) "immutable")))
(test-assert (not (contains? (message-of (set-car! "ab" 9)) "immutable")))
(test-assert (not (contains? (message-of (let ((f set-car!)) (f '#(1) 9))) "immutable")))
(test-assert (contains? (message-of (set-car! 5 9)) "pair"))
(test-assert (contains? (message-of (apply set-cdr! (list '() 9))) "pair"))
;; list-set! reports a bad index before immutability.
(test-assert (not (contains? (message-of (list-set! (lit) 5 9)) "immutable")))

;; Fresh pairs stay mutable, whatever they are made of, even when the
;; optimiser could see every argument is a constant.
(test-equal 9 (let ((p (cons 1 2))) (set-car! p 9) (car p)))
(test-equal '(1 . 9) (let ((p (cons 1 2))) (set-cdr! p 9) p))
(test-equal '(9 2) (let ((p (list 1 2))) (set-car! p 9) p))
(test-equal '(1 9) (let ((p (list 1 2))) (set-car! (cdr p) 9) p))
(test-equal '(1) (let ((p (list 1 2))) (set-cdr! p '()) p))
(test-equal '(9 2 3) (let ((p (list 1 2 3))) (list-set! p 0 9) p))
(test-equal '(1 2 9) (let ((p (list 1 2 3))) (list-set! p 2 9) p))
(test-equal '(9 2) (let ((p (list-copy '(1 2)))) (set-car! p 9) p))
(test-equal '(9 2) (let ((p (append '(1) '(2)))) (set-car! p 9) p))
(test-equal '(9 2) (let ((p (map (lambda (x) x) '(1 2)))) (set-car! p 9) p))
(test-equal '#(9 2) (let ((v (vector 1 2))) (vector-set! v 0 9) v))
(test-equal '(9 . 2) (let ((p (vector-ref (vector (cons 1 2)) 0))) (set-car! p 9) p))
(test-equal '(9 2) (let ((p (make-list 2 2))) (set-car! p 9) p))
(test-equal 9 (let ((p (cons 1 2)) (q (cons 1 2))) (set-car! p 9) (set-car! q 8) (car p)))
;; The pair constructed fresh on each call is a new, mutable one.
(define (fresh) (cons 1 2))
(test-equal '(9 . 2) (let ((p (fresh))) (set-car! p 9) p))
(test-equal '(1 . 2) (begin (set-car! (fresh) 9) (fresh)))
(test-assert (not (eq? (fresh) (fresh))))
;; Quasiquote makes fresh pairs from its unquotes; a mutated one leaves the
;; literal parts as they were.
(test-equal '(9 2) (let ((p `(,(+ 1 0) 2))) (set-car! p 9) p))
(test-equal '(1 2) (begin (let ((p `(,(+ 1 0) 2))) (set-car! p 9)) (lit)))
;; A fresh pair may hold a literal, and take another in its place.
(test-equal '(1 2) (let ((p (cons '(1 2) '()))) (car p)))
(test-equal '((7 8)) (let ((p (cons '(1 2) '()))) (set-car! p '(7 8)) p))
(test-error (let ((p (cons '(1 2) '()))) (set-car! (car p) 0)))
;; The reader returns fresh data.
(test-equal '(9 2)
            (let ((p (read (open-input-string "(1 2)")))) (set-car! p 9) p))

;; Reading, walking and comparing a literal still work.
(test-equal 1 (car (lit)))
(test-equal '(2) (cdr (lit)))
(test-equal 2 (length (lit)))
(test-equal '(2 1) (reverse (lit)))
(test-equal '(1 2 3) (append (lit) '(3)))
(test-equal '(1 2) (list-copy (lit)))
(test-equal 5 (length big))
(test-equal 2 (list-ref (lit) 1))
(test-equal '(2 3) (map (lambda (x) (+ x 1)) (lit)))
(test-assert (equal? (lit) (list 1 2)))
(test-assert (equal? (list 1 2) (lit)))
(test-assert (not (equal? (lit) (list 1 9))))
(test-assert (pair? (lit)))
(test-assert (list? (lit)))
(test-equal '(1 2 3) (let ((p (append (lit) (list 3)))) p))
(test-end)
