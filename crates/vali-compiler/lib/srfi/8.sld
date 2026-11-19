;;; (srfi 8): receive, binding the values of an expression to formals.

(define-library (srfi 8)
  (import (scheme base))
  (export receive)
  (begin
    (define-syntax receive
      (syntax-rules ()
        ((_ formals expression body1 body2 ...)
         (call-with-values (lambda () expression)
           (lambda formals body1 body2 ...)))))))
