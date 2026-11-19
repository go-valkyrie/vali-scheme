;;; Builds a list of pairs and walks it with car and cdr many times. The
;;; baseline for ipair-walk.scm, the same program over immutable pairs.
;;; expect: 499500000
(import (scheme base))
(define (build n)
  (let loop ((i (- n 1)) (acc '()))
    (if (< i 0) acc (loop (- i 1) (cons i acc)))))
(define (sum l)
  (let loop ((l l) (acc 0))
    (if (pair? l) (loop (cdr l) (+ acc (car l))) acc)))
(define (run k n total)
  (if (= k 0)
      total
      (run (- k 1) n (+ total (sum (build n))))))
(run 1000 1000 0)
