;;; requirement: Procedural macros can be exported from libraries
;;; found-in: Steel
;;; kind: scheme

;; Steel only exported syntax-rules macros, so a library could not offer a
;; macro that computes its expansion; hosts needed a separate expansion step.

(define-library (regression exported-macros)
  (export count-args)
  (import (scheme base))
  (begin
    (define-syntax count-args
      (lambda (stx)
        (syntax-case stx ()
          ((_ arg ...)
           (datum->syntax stx (length (syntax->datum #'(arg ...))))))))))

(import (scheme base) (srfi 64) (regression exported-macros))

(test-begin "exported-procedural-macros")
(test-equal 3 (count-args a b c))
(test-equal 0 (count-args))
(test-end)
