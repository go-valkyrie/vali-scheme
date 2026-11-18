;;; requirement: Allocation takes no global lock
;;; found-in: scheme-rs profiling
;;; kind: benchmark

;; In scheme-rs, every allocation locked the global heap mutex and did a
;; hash-map lookup: 27 to 33% of allocation-heavy code. This benchmark
;; allocates many short-lived pairs, vectors and closures, and records the
;; time per allocation, which should involve no lock (ADR 0003).

;; For now the harness records its time under each memory strategy, in
;; target/vali-benchmarks/; the share described above needs a profiler.

(import (scheme base))

(define (churn n)
  (let loop ((i 0) (keep '()))
    (if (= i n)
        (length keep)
        (let* ((pair (cons i i))
               (vec (vector pair pair))
               (thunk (lambda () vec)))
          (loop (+ i 1)
                (if (= 0 (remainder i 1000)) (cons (thunk) keep) keep))))))

(churn 1000000)
