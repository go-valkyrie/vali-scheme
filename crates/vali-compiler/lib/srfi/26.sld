;;; (srfi 26): cut and cute, notation for specialising parameters without
;;; currying. `<>` marks a slot and `<...>`, last, the rest arguments.
;;; `cut` evaluates every expression each time the procedure is called;
;;; `cute` evaluates them once, when it makes the procedure. The slot
;;; markers are matched as literals, so they need not be imported.

(define-library (srfi 26)
  (import (scheme base))
  (export cut cute)
  (begin
    (define-syntax cut
      (syntax-rules ()
        ((_ slot-or-expr ...) (%cut () () slot-or-expr ...))))

    ;; (%cut (parameter ...) (element ...) slot-or-expr ...)
    (define-syntax %cut
      (syntax-rules (<> <...>)
        ((_ (x ...) (a ...))
         (lambda (x ...) (a ...)))
        ((_ (x ...) (a ...) <...>)
         (lambda (x ... . rest) (apply a ... rest)))
        ((_ (x ...) (a ...) <> . more)
         (%cut (x ... tmp) (a ... tmp) . more))
        ((_ (x ...) (a ...) e . more)
         (%cut (x ...) (a ... e) . more))))

    (define-syntax cute
      (syntax-rules ()
        ((_ slot-or-expr ...) (%cute () () () slot-or-expr ...))))

    ;; (%cute (parameter ...) (element ...) (binding ...) slot-or-expr ...)
    (define-syntax %cute
      (syntax-rules (<> <...>)
        ((_ (x ...) (a ...) (b ...))
         (let (b ...) (lambda (x ...) (a ...))))
        ((_ (x ...) (a ...) (b ...) <...>)
         (let (b ...) (lambda (x ... . rest) (apply a ... rest))))
        ((_ (x ...) (a ...) (b ...) <> . more)
         (%cute (x ... tmp) (a ... tmp) (b ...) . more))
        ((_ (x ...) (a ...) (b ...) e . more)
         (%cute (x ...) (a ... tmp) (b ... (tmp e)) . more))))))
