;;; requirement: A letrec variable whose value is not a lambda can be captured by a closure made inside that value
;;; found-in: review
;;; kind: scheme

;; A closure made inside a letrec binding's value captured the variable
;; while it still held its placeholder, and nothing patched it, so calling
;; it failed with "not a procedure".

(import (scheme base) (srfi 64))

(define (f)
  (define g (let ((z 1)) (lambda (n) (if (= n 0) z (g (- n 1))))))
  (g 5))

(define (h)
  (letrec* ((a (let ((k 2)) (lambda (n) (if (= n 0) k (b (- n 1))))))
            (b (let ((k 3)) (lambda (n) (if (= n 0) k (a (- n 1)))))))
    (list (a 4) (a 3))))

(test-begin "letrec-value-captures-itself")
(test-equal 1 (f))
(test-equal '(2 3) (h))
(test-end)
