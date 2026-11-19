;;; (scheme r5rs): the R5RS names (R7RS appendix A), almost all of them
;;; re-exported from the libraries that define them. What is new here is
;;; exact->inexact and inexact->exact, the old names of inexact and exact,
;;; and the two environments R5RS evaluates in, which R7RS keeps for old
;;; programs: scheme-report-environment, with every name this library
;;; exports, and null-environment, with only the syntax.
;;;
;;; Files are not ambient authority (ADR 0009 #5 and #6): the file
;;; procedures and load are the ones (scheme file) and (scheme load) have,
;;; so importing this library always succeeds, and they raise unless the
;;; host grants the file capability.

(define-library (scheme r5rs)
  (import (scheme base) (scheme char) (scheme complex) (scheme cxr)
          (scheme eval) (scheme file) (scheme inexact) (scheme lazy)
          (scheme load) (scheme read) (scheme repl) (scheme write))
  (export
    * + - / < <= = > >= abs acos and angle append apply asin assoc assq assv
    atan begin boolean?
    caaaar caaadr caaar caadar caaddr caadr caar cadaar cadadr cadar caddar
    cadddr caddr cadr call-with-current-continuation call-with-input-file
    call-with-output-file call-with-values car case
    cdaaar cdaadr cdaar cdadar cdaddr cdadr cdar cddaar cddadr cddar cdddar
    cddddr cdddr cddr cdr ceiling char->integer char-alphabetic? char-ci<=?
    char-ci<? char-ci=? char-ci>=? char-ci>? char-downcase char-lower-case?
    char-numeric? char-ready? char-upcase char-upper-case? char-whitespace?
    char<=? char<? char=? char>=? char>? char? close-input-port
    close-output-port complex? cond cons cos current-input-port
    current-output-port define define-syntax delay denominator display do
    dynamic-wind eof-object? eq? equal? eqv? eval even? exact->inexact
    exact? exp expt floor for-each force gcd if imag-part inexact->exact
    inexact? input-port? integer->char integer? interaction-environment
    lambda lcm length let let* let-syntax letrec letrec-syntax list
    list->string list->vector list-ref list-tail list? load log magnitude
    make-polar make-rectangular make-string make-vector map max member memq
    memv min modulo negative? newline not null-environment null? number->string
    number? numerator odd? open-input-file open-output-file or output-port?
    pair? peek-char positive? procedure? quasiquote quote quotient rational?
    rationalize read read-char real-part real? remainder reverse round
    scheme-report-environment set! set-car! set-cdr! sin sqrt string
    string->list string->number string->symbol string-append string-ci<=?
    string-ci<? string-ci=? string-ci>=? string-ci>? string-copy
    string-fill! string-length string-ref string-set! string<=? string<?
    string=? string>=? string>? string? substring symbol->string symbol?
    tan truncate values vector vector->list vector-fill! vector-length
    vector-ref vector-set! vector? with-input-from-file with-output-to-file
    write write-char zero?
    ;; R5RS's syntax-rules and the auxiliary syntax its keywords use, which
    ;; appendix A's list leaves out though programs need them.
    syntax-rules else => unquote unquote-splicing ... _)
  (begin
    (define (exact->inexact z) (inexact z))
    (define (inexact->exact z) (exact z))

    ;; R7RS leaves other versions to the implementation; Vali has R5RS's
    ;; names only.
    (define (%check-version who version)
      (unless (eqv? version 5)
        (error (string-append who ": only version 5 is available") version)))

    (define (scheme-report-environment version)
      (%check-version "scheme-report-environment" version)
      (environment '(scheme r5rs)))

    ;; The syntax of R5RS: its keywords and the auxiliary syntax they use.
    (define (null-environment version)
      (%check-version "null-environment" version)
      (environment '(only (scheme base)
                          and begin case cond define define-syntax do
                          if lambda let let* let-syntax letrec letrec-syntax
                          or quasiquote quote set! syntax-rules
                          else => unquote unquote-splicing ... _)
                   '(only (scheme lazy) delay)))))
