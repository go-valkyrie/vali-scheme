;;; Pair mutation: set-car! down a long list of contiguous pairs, many times.
;;; expect: 299
(import (scheme base))
(define xs (let loop ((i 0) (acc '())) (if (= i 10000) acc (loop (+ i 1) (cons i acc)))))
(define (bump! l n)
  (let loop ((l l))
    (if (pair? l) (begin (set-car! l n) (loop (cdr l))))))
(let loop ((n 0))
  (if (= n 300) (car xs) (begin (bump! xs n) (loop (+ n 1)))))
