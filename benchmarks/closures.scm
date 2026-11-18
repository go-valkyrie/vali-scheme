;;; Higher-order code: closures made and called through map, fold and
;;; composition, with free variables of their own.
;;; expect: 333333000000
(import (scheme base))
(define (compose f g) (lambda (x) (f (g x))))
(define (fold f acc l) (if (null? l) acc (fold f (f (car l) acc) (cdr l))))
(define (iota n) (let loop ((i (- n 1)) (acc '())) (if (< i 0) acc (loop (- i 1) (cons i acc)))))
(define numbers (iota 1000))
(define (sum-of-squares-plus k)
  (let ((add-k (lambda (x) (+ x k))) (square (lambda (x) (* x x))))
    (fold + 0 (map (compose add-k square) numbers))))
(let loop ((k 0) (total 0))
  (if (= k 1000) total (loop (+ k 1) (+ total (sum-of-squares-plus k)))))
