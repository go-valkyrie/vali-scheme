;;; requirement: Library loading is pluggable
;;; found-in: scheme-rs
;;; kind: host
;;; host: in-memory-loader
;;; expect: fail, needs loaders (M1, M2)

;; scheme-rs searched a fixed path: the current directory, one environment
;; variable, and its built-ins. The `in-memory-loader` host serves
;; `(regression virtual)` from memory; no file for it exists anywhere
;; (ADR 0009).

(import (scheme base) (srfi 64) (regression virtual))

(test-begin "pluggable-library-loading")
(test-equal 'served-from-memory (where-was-i-loaded-from))
(test-end)
