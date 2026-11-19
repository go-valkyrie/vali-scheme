;;; Walks one list of pairs with car and cdr, many times: no allocation in
;;; the loop, so it times the reads alone. The baseline for ipair-scan.scm.
;;; expect: 9990000000
(import (scheme base))
(define data
  (let loop ((i 999) (acc '()))
    (if (< i 0) acc (loop (- i 1) (cons i acc)))))
(define (sum l)
  (let loop ((l l) (acc 0))
    (if (pair? l) (loop (cdr l) (+ acc (car l))) acc)))
(define (run k total)
  (if (= k 0)
      total
      (run (- k 1) (+ total (sum data)))))
(run 20000 0)
