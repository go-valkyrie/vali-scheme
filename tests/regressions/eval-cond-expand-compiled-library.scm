;;; requirement: cond-expand in evaluated code sees the program's libraries
;;; found-in: vali
;;; kind: scheme

;; `(library (a))` in evaluated code was false for a library the program
;; had already compiled, though `eval` could import it.

(define-library (a)
  (import (scheme base))
  (export av)
  (begin (define av 'from-a)))

(import (scheme base) (scheme eval) (srfi 64) (a))

(test-begin "eval-cond-expand-compiled-library")
(test-equal 'from-a (eval 'av (environment '(a))))
(test-equal 'yes
  (eval '(cond-expand ((library (a)) 'yes) (else 'no))
        (environment '(scheme base))))
(test-equal 'no
  (eval '(cond-expand ((library (nope)) 'yes) (else 'no))
        (environment '(scheme base))))
(test-end)
