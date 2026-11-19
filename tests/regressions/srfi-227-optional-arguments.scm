;;; requirement: (srfi 227) optional arguments are available
;;; found-in: SRFI audit
;;; kind: scheme

(import (scheme base) (srfi 64) (srfi 227) (srfi 227 definitions))

(define f (opt-lambda (a b (c 1) (d 2) . r) (list a b c d r)))

(define-optionals* (f2 x (y (* x x)) . z) (list x y z))
(define-optionals (g x (y 10)) (+ x y))

(test-begin "srfi-227")
(test-assert (memq 'srfi-227 (features)))
(test-equal '(1 2 1 2 ()) (f 1 2))
(test-equal '(1 2 3 2 ()) (f 1 2 3))
(test-equal '(1 2 3 4 (5)) (f 1 2 3 4 5))
(test-equal '(1 2 3 4 (5 6)) (f 1 2 3 4 5 6))
(test-equal 'raised (guard (e (#t 'raised)) (f 1)))
(test-equal '(3 9 ()) (f2 3))
(test-equal '(3 4 (5 6)) (f2 3 4 5 6))
(test-equal 11 (g 1))
(test-equal 3 (g 1 2))
(test-equal 'raised (guard (e (#t 'raised)) (g 1 2 3)))
(test-equal 'raised
  (guard (e (#t 'raised)) ((opt-lambda (a (b 1)) a) 1 2 3)))

;; let-optionals
(test-equal '(1 (2)) (let-optionals '(1 2) (x . y) (list x y)))
(test-equal '(1 3 4)
  (let-optionals* '(1 3) (x (y 2) (z (+ x y))) (list x y z)))
(test-equal '(1 2 3)
  (let-optionals* '(1) (x (y 2) (z (+ x y))) (list x y z)))
(test-equal '(1 2)
  (let-optionals '(1) (x (y 2)) (list x y)))

;; opt*-lambda inits see the earlier variables; opt-lambda inits do not,
;; and are evaluated at the call, in the lexical environment of the form.
(test-equal '(2 4) ((opt*-lambda (a (b (* a 2))) (list a b)) 2))
(test-equal '(2 10)
  (let ((a 10)) ((opt-lambda (a (b a)) (list a b)) 2)))
(test-equal '(1 20)
  (let ((x 10))
    (let ((p (opt-lambda ((a 1) (b x)) (list a b))))
      (set! x 20)
      (p 1 20))))
(test-equal '(1 20)
  (let ((x 10))
    (let ((p (opt-lambda ((a 1) (b x)) (list a b))))
      (set! x 20)
      (p))))
(test-end)
