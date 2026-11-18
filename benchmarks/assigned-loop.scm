;;; Loops over variables assigned with set!, calling a procedure in the
;;; body: whether such variables stay boxed is what this measures.
;;; expect: 100000
(import (scheme base))
(define (step x) (+ x 1))
(define (count-up n)
  (define i 0)
  (define total 0)
  (let loop ()
    (when (< i n)
      (set! total (step total))
      (set! i (+ i 1))
      (loop)))
  total)
(let loop ((k 0) (r 0))
  (if (= k 10) r (loop (+ k 1) (count-up 100000))))
