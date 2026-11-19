;;; Walks one list of immutable pairs with icar and icdr, many times: no
;;; allocation in the loop, so it times the reads alone. Compare with
;;; pair-scan.scm, the same program over pairs (ADR 0002 #14).
;;; expect: 9990000000
(import (scheme base) (srfi 116))
(define data (iiota 1000 0 1))
(define (sum l)
  (let loop ((l l) (acc 0))
    (if (ipair? l) (loop (icdr l) (+ acc (icar l))) acc)))
(define (run k total)
  (if (= k 0)
      total
      (run (- k 1) (+ total (sum data)))))
(run 20000 0)
