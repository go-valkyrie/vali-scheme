;;; requirement: Symbols from syntax->datum are eq?
;;; found-in: Steel
;;; kind: scheme
;;; expect: fail, needs syntax objects at run time, for #'x outside a transformer (M1)

;; In Steel, symbols produced by syntax->datum were not eq? to the same
;; symbols written literally, so memq silently failed where member worked.

(import (scheme base) (srfi 64))

(define-syntax one-of-x-y?
  (lambda (stx)
    (syntax-case stx ()
      ((_ id)
       (if (memq (syntax->datum #'id) '(x y)) #'#t #'#f)))))

(test-begin "syntax-datum-symbols-eq")
(test-assert (one-of-x-y? x))
(test-assert (one-of-x-y? y))
(test-assert (not (one-of-x-y? z)))
(test-assert (eq? (syntax->datum #'x) 'x))
(test-end)
