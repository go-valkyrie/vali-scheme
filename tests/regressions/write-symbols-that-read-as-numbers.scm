;;; requirement: R7RS write round trip for symbols
;;; found-in: R7RS conformance suite
;;; kind: scheme

;; R7RS 6.13.3: `write` writes a symbol so that `read` returns it. A name
;; the reader takes for a number (+i, -inf.0, 1+2i, #x10, .5, 1/2) needs
;; bars; +, - and ... are symbols and need none. `display` never adds bars.

(import (scheme base) (scheme write) (scheme read) (srfi 64))

(define (written x)
  (let ((port (open-output-string)))
    (write x port)
    (get-output-string port)))

(define (displayed x)
  (let ((port (open-output-string)))
    (display x port)
    (get-output-string port)))

(define barred
  '(|+i| |-i| |+inf.0| |-inf.0| |+nan.0| |-nan.0| |1+2i| |#x10| |.5|
    |1/2| |1e3| |+5| |-1.5e2i| |1@2|))

(test-begin "write-symbols-that-read-as-numbers")
(test-equal "|+i|" (written '|+i|))
(test-equal "|-inf.0|" (written '|-inf.0|))
(test-equal "|+nan.0|" (written '|+nan.0|))
(test-equal "|1+2i|" (written '|1+2i|))
(test-equal "+i" (displayed '|+i|))
(test-equal "+inf.0" (displayed '|+inf.0|))
(for-each
  (lambda (s)
    (test-assert (symbol? s))
    (test-eq s (read (open-input-string (written s)))))
  barred)
(for-each
  (lambda (s) (test-equal (symbol->string s) (written s)))
  '(+ - ... a->b +a -a .a x1))
(test-end)
