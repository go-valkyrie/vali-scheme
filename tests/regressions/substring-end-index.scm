;;; requirement: (substring s start end) takes an end index
;;; found-in: scheme-rs
;;; kind: scheme

;; scheme-rs treated substring's third argument as a length.

(import (scheme base) (srfi 64))

(test-begin "substring-end-index")
(test-equal "cd" (substring "abcdef" 2 4))
(test-equal "" (substring "abcdef" 3 3))
(test-equal "abcdef" (substring "abcdef" 0 6))
(test-error (substring "abcdef" 4 2))
(test-end)
