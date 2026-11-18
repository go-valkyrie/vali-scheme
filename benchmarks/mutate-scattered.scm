;;; Pair mutation: set-car! on pairs in a pseudo-random order across many pages.
;;; expect: 1999999
(import (scheme base))
(define n 1000000)
(define v (make-vector n #f))
(let fill ((i 0)) (if (< i n) (begin (vector-set! v i (cons i i)) (fill (+ i 1)))))
(let loop ((k 0) (idx 0))
  (if (= k 2000000)
      (car (vector-ref v 0))
      (let ((j (modulo (+ idx 7919) n)))
        (set-car! (vector-ref v j) k)
        (loop (+ k 1) j))))
