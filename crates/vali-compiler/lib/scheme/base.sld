;;; (scheme base): R7RS's base library, as much of it as Vali has so far.
;;;
;;; The core forms come from (vali core) and the procedures implemented in
;;; Rust from (vali primitive); the derived forms are macros defined here.
;;; Vali also exports syntax-case and its companions from here
;;; (ADR 0007 #10).

(define-library (scheme base)
  (import (vali core) (vali primitive))
  (export
   ;; Core forms.
   quote quote-syntax if begin lambda let letrec letrec* set! define
   define-syntax let-syntax letrec-syntax syntax-rules syntax-case syntax
   ;; Derived forms, below.
   and or when unless let* cond case do with-syntax quasiquote
   define-record-type
   ;; Procedures.
   + * - = < <= > >= quotient remainder modulo abs min max
   number? integer? exact? inexact? zero? positive? negative? odd? even?
   cons car cdr set-car! set-cdr! list length reverse append list-tail
   list-ref memq memv member assq assv assoc null? pair? list?
   not eq? eqv? equal? boolean? procedure? symbol? symbol=?
   symbol->string string->symbol
   vector make-vector vector-ref vector-set! vector-length vector?
   vector->list list->vector vector-fill!
   string? string-length string-ref string=? string<? string-append
   substring string->list list->string
   char? char->integer integer->char char=? char<?
   apply error (rename %raise raise) raise-continuable with-exception-handler
   guard error-object? error-object-message error-object-irritants
   values call-with-values
   ;; The syntax procedures, for transformers.
   syntax->datum datum->syntax identifier? bound-identifier=?
   free-identifier=? generate-temporaries syntax-violation)
  (begin
    (define-syntax and
      (syntax-rules ()
        ((_) #t)
        ((_ test) test)
        ((_ test1 test2 ...) (if test1 (and test2 ...) #f))))

    (define-syntax or
      (syntax-rules ()
        ((_) #f)
        ((_ test) test)
        ((_ test1 test2 ...)
         (let ((x test1))
           (if x x (or test2 ...))))))

    (define-syntax when
      (syntax-rules ()
        ((_ test result1 result2 ...)
         (if test (begin result1 result2 ...)))))

    (define-syntax unless
      (syntax-rules ()
        ((_ test result1 result2 ...)
         (if test (if #f #f) (begin result1 result2 ...)))))

    (define-syntax let*
      (syntax-rules ()
        ((_ () body1 body2 ...)
         (let () body1 body2 ...))
        ((_ ((name1 val1) (name2 val2) ...) body1 body2 ...)
         (let ((name1 val1))
           (let* ((name2 val2) ...) body1 body2 ...)))))

    (define-syntax cond
      (syntax-rules (else =>)
        ((_ (else result1 result2 ...))
         (begin result1 result2 ...))
        ((_ (test => result))
         (let ((temp test))
           (if temp (result temp))))
        ((_ (test => result) clause1 clause2 ...)
         (let ((temp test))
           (if temp
               (result temp)
               (cond clause1 clause2 ...))))
        ((_ (test)) test)
        ((_ (test) clause1 clause2 ...)
         (let ((temp test))
           (if temp temp (cond clause1 clause2 ...))))
        ((_ (test result1 result2 ...))
         (if test (begin result1 result2 ...)))
        ((_ (test result1 result2 ...) clause1 clause2 ...)
         (if test
             (begin result1 result2 ...)
             (cond clause1 clause2 ...)))))

    (define-syntax case
      (syntax-rules (else =>)
        ((_ (key ...) clauses ...)
         (let ((atom-key (key ...)))
           (case atom-key clauses ...)))
        ((_ key (else => result))
         (result key))
        ((_ key (else result1 result2 ...))
         (begin result1 result2 ...))
        ((_ key ((atoms ...) => result))
         (if (memv key '(atoms ...))
             (result key)))
        ((_ key ((atoms ...) => result) clause clauses ...)
         (if (memv key '(atoms ...))
             (result key)
             (case key clause clauses ...)))
        ((_ key ((atoms ...) result1 result2 ...))
         (if (memv key '(atoms ...))
             (begin result1 result2 ...)))
        ((_ key ((atoms ...) result1 result2 ...) clause clauses ...)
         (if (memv key '(atoms ...))
             (begin result1 result2 ...)
             (case key clause clauses ...)))))

    (define-syntax do
      (syntax-rules ()
        ((_ ((var init step ...) ...) (test expr ...) command ...)
         (let loop ((var init) ...)
           (if test
               (begin (if #f #f) expr ...)
               (begin
                 command ...
                 (loop (do "step" var step ...) ...)))))
        ((_ "step" x) x)
        ((_ "step" x y) y)))

    ;;; From R6RS's (rnrs syntax-case): with-syntax binds pattern variables to
    ;;; what each input matches, for a transformer's templates.

    ;; quasiquote (R7RS 4.2.8). qq carries the nesting depth as a list,
    ;; one element per enclosing quasiquote, so only the outermost level's
    ;; unquotes are evaluated.
    (define-syntax quasiquote
      (syntax-rules ()
        ((_ template) (qq template ()))))

    (define-syntax qq
      (syntax-rules (unquote unquote-splicing quasiquote)
        ((_ (unquote x) ()) x)
        ((_ (unquote x) (d . ds)) (list 'unquote (qq x ds)))
        ((_ ((unquote-splicing x) . rest) ())
         (append x (qq rest ())))
        ((_ ((unquote-splicing x) . rest) (d . ds))
         (cons (list 'unquote-splicing (qq x ds)) (qq rest (d . ds))))
        ((_ (quasiquote x) ds) (list 'quasiquote (qq x (#t . ds))))
        ((_ (a . b) ds) (cons (qq a ds) (qq b ds)))
        ((_ #(x ...) ds) (list->vector (qq (x ...) ds)))
        ((_ x ds) 'x)))

    ;; define-record-type (R7RS 5.5). Each field's index is looked up once,
    ;; when the type is defined, so accessors don't search.
    (define-syntax define-record-type
      (syntax-rules ()
        ((_ type (constructor field ...) predicate (fname . accessors) ...)
         (begin
           (define type (%make-record-type 'type '(fname ...)))
           (define constructor
             (let ((indices (%record-constructor-indices type '(field ...))))
               (lambda (field ...) (%make-record type indices field ...))))
           (define (predicate obj) (%record? obj type))
           (record-field type fname . accessors) ...))))

    (define-syntax record-field
      (syntax-rules ()
        ((_ type fname accessor)
         (define accessor
           (let ((i (%record-field-index type 'fname)))
             (lambda (obj) (%record-ref obj type i)))))
        ((_ type fname accessor modifier)
         (begin
           (record-field type fname accessor)
           (define modifier
             (let ((i (%record-field-index type 'fname)))
               (lambda (obj value) (%record-set! obj type i value))))))))

    ;; Exceptions (R7RS 6.11), over the VM's one escape, (%catch thunk
    ;; handler): a raise inside the thunk unwinds to it and calls the
    ;; handler with what was raised. %handlers is the stack of handlers
    ;; raise-continuable calls; each guard pushes one that raises again, so
    ;; it reaches the guard's own %catch.
    (define %handlers '())

    (define (with-exception-handler handler thunk)
      (let ((outer %handlers))
        (set! %handlers (cons handler outer))
        (let ((result
               (%catch thunk
                       (lambda (condition)
                         (set! %handlers outer)
                         (handler condition)
                         (error "an exception handler returned from a non-continuable exception"
                                condition)))))
          (set! %handlers outer)
          result)))

    (define (raise-continuable obj)
      (if (null? %handlers)
          (%raise obj)
          (let ((outer %handlers))
            (set! %handlers (cdr outer))
            (let ((result ((car outer) obj)))
              (set! %handlers outer)
              result))))

    ;; guard's work is a procedure here, since only this library can assign
    ;; %handlers; the macro wraps the body and the clauses in lambdas.
    (define (%guard body clauses)
      (let ((outer %handlers))
        (set! %handlers (cons %raise outer))
        (let ((result
               (%catch body
                       (lambda (condition)
                         (set! %handlers outer)
                         (clauses condition)))))
          (set! %handlers outer)
          result)))

    (define-syntax guard
      (syntax-rules ()
        ((_ (var clause ...) body1 body2 ...)
         (%guard (lambda () (let () body1 body2 ...))
                 (lambda (condition)
                   (let ((var condition))
                     (guard-clauses condition clause ...)))))))

    (define-syntax guard-clauses
      (syntax-rules (else =>)
        ((_ c) (%raise c))
        ((_ c (else e1 e2 ...)) (begin e1 e2 ...))
        ((_ c (test => receiver) clause ...)
         (let ((t test)) (if t (receiver t) (guard-clauses c clause ...))))
        ((_ c (test) clause ...)
         (let ((t test)) (if t t (guard-clauses c clause ...))))
        ((_ c (test e1 e2 ...) clause ...)
         (if test (begin e1 e2 ...) (guard-clauses c clause ...)))))

    ;; Multiple values are an object for now; call-with-values spreads it.
    (define (call-with-values producer consumer)
      (apply consumer (%values->list (producer))))

    (define-syntax with-syntax
      (syntax-rules ()
        ((_ () e1 e2 ...)
         (let () e1 e2 ...))
        ((_ ((out in)) e1 e2 ...)
         (syntax-case in () (out (let () e1 e2 ...))))
        ((_ ((out in) ...) e1 e2 ...)
         (syntax-case (list in ...) () ((out ...) (let () e1 e2 ...))))))))
