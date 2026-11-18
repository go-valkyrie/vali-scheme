;;; Search over lists: counting the solutions of the eight queens puzzle,
;;; again and again.
;;; expect: 92
(import (scheme base))
(define (ok? row dist placed)
  (or (null? placed)
      (and (not (= (car placed) (+ row dist)))
           (not (= (car placed) (- row dist)))
           (not (= (car placed) row))
           (ok? row (+ dist 1) (cdr placed)))))
(define (try rows-left rows-tried placed)
  (if (null? rows-left)
      (if (null? rows-tried) 1 0)
      (+ (if (ok? (car rows-left) 1 placed)
             (try (append (cdr rows-left) rows-tried) '() (cons (car rows-left) placed))
             0)
         (try (cdr rows-left) (cons (car rows-left) rows-tried) placed))))
(define (queens n) (try (let loop ((i n) (acc '())) (if (= i 0) acc (loop (- i 1) (cons i acc)))) '() '()))
(let loop ((i 0) (r 0)) (if (= i 20) r (loop (+ i 1) (queens 8))))
