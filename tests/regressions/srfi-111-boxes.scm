;;; requirement: (srfi 111) boxes are a distinct type
;;; found-in: SRFI audit
;;; kind: scheme

(import (scheme base) (srfi 64) (srfi 111))

(test-begin "srfi-111")
(test-assert (memq 'srfi-111 (features)))
(test-assert (box? (box 1)))
(test-assert (not (box? 1)))
(test-assert (not (box? '(1))))
(test-assert (not (box? (vector 1))))
(test-equal 1 (unbox (box 1)))
(test-equal 2 (let ((b (box 1))) (set-box! b 2) (unbox b)))
(test-assert (not (eq? (box 1) (box 1))))
(test-assert (not (pair? (box 1))))
(test-end)
