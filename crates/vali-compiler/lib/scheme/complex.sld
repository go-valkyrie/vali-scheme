;;; (scheme complex): building complex numbers and taking them apart
;;; (R7RS 6.2.6).

(define-library (scheme complex)
  (import (vali primitive))
  (export angle imag-part magnitude make-polar make-rectangular real-part))
