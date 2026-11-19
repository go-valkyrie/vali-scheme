;;; requirement: (srfi 2) and-let* is available
;;; found-in: SRFI audit
;;; kind: scheme

(import (scheme base) (srfi 64) (srfi 2))

(test-begin "srfi-2")
(test-assert (memq 'srfi-2 (features)))
(test-equal 1 (and-let* () 1))
(test-equal #t (and-let* ()))
(test-equal 6 (let ((x 5)) (and-let* ((x (+ x 1))) x)))
(test-equal #f (and-let* ((x #f) (y (car x))) y))
(test-equal 3 (and-let* ((x 1) (y (+ x 1)) ((> y x))) (+ x y)))
(test-equal #f (and-let* ((x 1) ((< x 0))) 'no))
(test-equal 7 (let ((x 7)) (and-let* (x))))
(test-equal 5 (and-let* ((x 5))))
(test-equal 'ok (let ((l '(1))) (and-let* (((pair? l)) (h (car l))) 'ok)))
(test-equal #f (let ((l '())) (and-let* (((pair? l)) (h (car l))) 'ok)))
(test-end)
