;;; (srfi 197): pipeline operators. Each step is a list, in which the
;;; placeholder (`_`) stands for the previous step's value, and a
;;; placeholder followed by the ellipsis (`...`), last, for all the values
;;; no other placeholder took. As in the SRFI, a macro takes a custom
;;; placeholder, and `chain` a custom ellipsis, as identifiers after the
;;; first value (`chain`, `chain-and`, `chain-when`, `nest-reverse`) or
;;; before the steps (`chain-lambda`, `nest`).
;;;
;;; Written for Vali from the specification, with syntax-case.

(define-library (srfi 197)
  (import (scheme base) (vali pipeline))
  (export chain chain-and chain-when chain-lambda nest nest-reverse)
  (begin
    (define-syntax chain
      (lambda (stx)
        (syntax-case stx ()
          ((_ init . more)
           (let-values (((ids steps) (%leading (syntax more) 2)))
             (%chain (syntax init) (%elements steps)
                     (%placeholder? ids) (%ellipsis? ids)))))))

    ;; Each step is applied to the previous value in `t`, which is false
    ;; or stops the chain.
    (define-syntax chain-and
      (lambda (stx)
        (syntax-case stx ()
          ((_ init . more)
           (let-values (((ids steps) (%leading (syntax more) 2)))
             (let ((placeholder? (%placeholder? ids)))
               (let loop ((value (syntax init)) (steps (%elements steps)))
                 (if (null? steps)
                     value
                     (with-syntax ((t (%fresh (syntax init)))
                                   (v value))
                       (with-syntax ((next (loop (%fill (car steps) placeholder? (syntax t))
                                                 (cdr steps))))
                         (syntax (let ((t v)) (if t next #f)))))))))))))

    ;; Each step is `(guard step)`, skipped when the guard is false.
    (define-syntax chain-when
      (lambda (stx)
        (syntax-case stx ()
          ((_ init . more)
           (let-values (((ids steps) (%leading (syntax more) 2)))
             (let ((placeholder? (%placeholder? ids)))
               (let loop ((value (syntax init)) (steps (%elements steps)))
                 (if (null? steps)
                     value
                     (syntax-case (car steps) ()
                       ((guard step)
                        (with-syntax ((t (%fresh (syntax init)))
                                      (v value))
                          (with-syntax ((filled (%fill (syntax step) placeholder? (syntax t))))
                            (with-syntax ((next (loop (syntax (if guard filled t))
                                                      (cdr steps))))
                              (syntax (let ((t v)) next)))))))))))))))

    (define-syntax chain-lambda
      (lambda (stx)
        (syntax-case stx ()
          ((_ . more)
           (let-values (((ids steps) (%leading (syntax more) 2)))
             (let ((placeholder? (%placeholder? ids))
                   (ellipsis? (%ellipsis? ids))
                   (steps (%elements steps)))
               (let-values (((call formals) (%scan (car steps) placeholder? ellipsis?)))
                 (with-syntax ((f formals)
                               (body (%chain call (cdr steps) placeholder? ellipsis?)))
                   (syntax (lambda f body))))))))))

    ;; Steps nest outward: the last argument is the innermost.
    (define-syntax nest
      (lambda (stx)
        (syntax-case stx ()
          ((_ . more)
           (let-values (((ids args)
                         (let-values (((ids rest) (%leading (syntax more) 1)))
                           ;; A lone identifier is the value, not a
                           ;; placeholder.
                           (if (and (pair? ids) (null? (%elements rest)))
                               (values '() (syntax more))
                               (values ids rest)))))
             (let ((placeholder? (%placeholder? ids)))
               (let loop ((args (%elements args)))
                 (if (null? (cdr args))
                     (car args)
                     (%fill (car args) placeholder? (loop (cdr args)))))))))))

    ;; The same, with the value first and the steps in order.
    (define-syntax nest-reverse
      (lambda (stx)
        (syntax-case stx ()
          ((_ init . more)
           (let-values (((ids steps) (%leading (syntax more) 1)))
             (let ((placeholder? (%placeholder? ids)))
               (let loop ((value (syntax init)) (steps (%elements steps)))
                 (if (null? steps)
                     value
                     (loop (%fill (car steps) placeholder? value)
                           (cdr steps))))))))))))
