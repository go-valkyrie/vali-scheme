;;; requirement: R7RS complex literals with an exact zero imaginary part
;;; found-in: R7RS conformance suite
;;; kind: scheme

;; R7RS 6.2.6: a complex number whose imaginary part is an exact zero is
;; real, whatever the exactness of its real part, so -2.5+0i reads as the
;; real -2.5. An inexact zero imaginary part keeps the number complex.

(import (scheme base) (scheme complex) (srfi 64))

(test-begin "complex-literal-exact-zero-imaginary-is-real")
(test-assert (real? -2.5+0i))
(test-eqv -2.5 -2.5+0i)
(test-eqv 1 1+0i)
(test-eqv 1.5 1.5+0i)
(test-eqv 1.0 #i1+0i)
(test-eqv 1/2 1/2-0i)
(test-assert (exact? (imag-part -2.5+0i)))
(test-eqv 0 (imag-part -2.5+0i))
(test-assert (not (real? -2.5+0.0i)))
(test-assert (inexact? (imag-part -2.5+0.0i)))
(test-eqv -2.5 (make-rectangular -2.5 0))
(test-assert (real? (make-rectangular -2.5 0)))
(test-assert (not (real? (make-rectangular -2.5 0.0))))
(test-end)
