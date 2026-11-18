;;; Pair mutation: set-car! and set-cdr! on one fresh pair, again and again.
;;; expect: 2999999
(import (scheme base))
(define p (cons 0 0))
(let loop ((i 0))
  (if (= i 3000000)
      (car p)
      (begin (set-car! p i) (set-cdr! p i) (set-car! p i) (set-cdr! p i) (loop (+ i 1)))))
