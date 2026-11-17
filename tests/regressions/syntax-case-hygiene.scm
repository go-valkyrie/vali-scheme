;;; requirement: syntax-case templates are hygienic
;;; found-in: Steel
;;; kind: scheme

;; In Steel, a template's `tmp` captured the user's `tmp`, so swapping a
;; variable named `tmp` silently did the wrong thing.

(import (scheme base) (srfi 64))

(define-syntax swap!
  (lambda (stx)
    (syntax-case stx ()
      ((_ a b)
       #'(let ((tmp a))
           (set! a b)
           (set! b tmp))))))

(test-begin "syntax-case-hygiene")
(define tmp 1)
(define other 2)
(swap! tmp other)
(test-equal 2 tmp)
(test-equal 1 other)
(test-end)
