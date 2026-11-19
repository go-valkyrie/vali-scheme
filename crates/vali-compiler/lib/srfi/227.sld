;;; (srfi 227): optional arguments. `(opt-lambda (v ... (b init) ... . r)
;;; body)` makes a procedure taking the required arguments `v` and, after
;;; them, optional ones `b`, which `init` supplies when they are missing.
;;; `opt-lambda` evaluates each `init` where the `opt-lambda` is, so it
;;; cannot see the procedure's own variables; `opt*-lambda` evaluates them
;;; in order, each seeing the variables before it. `r` takes the arguments
;;; left over; without it, there is an error for too many.
;;; `let-optionals` applies such a procedure to a list of arguments, and
;;; the `define-optionals` forms, which `(srfi 227 definitions)` also
;;; exports, define one.
;;;
;;; Written for Vali from the specification, with syntax-rules.

(define-library (srfi 227)
  (import (scheme base))
  (export opt-lambda opt*-lambda let-optionals let-optionals*
          define-optionals define-optionals*)
  (begin
    (define-syntax opt-lambda
      (syntax-rules ()
        ((_ formals body1 body2 ...)
         (%opt-req %opt-thunk () formals body1 body2 ...))))

    (define-syntax opt*-lambda
      (syntax-rules ()
        ((_ formals body1 body2 ...)
         (%opt-req %opt* () formals body1 body2 ...))))

    ;; Takes the required variables off the formals: next is the macro to
    ;; call with (required ...) (optional ...) rest body ...
    (define-syntax %opt-req
      (syntax-rules ()
        ((_ next (v ...) () body ...)
         (next (v ...) () () body ...))
        ((_ next (v ...) ((b i) . more) body ...)
         (%opt-collect next (v ...) () ((b i) . more) body ...))
        ((_ next (v ...) (x . more) body ...)
         (%opt-req next (v ... x) more body ...))
        ((_ next (v ...) r body ...)
         (next (v ...) () r body ...))))

    ;; Then the optional bindings, and what follows them: () or the rest.
    (define-syntax %opt-collect
      (syntax-rules ()
        ((_ next (v ...) (opt ...) ((b i) . more) body ...)
         (%opt-collect next (v ...) (opt ... (b i)) more body ...))
        ((_ next (v ...) (opt ...) () body ...)
         (next (v ...) (opt ...) () body ...))
        ((_ next (v ...) (opt ...) r body ...)
         (next (v ...) (opt ...) r body ...))))

    ;; For `opt-lambda`: each init becomes a procedure made outside the
    ;; lambda, then the sequential case takes over.
    (define-syntax %opt-thunk
      (syntax-rules ()
        ((_ vs (opt ...) r body ...)
         (%opt-thunks vs () (opt ...) r body ...))))

    (define-syntax %opt-thunks
      (syntax-rules ()
        ((_ vs (done ...) () r body ...)
         (%opt* vs (done ...) r body ...))
        ((_ vs (done ...) ((b i) more ...) r body ...)
         (let ((t (lambda () i)))
           (%opt-thunks vs (done ... (b (t))) (more ...) r body ...)))))

    (define-syntax %opt*
      (syntax-rules ()
        ((_ (v ...) (opt ...) r body ...)
         (lambda (v ... . args)
           (%opt-bind args (opt ...) r body ...)))))

    ;; Binds each optional variable to the next argument, or to its init
    ;; when there are none left, then the rest.
    (define-syntax %opt-bind
      (syntax-rules ()
        ((_ args () () body ...)
         (if (null? args)
             (let () body ...)
             (error "opt-lambda: too many arguments" args)))
        ((_ args () r body ...)
         (let ((r args)) body ...))
        ((_ args ((b i) more ...) r body ...)
         (let* ((b (if (pair? args) (car args) i))
                (args (if (pair? args) (cdr args) args)))
           (%opt-bind args (more ...) r body ...)))))

    (define-syntax let-optionals
      (syntax-rules ()
        ((_ expression opt-formals body1 body2 ...)
         (apply (opt-lambda opt-formals body1 body2 ...) expression))))

    (define-syntax let-optionals*
      (syntax-rules ()
        ((_ expression opt-formals body1 body2 ...)
         (apply (opt*-lambda opt-formals body1 body2 ...) expression))))

    (define-syntax define-optionals
      (syntax-rules ()
        ((_ (name . opt-formals) body1 body2 ...)
         (define name (opt-lambda opt-formals body1 body2 ...)))))

    (define-syntax define-optionals*
      (syntax-rules ()
        ((_ (name . opt-formals) body1 body2 ...)
         (define name (opt*-lambda opt-formals body1 body2 ...)))))))
