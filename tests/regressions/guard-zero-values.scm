;;; requirement: A guard clause can end in a zero-value expression
;;; found-in: scheme-rs
;;; kind: scheme

;; In scheme-rs, a guard clause ending with (newline) failed with "expected
;; 1 arguments, provided 0".

(import (scheme base) (scheme write) (srfi 64))

(test-begin "guard-zero-values")
(test-equal 'after-newline
  (begin (guard (e (#t (newline))) (raise 'oops))
         'after-newline))
(test-equal 'after-values
  (begin (guard (e (#t (values))) (raise 'oops))
         'after-values))
(test-end)
