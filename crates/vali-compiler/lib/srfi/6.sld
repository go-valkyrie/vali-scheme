;;; (srfi 6): basic string ports, which R7RS covers.

(define-library (srfi 6)
  (import (scheme base))
  (export open-input-string open-output-string get-output-string))
