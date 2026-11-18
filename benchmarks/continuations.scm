;;; call/cc used for early exit inside dynamic-wind, many times over.
;;; expect: 200000
(import (scheme base))
(define (first-negative l)
  (call/cc
    (lambda (return)
      (dynamic-wind
        (lambda () #f)
        (lambda () (for-each (lambda (x) (if (< x 0) (return x))) l) #f)
        (lambda () #f)))))
(define data '(1 2 3 4 5 6 7 8 9 -2 10 11))
(let loop ((i 0) (total 0))
  (if (= i 100000) (- total) (loop (+ i 1) (+ total (first-negative data)))))
