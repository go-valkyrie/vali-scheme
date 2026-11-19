;;; requirement: A program importing only (scheme r5rs) has R5RS's syntax-rules and auxiliary syntax
;;; found-in: R7RS-small audit
;;; kind: scheme

;; Appendix A's export list for (scheme r5rs) leaves out syntax-rules and
;; the auxiliary syntax else, =>, unquote, unquote-splicing, ... and _,
;; which R5RS programs need: without them else was unbound, case found no
;; match, and a quasiquote silently kept its unquotes as data. Vali's
;; library exports them.

(import (scheme r5rs) (srfi 64))

(test-begin "scheme-r5rs-only-import")

(test-equal 2 (cond (#f 1) (else 2)))
(test-equal 'b (case 3 ((1) 'a) (else 'b)))
(test-equal 4 (cond ((assv 2 '((2 . 4))) => cdr) (else #f)))
(define-syntax swap!
  (syntax-rules ()
    ((_ a b) (let ((tmp a)) (set! a b) (set! b tmp)))))
(test-equal '(2 1) (let ((x 1) (y 2)) (swap! x y) (list x y)))
(test-equal '(1 2 3) `(1 ,(+ 1 1) ,@(list 3)))

(test-end "scheme-r5rs-only-import")
