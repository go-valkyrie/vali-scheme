;;; (scheme inexact): the transcendental functions and the tests on
;;; inexact numbers (R7RS 6.2.6).

(define-library (scheme inexact)
  (import (vali primitive))
  (export acos asin atan cos exp finite? infinite? log nan? sin sqrt tan))
