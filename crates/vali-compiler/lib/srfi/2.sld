;;; (srfi 2): and-let*, an `and` whose clauses may also bind variables.
;;; Each clause is `(variable expression)`, `(expression)` or a variable;
;;; the first false value ends the form, and the body's value (or the last
;;; clause's, with no body) is its value.

(define-library (srfi 2)
  (import (scheme base))
  (export and-let*)
  (begin
    (define-syntax and-let*
      (syntax-rules ()
        ((_ ()) #t)
        ((_ () body ...) (let () body ...))
        ;; The last clause, with no body, gives its own value.
        ((_ ((var expr))) (let ((var expr)) var))
        ((_ ((expr))) expr)
        ((_ (var)) var)
        ((_ ((var expr) . rest) . body)
         (let ((var expr))
           (if var (and-let* rest . body) #f)))
        ((_ ((expr) . rest) . body)
         (if expr (and-let* rest . body) #f))
        ((_ (var . rest) . body)
         (if var (and-let* rest . body) #f))))))
