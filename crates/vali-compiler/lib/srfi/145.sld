;;; (srfi 145): assume, a statement that an expression is true. A false
;;; one is an error; Vali always checks, and signals it with `error`.

(define-library (srfi 145)
  (import (scheme base))
  (export assume)
  (begin
    (define-syntax assume
      (syntax-rules ()
        ((_ expression message ...)
         (or expression
             (error "invalid assumption" 'expression message ...)))))))
