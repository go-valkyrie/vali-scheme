;;; requirement: The reader accepts the R5RS exponent markers s, f, d and l as e
;;; found-in: R7RS-small audit
;;; kind: scheme

;; 1s2, 1f2, 1d2 and 1l2 (in either case) read as 100.0, as 1e2 does. In
;; hexadecimal d and f are digits, and what is not a number stays a symbol.

(import (scheme base) (scheme complex) (scheme read) (scheme write) (srfi 64))

(test-begin "r5rs-exponent-markers")

(test-equal 100.0 1s2)
(test-equal 100.0 1S2)
(test-equal 100.0 1f2)
(test-equal 100.0 1F2)
(test-equal 100.0 1d2)
(test-equal 100.0 1D2)
(test-equal 100.0 1l2)
(test-equal 100.0 1L2)
(test-equal 0.15 1.5d-1)
(test-equal 5.0 .5f+1)
(test-equal 100 #e1l2)
(test-equal 100.0 #d1s2)
(test-equal (make-rectangular 100.0 1.0) 1s2+1.0i)
(test-equal (make-rectangular 1.0 100.0) 1.0+1s2i)

;; Hexadecimal digits, not exponents.
(test-equal 466 #x1d2)
(test-equal 498 #x1f2)

;; Not numbers, so still symbols.
(define (read-string-datum s) (read (open-input-string s)))
(test-assert (symbol? (read-string-datum "s2")))
(test-assert (symbol? (read-string-datum "d1")))
(test-assert (symbol? (read-string-datum "f2")))
(test-assert (symbol? (read-string-datum "l1")))
(test-assert (symbol? (read-string-datum "+s2")))
(test-assert (symbol? (read-string-datum "-d1")))

(test-end "r5rs-exponent-markers")
