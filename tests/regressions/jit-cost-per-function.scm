;;; requirement: JIT cost per function stays low
;;; found-in: scheme-rs
;;; kind: benchmark
;;; expect: fail, needs the baseline JIT and benchmarking (M3)

;; In scheme-rs, compiling each function cost about 0.6 ms, and Cranelift's
;; settings alone cut start-up by a third. The benchmark records the JIT's
;; time per function over many small, hot functions.

(import (scheme base) (srfi 1))

(define (make-adder k) (lambda (x) (+ x k)))
(define adders (map make-adder (iota 500)))

(define (run rounds)
  (let loop ((i 0) (sum 0))
    (if (= i rounds)
        sum
        (loop (+ i 1)
              (fold (lambda (f acc) (f acc)) sum adders)))))

(run 1000)
