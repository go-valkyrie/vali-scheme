;;; requirement: Reference counts are not out-of-line atomic calls
;;; found-in: scheme-rs profiling
;;; kind: benchmark

;; In scheme-rs, each copy or drop of a heap value was a call, a match on its
;; tag and a locked atomic instruction: 19 to 32% of main-thread time. This
;; benchmark passes lists around constantly. It profiles memory management's
;; share of the time under each counting strategy (ADR 0003).

;; For now the harness records its time under each memory strategy, in
;; target/vali-benchmarks/; the share described above needs a profiler.

(import (scheme base))

(define (build n)
  (let loop ((i 0) (acc '()))
    (if (= i n) acc (loop (+ i 1) (cons i acc)))))

(define (sum xs)
  (let loop ((xs xs) (total 0))
    (if (null? xs) total (loop (cdr xs) (+ total (car xs))))))

(define (run rounds)
  (let loop ((i 0) (total 0))
    (if (= i rounds)
        total
        (loop (+ i 1) (+ total (sum (build 1000)))))))

(run 2000)
