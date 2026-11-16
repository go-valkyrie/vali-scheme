;;; requirement: _ matches inside a nested syntax-case
;;; found-in: scheme-rs
;;; kind: scheme
;;; expect: fail, needs the expander (M1)

;; In scheme-rs, `_` in a syntax-case inside a transformer's helper
;; procedure failed with "No patterns match".

(import (scheme base) (srfi 64))

(define-syntax second-of
  (lambda (stx)
    (define (second form)
      (syntax-case form ()
        ((_ b . _) #'b)))
    (syntax-case stx ()
      ((_ elements) (second #'elements)))))

(test-begin "nested-syntax-case-wildcard")
(test-equal 2 (second-of (1 2 3)))
(test-equal 'b (second-of (a 'b)))
(test-end)
