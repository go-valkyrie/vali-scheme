;;; requirement: (srfi 8) receive is available
;;; found-in: SRFI audit
;;; kind: scheme

(import (scheme base) (srfi 64) (srfi 8))

(test-begin "srfi-8")
(test-assert (memq 'srfi-8 (features)))
(test-equal '(1 2) (receive (a b) (values 1 2) (list a b)))
(test-equal '(1 (2 3)) (receive (a . rest) (values 1 2 3) (list a rest)))
(test-equal '(1 2 3) (receive all (values 1 2 3) all))
(test-equal 5 (receive (q r) (floor/ 17 3) (- q (- r 2) 1 -1)))
(test-end)
