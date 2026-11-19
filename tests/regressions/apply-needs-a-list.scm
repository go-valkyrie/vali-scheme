;;; requirement: apply with no list argument raises, and with an empty list still works
;;; found-in: R7RS-small audit
;;; kind: scheme

;; (apply f) used to call f with no arguments, so (apply +) returned 0.
;; R7RS 6.10 gives apply a procedure and a list at least, so the call is
;; an error, however it is reached.

(import (scheme base) (srfi 64))

(test-begin "apply-needs-a-list")

(test-error (apply +))
(test-error (apply list))
(test-error (let ((f apply)) (f +)))
(test-error (apply))

(test-equal 0 (apply + '()))
(test-equal '() (apply list '()))
(test-equal 6 (apply + 1 2 '(3)))
(test-equal 3 (apply + '(1 2)))

(test-end "apply-needs-a-list")
