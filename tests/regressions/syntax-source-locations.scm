;;; requirement: Syntax objects expose source locations
;;; found-in: scheme-rs
;;; kind: scheme

;; scheme-rs kept source locations on syntax objects, but Scheme code could
;; not read them. The names below are provisional until (vali syntax) is
;; designed; lines are 1-based.

(import (scheme base) (srfi 64) (vali syntax))

(define-syntax line-of
  (lambda (stx)
    (syntax-case stx ()
      ((_ form) (datum->syntax stx (syntax-line #'form))))))

(test-begin "syntax-source-locations")
(test-equal 17 (line-of here))
(test-assert (string? (syntax-source #'here)))
(test-end)
