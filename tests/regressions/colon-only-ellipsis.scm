;;; requirement: R7RS custom ellipsis written as colons
;;; found-in: R7RS-small audit
;;; kind: scheme

;; R7RS 4.3.2 uses ::: as a custom ellipsis, so an identifier made only of
;; colons must not read as an SRFI 88 keyword, while foo: still does.

(import (scheme base) (scheme write) (srfi 64) (srfi 88))

(define-syntax be-like-begin
  (syntax-rules ()
    ((be-like-begin name)
     (define-syntax name
       (syntax-rules ()
         ((name expr (... ...))
          (begin expr (... ...))))))))
(be-like-begin sequence)

(define-syntax my-list
  (syntax-rules ::: ()
    ((_ a :::) (list a :::))))

(test-begin "colon-only-ellipsis")
(test-equal '(1 2 3) (my-list 1 2 3))
(test-equal 4 (sequence 1 2 3 4))
(test-assert (symbol? '::))
(test-assert (keyword? foo:))
(test-assert (not (keyword? '::)))
(test-end)
