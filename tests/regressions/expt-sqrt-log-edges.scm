;;; requirement: expt, sqrt and log are right at the edges: zero exponents, negative bases, exact arguments beyond the flonum range, and exact results too large to build
;;; kind: scheme

(import (scheme base) (scheme inexact) (scheme complex) (srfi 64))

(define (near? x y) (< (abs (- x y)) (* 1e-12 (abs y))))

(test-begin "expt-sqrt-log-edges")

;; A zero exponent is 1, whatever the base, exact only if the exponent is.
(test-eqv 1.0 (expt 0.0 0.0))
(test-eqv 1.0 (expt 0 0.0))
(test-eqv 1.0 (expt 2.5 0.0))
(test-eqv 1 (expt 0 0))
(test-eqv 1.0 (expt 0.0 0))
(test-eqv 0 (expt 0 2))
(test-eqv 0.0 (expt 0 2.0))
(test-error (expt 0 -1))

;; A negative base to an integral inexact exponent is real.
(test-eqv -8.0 (expt -2.0 3.0))
(test-eqv 4.0 (expt -2 2.0))
(test-eqv -8.0 (expt -1/2 -3.0))
(test-assert (real? (expt -2.0 3.0)))
;; ... and complex to a fractional one.
(test-assert (not (real? (expt -2.0 0.5))))
(test-assert (near? 1.4142135623730951 (imag-part (expt -2.0 0.5))))

;; Exact results.
(test-eqv 2 (expt 4 1/2))
(test-eqv 1267650600228229401496703205376 (expt 2 100))
(test-eqv 1/8 (expt 2 -3))
(test-eqv 1 (expt 1 (expt 10 30)))
(test-eqv -1 (expt -1 (+ (expt 10 30) 1)))

;; An exact result too large to build is an error, not an exhausted heap.
(test-error (expt 2 (expt 10 10)))
(test-error (expt 1/2 (- (expt 10 10))))
(test-error (expt 1+i (expt 10 11)))

;; sqrt and log of exact arguments beyond the flonum range.
(define big (+ (expt 10 400) 1))
(test-assert (near? 1e200 (sqrt big)))
(test-assert (near? 921.0340371976183 (log big)))
(test-assert (near? 1e-200 (sqrt (/ 1 big))))
(test-assert (near? -921.0340371976183 (log (/ 1 big))))
(test-eqv (expt 10 200) (sqrt (expt 10 400)))
(test-assert (near? 921.0340371976183 (real-part (log (- big)))))
(test-assert (near? 3.141592653589793 (imag-part (log (- big)))))
(test-assert (near? 1e200 (imag-part (sqrt (- big)))))
(test-eqv +inf.0 (sqrt (+ (expt 10 700) 1)))

;; In-range arguments are unchanged.
(test-eqv 2.302585092994046 (log 10))
(test-eqv 1.4142135623730951 (sqrt 2))
(test-eqv 4 (sqrt 16))

(test-end "expt-sqrt-log-edges")
