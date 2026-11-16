;;; requirement: Expansion-time file access goes through the host
;;; found-in: Steel, scheme-rs
;;; kind: host
;;; host: expansion-reads-scoped
;;; expect: fail, needs the expander, loaders and capabilities (M1, M2)

;; Steel blocked expansion-time file access completely, so `include` could
;; not work; scheme-rs allowed all of it. The `expansion-reads-scoped` host
;; grants expansion-time read access to this directory's `data/` and
;; nothing else (ADR 0009).

(import (scheme base) (scheme eval) (srfi 64))

(include "data/included.scm")

(test-begin "expansion-time-file-access")
;; Inside the granted directory, include works.
(test-equal 42 included-value)
;; Outside it, expansion fails with a capability error.
(test-error (eval '(include "/etc/hostname") (environment '(scheme base))))
(test-end)
