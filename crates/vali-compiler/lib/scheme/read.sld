;;; (scheme read): reading a datum from a textual input port, the current
;;; input port by default (R7RS 6.13.2).

(define-library (scheme read)
  (import (scheme base) (only (vali primitive) %read))
  (export read)
  (begin
    (define (read . port)
      (%read (if (pair? port) (car port) (current-input-port))))))
