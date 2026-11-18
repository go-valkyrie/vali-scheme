;;; requirement: a literal vector is immutable, and every vector mutator raises on one
;;; found-in: vali
;;; kind: scheme

;; R7RS calls mutating a literal constant an error. Vali signals it for
;; literal strings and bytevectors; this does the same for vectors (#59).
;; Quasiquote builds its vectors with list->vector, so `#(1 ,x) is fresh
;; and mutable, and so is `#(1 2) with no unquote. Run with VALI_NO_OPT=1
;; as well.

(import (scheme base) (scheme read) (srfi 64) (srfi 132))

(define (lit) '#(3 1 2))
(define big '#(5 4 3 2 1))

(test-begin "literal-vectors-immutable")

;; The VM's inline vector-set, through a variable and through the
;; procedure, which the optimiser cannot see through.
(test-error (vector-set! (lit) 0 9))
(test-error (let ((v (lit))) (vector-set! v 0 9)))
(test-error (let ((f vector-set!)) (f (lit) 0 9)))
(test-error (apply vector-set! (list (lit) 0 9)))
(test-error (vector-fill! (lit) 0))
(test-error (vector-fill! (lit) 0 1 2))
(test-error (vector-copy! (lit) 0 #(7 8)))
(test-error (vector-copy! (lit) 0 #(7 8) 0 1))
(test-error (vector-sort! (lit) <))
(test-error (vector-stable-sort! (lit) <))
(test-error (vector-sort! (lit) < 0 2))
(test-error (vector-merge! < (lit) #(1) #(2)))
(test-error (vector-delete-neighbor-dups! = (lit)))
;; Nested literal vectors are immutable too.
(test-error (vector-set! (vector-ref '#(1 #(2 3)) 1) 0 9))
(test-error (vector-set! (car '(#(1) 2)) 0 9))
(test-error (vector-set! '#(1 #(2 3)) 0 9))
;; An empty range still raises: the check comes before any store.
(test-error (vector-copy! (lit) 0 #()))
(test-error (vector-copy! (lit) 1 #(7 8) 1 1))
(test-error (vector-fill! (lit) 0 1 1))
(test-error (vector-sort! (lit) < 1 1))
(test-error (vector-stable-sort! (lit) < 0 0))
(test-error (vector-merge! < (lit) #() #()))
(test-error (vector-delete-neighbor-dups! = (lit) 2 2))
;; The empty vector too.
(test-error (vector-fill! '#() 0))
;; A failed write leaves the literal as it was.
(test-equal '#(3 1 2) (begin (guard (e (#t #f)) (vector-set! (lit) 0 9)) (lit)))
(test-equal '#(5 4 3 2 1)
            (begin (guard (e (#t #f)) (vector-sort! big <)) big))
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
(test-assert (contains? (message-of (vector-set! (lit) 0 9)) "immutable"))
(test-assert (contains? (message-of (vector-set! (lit) 0 9)) "vector-set!"))
(test-assert (contains? (message-of (vector-fill! (lit) 0)) "vector-fill!"))
(test-assert (contains? (message-of (vector-copy! (lit) 0 #())) "vector-copy!"))
(test-assert (contains? (message-of (vector-merge! < (lit) #() #())) "vector-merge!"))
(test-assert (contains? (message-of (vector-delete-neighbor-dups! = (lit))) "vector-delete-neighbor-dups!"))
(test-assert (contains? (message-of (vector-stable-sort! (lit) <)) "vector-stable-sort!"))

;; An out-of-range index is reported before immutability, however the
;; write is made.
(test-assert (contains? (message-of (vector-set! (lit) 5 9)) "out of range"))
(test-assert (not (contains? (message-of (vector-set! (lit) 5 9)) "immutable")))
(test-assert (contains? (message-of (apply vector-set! (list (lit) 5 9))) "out of range"))
(test-assert (contains? (message-of (let ((f vector-set!)) (f (lit) -1 9))) "out of range"))
(test-assert (contains? (message-of (let ((v (lit)) (i 5)) (vector-set! v i 9))) "out of range"))
(test-assert (contains? (message-of (vector-copy! (lit) 2 #(1 2 3))) "bad range"))

;; Fresh vectors stay mutable.
(test-equal '#(9 1 2) (let ((v (make-vector 3 1))) (vector-set! v 0 9) (vector-set! v 2 2) v))
(test-equal '#(9 9 9) (let ((v (vector 1 2 3))) (vector-fill! v 9) v))
(test-equal '#(9 2 3) (let ((v (list->vector '(1 2 3)))) (vector-set! v 0 9) v))
(test-equal '#(9 1 2) (let ((v (vector-copy (lit)))) (vector-set! v 0 9) (vector-set! v 1 1) (vector-set! v 2 2) v))
(test-equal '#(1 2 3) (let ((v (vector-copy '#(3 1 2)))) (vector-sort! v <) v))
(test-equal '#(3 1 2 9) (let ((v (vector-append (lit) '#(0)))) (vector-set! v 3 9) v))
(test-equal '#(7 7) (let ((v (vector-map (lambda (x) x) '#(1 2)))) (vector-fill! v 7) v))
(test-equal '#(1 2 3) (let ((v (vector 1 2 3))) (vector-copy! v 0 #()) (vector-fill! v 0 1 1) v))
(test-equal '#(7 8 3) (let ((v (vector-copy '#(1 2 3)))) (vector-copy! v 0 #(7 8)) v))
;; Reading, converting and comparing a literal still work.
(test-equal '(3 1 2) (vector->list (lit)))
(test-equal '#(1 2) (vector-copy '#(0 1 2) 1))
(test-assert (equal? (lit) (vector 3 1 2)))
(test-assert (equal? (vector 3 1 2) (lit)))
(test-assert (not (equal? (lit) (vector 3 1 9))))
(test-equal 3 (vector-ref (lit) 0))
(test-equal 3 (vector-length (lit)))

;; Quasiquote builds fresh vectors, mutable even without an unquote.
(test-equal '#(9 2) (let ((v `#(1 2))) (vector-set! v 0 9) v))
(test-equal '#(9 5) (let ((v `#(1 ,(+ 2 3)))) (vector-set! v 0 9) v))
(test-equal '#(9 1 2) (let ((v `#(,@'(1 1 2)))) (vector-set! v 0 9) v))
(test-assert (not (eq? `#(1 2) `#(1 2))))

;; The reader returns fresh data.
(test-equal '#(9 2)
            (let ((v (read (open-input-string "#(1 2)")))) (vector-set! v 0 9) v))
(test-end)
