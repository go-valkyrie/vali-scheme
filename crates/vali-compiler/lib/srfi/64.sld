;;; (srfi 64): the part of SRFI 64 Vali's tests use.
;;;
;;; Failures are collected as tests run; test-end raises an error listing
;;; them, so a file whose assertions fail ends in an error. Each failure is
;;; (test expected actual), test being the expression as written.

(define-library (srfi 64)
  (import (scheme base))
  (export test-begin test-end test-assert test-equal test-eqv test-eq
          test-error)
  (begin
    (define failures '())

    (define (test-begin . name)
      (set! failures '()))

    (define (test-end . name)
      (unless (null? failures)
        (let ((failed (reverse failures)))
          (set! failures '())
          (error "tests failed:" failed))))

    (define (check same? test expected actual)
      (unless (same? expected actual)
        (set! failures (cons (list test expected actual) failures))))

    (define-syntax test-assert
      (syntax-rules ()
        ((_ name expr) (check eq? 'expr #t (if expr #t #f)))
        ((_ expr) (check eq? 'expr #t (if expr #t #f)))))

    (define-syntax test-equal
      (syntax-rules ()
        ((_ name expected expr) (check equal? 'expr expected expr))
        ((_ expected expr) (check equal? 'expr expected expr))))

    (define-syntax test-eqv
      (syntax-rules ()
        ((_ name expected expr) (check eqv? 'expr expected expr))
        ((_ expected expr) (check eqv? 'expr expected expr))))

    ;; (test-error [[name] type] expr): expr must raise. The type is not
    ;; checked yet.
    (define (check-error test thunk)
      (unless (guard (e (#t #t)) (thunk) #f)
        (set! failures (cons (list test 'an-error 'no-error) failures))))

    (define-syntax test-error
      (syntax-rules ()
        ((_ name type expr) (check-error 'expr (lambda () expr)))
        ((_ type expr) (check-error 'expr (lambda () expr)))
        ((_ expr) (check-error 'expr (lambda () expr)))))

    (define-syntax test-eq
      (syntax-rules ()
        ((_ name expected expr) (check eq? 'expr expected expr))
        ((_ expected expr) (check eq? 'expr expected expr))))))
