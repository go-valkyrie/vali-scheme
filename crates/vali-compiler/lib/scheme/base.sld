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
   and or when unless let* cond case do with-syntax
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
   apply error
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

    (define-syntax with-syntax
      (syntax-rules ()
        ((_ () e1 e2 ...)
         (let () e1 e2 ...))
        ((_ ((out in)) e1 e2 ...)
         (syntax-case in () (out (let () e1 e2 ...))))
        ((_ ((out in) ...) e1 e2 ...)
         (syntax-case (list in ...) () ((out ...) (let () e1 e2 ...))))))))
