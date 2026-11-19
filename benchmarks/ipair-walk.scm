;;; Builds a list of immutable pairs and walks it with icar and icdr many
;;; times. Compare with pair-walk.scm, the same program over pairs: the
;;; two should be close (ADR 0002 #14).
;;; expect: 499500000
(import (scheme base) (srfi 116))
(define (build n)
  (let loop ((i (- n 1)) (acc '()))
    (if (< i 0) acc (loop (- i 1) (ipair i acc)))))
(define (sum l)
  (let loop ((l l) (acc 0))
    (if (ipair? l) (loop (icdr l) (+ acc (icar l))) acc)))
(define (run k n total)
  (if (= k 0)
      total
      (run (- k 1) n (+ total (sum (build n))))))
(run 1000 1000 0)
