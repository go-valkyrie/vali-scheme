;;; requirement: The (scheme r5rs) and (scheme load) libraries exist
;;; found-in: R7RS-small audit
;;; kind: scheme

;; (import (scheme r5rs)) and (import (scheme load)) reported that the
;; library was not found. (scheme r5rs) re-exports R5RS's names and adds
;; exact->inexact, inexact->exact and the two environments. (scheme load)
;; reads a file, so it needs the file capability like (scheme file), which
;; this host does not grant (ADR 0009); importing it still succeeds.

(import (scheme base) (scheme eval) (scheme load) (srfi 64)
        (only (scheme r5rs)
              exact->inexact inexact->exact null-environment
              scheme-report-environment interaction-environment
              call-with-input-file open-input-file with-output-to-file
              load char-ready? delay force quotient exact? string-copy))

(test-begin "scheme-r5rs-and-load-libraries")

(test-equal 0.5 (exact->inexact 1/2))
(test-assert (inexact? (exact->inexact 3)))
(test-equal 1/2 (inexact->exact 0.5))
(test-assert (exact? (inexact->exact 2.0)))

;; scheme-report-environment has every R5RS name.
(let ((env (scheme-report-environment 5)))
  (test-equal 3 (eval '(+ 1 2) env))
  (test-equal 0.25 (eval '(exact->inexact 1/4) env))
  (test-equal 2 (eval '(quotient 5 2) env))
  (test-equal '(2 4) (eval '(map (lambda (x) (* 2 x)) '(1 2)) env))
  (test-equal 3 (eval '(force (delay 3)) env))
  (test-equal 'b (eval '(case 2 ((1) 'a) ((2) 'b) (else 'c)) env))
  (test-equal 5 (eval '(call-with-current-continuation (lambda (k) (k 5))) env)))

;; null-environment has the syntax and nothing else.
(let ((env (null-environment 5)))
  (test-equal 3 (eval '(if #t 3 4) env))
  (test-equal 3 (eval '((lambda (x) x) 3) env))
  (test-equal 'b (eval '(case 2 ((1) 'a) ((2) 'b) (else 'c)) env))
  (test-equal 'yes (eval '(cond (#f 'no) (else 'yes)) env))
  (test-equal 'yes (eval '(cond (#t => (lambda (x) 'yes)) (else 'no)) env))
  (test-equal '(1 2) (eval '`(1 ,(if #t 2 3)) env))
  (test-equal '(1 2 3) (eval '`(1 ,@'(2 3)) env))
  (test-equal 3 (eval '(let ((x 1)) (let* ((y x)) (letrec ((z y)) (do ((i 0 i)) (#t 3))))) env))
  (test-assert (eval '(and (or #f #t) (begin 1)) env))
  (test-assert (not (eq? #f (eval '(delay 3) env))))
  (test-error (eval '(car '(1)) env))
  (test-error (eval '+ env)))

;; Only version 5 is available.
(test-error (scheme-report-environment 4))
(test-error (null-environment 7))

(test-equal 5 (eval '(+ 2 3) (interaction-environment)))

;; Without the file capability, load raises a file error, and so do the
;; file procedures (scheme r5rs) re-exports.
(define (file-error-from thunk)
  (guard (e ((file-error? e) 'file-error)) (thunk) 'no-error))
(test-equal 'file-error (file-error-from (lambda () (load "no-such-file.scm"))))
(test-equal 'file-error (file-error-from (lambda () (open-input-file "x"))))

(test-end "scheme-r5rs-and-load-libraries")
