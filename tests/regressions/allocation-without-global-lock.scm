;;; requirement: Allocation takes no global lock
;;; found-in: scheme-rs profiling
;;; kind: benchmark
;;; expect: fail, needs the runtime and benchmarking (M1)

;; In scheme-rs, every allocation locked the global heap mutex and did a
;; hash-map lookup: 27 to 33% of allocation-heavy code. This benchmark
;; allocates many short-lived pairs, vectors and closures, and records the
;; time per allocation, which should involve no lock (ADR 0003).

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
