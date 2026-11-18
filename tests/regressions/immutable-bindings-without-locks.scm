;;; requirement: Library bindings that are never mutated are read without a cell or lock
;;; found-in: scheme-rs profiling
;;; kind: benchmark

;; In scheme-rs, every global read went through an RwLock and a clone: 26% of
;; fib(33). Reading `fib` itself is a global read on every call. The
;; benchmark profiles this and records the share spent reading bindings,
;; which should be zero (ADR 0005).

;; For now the harness records its time under each memory strategy, in
;; target/vali-benchmarks/; the share described above needs a profiler.

(import (scheme base))

(define (fib n)
  (if (< n 2)
      n
      (+ (fib (- n 1)) (fib (- n 2)))))

(fib 30)
