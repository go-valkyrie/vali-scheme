;;; Procedure calls and fixnum arithmetic: doubly recursive Fibonacci.
;;; expect: 832040
(import (scheme base))
(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
(fib 30)
