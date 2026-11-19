;;; requirement: (srfi 141) integer division is available
;;; found-in: SRFI audit
;;; kind: scheme

(import (scheme base) (srfi 64) (srfi 141))

;; Each rule's quotient and remainder, for the sign combinations of 7/2.
(define-syntax check
  (syntax-rules ()
    ((_ op n d q r)
     (test-equal '(q r) (call-with-values (lambda () (op n d)) list)))))

(define (check-all n d fq fr cq cr tq tr rq rr eq er bq br)
  (test-equal (list fq fr) (call-with-values (lambda () (floor/ n d)) list))
  (test-equal (list cq cr) (call-with-values (lambda () (ceiling/ n d)) list))
  (test-equal (list tq tr) (call-with-values (lambda () (truncate/ n d)) list))
  (test-equal (list rq rr) (call-with-values (lambda () (round/ n d)) list))
  (test-equal (list eq er) (call-with-values (lambda () (euclidean/ n d)) list))
  (test-equal (list bq br) (call-with-values (lambda () (balanced/ n d)) list))
  (test-equal fq (floor-quotient n d))
  (test-equal fr (floor-remainder n d))
  (test-equal cq (ceiling-quotient n d))
  (test-equal cr (ceiling-remainder n d))
  (test-equal tq (truncate-quotient n d))
  (test-equal tr (truncate-remainder n d))
  (test-equal rq (round-quotient n d))
  (test-equal rr (round-remainder n d))
  (test-equal eq (euclidean-quotient n d))
  (test-equal er (euclidean-remainder n d))
  (test-equal bq (balanced-quotient n d))
  (test-equal br (balanced-remainder n d)))

(test-begin "srfi-141")
(test-assert (memq 'srfi-141 (features)))
;;         n   d  floor  ceiling truncate round  euclid balanced
(check-all  7   2  3  1   4 -1    3  1    4 -1    3  1    4 -1)
(check-all -7   2 -4  1  -3 -1   -3 -1   -4  1   -4  1   -3 -1)
(check-all  7  -2 -4 -1  -3  1   -3  1   -4 -1   -3  1   -4 -1)
(check-all -7  -2  3 -1   4  1    3 -1    4  1    4  1    3 -1)
(check-all  8   3  2  2   3 -1    2  2    3 -1    2  2    3 -1)
(check-all -8   3 -3  1  -2 -2   -2 -2   -3  1   -3  1   -3  1)
(check-all  6   3  2  0   2  0    2  0    2  0    2  0    2  0)
(check-all  5   2  2  1   3 -1    2  1    2  1    2  1    3 -1)
;; Inexact integers work too.
(test-equal 3. (floor-quotient 7. 2.))
(test-equal -1. (ceiling-remainder 7. 2.))
(test-end)
