;;; A stand-in for (scheme r5rs), which Vali's standard libraries do not
;;; provide yet. r7rs-tests.scm imports it, so the suite can't load
;;; without it. It exports only the names (scheme r5rs) has that no other
;;; (scheme ...) library does, since the suite imports those too.
(define-library (scheme r5rs)
  (import (scheme base) (scheme eval) (scheme lazy))
  (export exact->inexact inexact->exact
          scheme-report-environment null-environment)
  (begin
    (define (exact->inexact z) (inexact z))
    (define (inexact->exact z) (exact z))
    (define (scheme-report-environment version)
      (environment '(scheme base)))
    (define (null-environment version)
      (environment '(only (scheme base)
                          and begin case cond define define-syntax do
                          if lambda let let* let-syntax letrec letrec-syntax
                          or quasiquote quote set! syntax-rules
                          else => unquote unquote-splicing ... _)
                   '(only (scheme lazy) delay)))))
