;;; (scheme case-lambda): procedures that choose a body by how many
;;; arguments they get (R7RS 4.2.9).

(define-library (scheme case-lambda)
  (import (scheme base))
  (export case-lambda)
  (begin
    (define-syntax case-lambda
      (syntax-rules ()
        ((_ (formals body0 body1 ...) ...)
         (lambda args
           (let ((n (length args)))
             (case-lambda-clauses args n (formals body0 body1 ...) ...))))))

    ;; The first clause whose formals accept n arguments; a proper list of
    ;; formals is tried before a dotted one, as written.
    (define-syntax case-lambda-clauses
      (syntax-rules ()
        ((_ args n)
         (error "case-lambda: no clause accepts this many arguments" n))
        ((_ args n ((x ...) body ...) clause ...)
         (if (= n (length '(x ...)))
             (apply (lambda (x ...) body ...) args)
             (case-lambda-clauses args n clause ...)))
        ((_ args n ((x ... . rest) body ...) clause ...)
         (if (>= n (length '(x ...)))
             (apply (lambda (x ... . rest) body ...) args)
             (case-lambda-clauses args n clause ...)))
        ((_ args n (rest body ...) clause ...)
         (apply (lambda rest body ...) args))))))
