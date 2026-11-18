;;; requirement: mutating a quoted pair gives the same answer with and without the optimiser
;;; found-in: vali
;;; kind: scheme

;; The optimiser folded (car '(1 2)) to 1 even after set-car! changed the
;; literal. R7RS calls mutating a literal an error without requiring it to be
;; signalled; until literal pairs are immutable (#60) the answer must not
;; depend on the optimisation level. Literal vectors are immutable (#59), so
;; literal-vectors-immutable covers them. Run with VALI_NO_OPT=1 as well.

(import (scheme base) (srfi 64))

(test-begin "mutated-literals-same-at-every-opt-level")
(test-equal 9 (let ((p '(1 2))) (set-car! p 9) (car p)))
(test-equal '(2) (let ((p '(1 2))) (set-car! p 9) (cdr p)))
(test-equal '(1) (let ((p '(1 2))) (set-cdr! p '()) (cdr (cons 0 p))))
(test-equal 1 (let ((p '(1 2 3))) (set-cdr! p '()) (length p)))
(test-equal 9 (let ((p '(1 2))) (set-car! p 9) (car p) (car p)))
(test-equal 5 (let ((p '(1 . 2))) (set-cdr! p 5) (cdr p)))
;; Two equal literals are separate objects, so mutating one leaves the other.
(test-equal '(1 2)
            (let ((a '(1 2)) (b '(1 2))) (set-car! a 9) b))
;; One literal is one object, even across calls the optimiser inlines.
(define (lit) '(1 2))
(test-assert (eq? (lit) (lit)))
(test-equal 9 (begin (set-car! (lit) 9) (car (lit))))
(test-end)
