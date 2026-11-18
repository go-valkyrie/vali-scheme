;;; requirement: Numeric literals and their printed forms handle R7RS's corner cases, and -0.0 stays distinct from 0.0
;;; kind: scheme

;; Number syntax at its edges: prefixes, exponents past the flonum range,
;; denormals, exact decimals, infinities and NaNs, complex forms, and the
;; strings that look like numbers but are not. -0.0 must survive reading,
;; constants and negation, and eqv? must tell it from 0.0.

(import (scheme base) (scheme read) (scheme inexact) (srfi 64))

(define (read-number s) (read (open-input-string s)))

(test-begin "numeric-corner-cases")

;; Signed zero.
(test-assert (not (eqv? -0.0 0.0)))
(test-assert (not (equal? -0.0 0.0)))
(test-assert (= -0.0 0.0))
(test-equal "-0.0" (number->string -0.0))
(test-equal "-0.0" (number->string (- 0.0)))
(test-equal "0.0" (number->string (- -0.0)))
(test-equal "0.0" (number->string (- 0 0.0)))
(test-equal "-0.0" (number->string (* -1 0.0)))
(define (negate x) (- x))
(test-equal "-0.0" (number->string (negate 0.0)))
(test-eqv -5 (negate 5))
(test-eqv 0 (negate 0))
(test-eqv (- (expt 2 62)) (negate (expt 2 62)))
(test-eqv (expt 2 62) (negate (- (expt 2 62))))
(test-equal '(-0.0 0.0) (list -0.0 0.0))
(test-assert (not (eqv? (car '(-0.0)) (car '(0.0)))))
(test-eqv 0 (exact -0.0))
(test-eqv 0 (string->number "#e-0.0"))

;; Prefixes, in either order, but each at most once.
(test-eqv -255 (string->number "#x-ff"))
(test-eqv 26 (read-number "#X1A"))
(test-eqv 16 (string->number "#e#x10"))
(test-eqv 16 (string->number "#x#e10"))
(test-eqv 5 (string->number "#b101"))
(test-eqv 511 (string->number "#o777"))
(test-eqv 10/11 (string->number "#xA/B"))
(test-eqv #f (string->number "#e#e10"))
(test-eqv #f (string->number "#d#x1"))
(test-eqv #f (string->number "#b2"))
(test-eqv #f (string->number "#b1.1"))
(test-eqv #f (string->number "-#x1"))
(test-error (read-number "#i#e1"))

;; A radix argument, and a prefix overriding it.
(test-eqv 5 (string->number "101" 2))
(test-eqv 482 (string->number "1e2" 16))
(test-eqv 100.0 (string->number "#d1e2" 16))

;; Exact decimals and exponents.
(test-eqv 3/2 (string->number "#e1.5"))
(test-eqv 1/2 (string->number "#e.5"))
(test-eqv 3/2000 (string->number "#e1.5e-3"))
(test-eqv (expt 10 400) (string->number "#e1e400"))
(test-eqv 12345678901234567890123/10000000000000000000000
          (string->number "#e1.2345678901234567890123"))

;; Flonums past the edges of the range round to infinity or zero, and
;; denormals round to nearest.
(test-eqv +inf.0 (string->number "1e400"))
(test-eqv -inf.0 (string->number "-1e400"))
(test-eqv +inf.0 (string->number "1.8e308"))
(test-eqv 1e308 (string->number "1e308"))
(test-eqv 0.0 (string->number "1e-400"))
(test-eqv 5e-324 (string->number "4.9e-324"))
(test-eqv 5e-324 (string->number "2.4703282292062328e-324"))
(test-eqv 0.0 (string->number "2.4703282292062327e-324"))
(test-eqv 9007199254740992.0 (string->number "9007199254740993.0"))
(test-eqv 9007199254740993 (string->number "9007199254740993"))
(test-eqv 0.3333333333333333 (string->number "#i1/3"))

;; Infinities and NaNs.
(test-eqv +inf.0 (read-number "+INF.0"))
(test-assert (nan? (string->number "-nan.0")))
(test-assert (nan? (string->number "+nan.0")))
(test-eqv #f (string->number "inf.0"))
(test-eqv #f (string->number "+inf.1"))
(test-eqv #f (string->number "#e+inf.0"))
(test-eqv #f (string->number "#e+nan.0"))
(test-error (exact +inf.0))
(test-error (exact +nan.0))

;; Fractions and decimals.
(test-eqv #f (string->number "1/0"))
(test-eqv 0 (string->number "0/5"))
(test-eqv #f (string->number "1/2/3"))
(test-eqv #f (string->number "1/2e2"))
(test-eqv 1.0 (string->number "1."))
(test-eqv 0.5 (string->number ".5"))
(test-eqv -5.0 (string->number "-.5e1"))
(test-eqv 150.0 (string->number "1.5e+2"))

;; Complex numbers.
(test-equal "0+1i" (number->string (string->number "+i")))
(test-equal "0-1i" (number->string (string->number "-i")))
(test-equal "1+2i" (number->string (string->number "1+2i")))
(test-eqv 1 (string->number "1-0i"))
(test-eqv 1 (string->number "1@0"))
(test-eqv #f (string->number "i"))

;; Strings that are not numbers.
(for-each
 (lambda (s) (test-eqv s #f (string->number s)))
 '("" "." "+" "-" "..." "1e" "e1" "1e2.5" " 1" "1 " "1#" "1_000"))
(test-error (read-number "1+"))
(test-error (read-number "1a"))

;; Printing reads back the same number.
(for-each
 (lambda (x) (test-eqv x (string->number (number->string x))))
 (list 0.1 1e21 1e22 1e-7 5e-324 1.7976931348623157e308 123.456
       (expt 2 100) (- (expt 2 62)) -1/3 1.5e300))
(test-equal "1e21" (number->string 1e21))
(test-equal "100.0" (number->string 100.0))
(test-equal "ff" (number->string 255 16))
(test-equal "-ff" (number->string -255 16))
(test-equal "1/11" (number->string 1/3 2))
(test-equal "+nan.0" (number->string +nan.0))
(test-equal "-inf.0" (number->string -inf.0))

;; Fixnum boundaries overflow into bignums.
(test-eqv 4611686018427387904 (+ 4611686018427387903 1))
(test-eqv -4611686018427387905 (- -4611686018427387904 1))
(test-eqv 9223372036854775808 (* 4611686018427387904 2))
(test-eqv 4611686018427387904 (abs (- (expt 2 62))))

(test-end)
