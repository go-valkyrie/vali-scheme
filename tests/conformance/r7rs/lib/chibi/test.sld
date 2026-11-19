;;; A minimal stand-in for chibi-scheme's (chibi test), enough for
;;; r7rs-tests.scm: test-begin, test-end, test, test-assert, test-not,
;;; test-values and test-error. It also reads known-failures.scm, the
;;; cases Vali is known to get wrong, and sorts each case into passed,
;;; failed, known failure or unexpected pass. The outermost test-end
;;; prints the counts and exits with status 1 if any case failed or any
;;; known failure passed.
(define-library (chibi test)
  (import (scheme base) (scheme write) (scheme inexact) (scheme complex)
          (scheme process-context))
  (export test-begin test-end test test-assert test-not test-values
          test-error)
  (include "known-failures.scm")
  (begin
    ;; The known failures, as a list of (expression-text reason).
    (define known (map (lambda (entry) (cons (car entry) (cadr entry)))
                       known-failures))

    (define depth 0)
    (define passed 0)
    (define failed 0)
    (define known-failed 0)
    (define unexpected-passed 0)
    (define group-names '())

    (define (write-to-string x)
      (let ((out (open-output-string)))
        (write x out)
        (get-output-string out)))

    (define (approx-equal? a b epsilon)
      (cond ((> (abs a) (abs b)) (approx-equal? b a epsilon))
            ((zero? a) (< (abs b) epsilon))
            (else (< (abs (/ (- a b) b)) epsilon))))

    ;; chibi's test-equal?: equal?, but inexact expectations match within
    ;; a relative 1e-5.
    (define (test-equal? expect res)
      (or (equal? expect res)
          (if (real? expect)
              (and (inexact? expect) (real? res)
                   (approx-equal? expect res 1e-5))
              (and (complex? res) (complex? expect)
                   (test-equal? (real-part expect) (real-part res))
                   (test-equal? (imag-part expect) (imag-part res))))))

    (define (report-line status text detail)
      (display status)
      (display ": ")
      (unless (null? group-names)
        (display (car group-names))
        (display ": "))
      (display text)
      (when detail (display detail))
      (newline))

    ;; Records one case's outcome. `ok` is whether it behaved as the suite
    ;; expects; `detail` says how it did not.
    (define (record! text ok detail)
      (let ((known? (assoc text known)))
        (cond ((and ok known?)
               (set! unexpected-passed (+ unexpected-passed 1))
               (report-line "XPASS" text " (remove it from known-failures.scm)"))
              (ok (set! passed (+ passed 1)))
              (known? (set! known-failed (+ known-failed 1)))
              (else (set! failed (+ failed 1))
                    (report-line "FAIL" text detail)))))

    (define (run text thunk check)
      (let ((result (call/cc
                     (lambda (k)
                       (with-exception-handler
                        (lambda (e) (k (cons 'raised e)))
                        (lambda () (call-with-values thunk
                                     (lambda vals (cons 'returned vals)))))))))
        (check result)))

    (define (run-value text expect-thunk expr-thunk)
      (let* ((expected (call-with-values expect-thunk list))
             (key (string-append text " => "
                                 (write-to-string
                                  (if (and (pair? expected) (null? (cdr expected)))
                                      (car expected)
                                      expected)))))
        (run key expr-thunk
             (lambda (result)
               (if (eq? (car result) 'returned)
                   (record! key
                            (if (and (pair? expected) (null? (cdr expected))
                                     (pair? (cdr result))
                                     (null? (cddr result)))
                                (test-equal? (car expected) (cadr result))
                                (test-equal? expected (cdr result)))
                            (string-append ": got "
                                           (write-to-string (cdr result))))
                   (record! key #f ": raised an error"))))))

    (define (test-begin name)
      (set! depth (+ depth 1))
      (set! group-names (cons name group-names)))

    (define (test-end)
      (set! depth (- depth 1))
      (set! group-names (cdr group-names))
      (when (= depth 0)
        (display "r7rs: ")
        (display passed) (display " passed, ")
        (display failed) (display " failed, ")
        (display known-failed) (display " known failures, ")
        (display unexpected-passed) (display " known failures now passing")
        (newline)
        (when (or (> failed 0) (> unexpected-passed 0))
          (exit 1))))

    (define-syntax test
      (syntax-rules ()
        ((_ expect expr)
         (run-value (write-to-string 'expr)
                    (lambda () expect) (lambda () expr)))
        ((_ name expect expr)
         (test expect expr))))

    (define-syntax test-values
      (syntax-rules ()
        ((_ expect expr) (test expect expr))))

    (define-syntax test-assert
      (syntax-rules ()
        ((_ expr) (test #t (and expr #t)))
        ((_ name expr)
         (run-value name (lambda () #t) (lambda () (and expr #t))))))

    (define-syntax test-not
      (syntax-rules ()
        ((_ expr) (test #f expr))))

    ;; Passes when evaluating `expr` raises.
    (define-syntax test-error
      (syntax-rules ()
        ((_ expr)
         (let ((text (write-to-string 'expr)))
           (run text (lambda () expr)
                (lambda (result)
                  (record! text (eq? (car result) 'raised)
                           ": expected an error but got a value")))))
        ((_ name expr) (test-error expr))))))
