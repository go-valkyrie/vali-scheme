;;; requirement: Pair fields need no locks
;;; found-in: scheme-rs profiling
;;; kind: benchmark

;; In scheme-rs, car and cdr each took a read lock and cloned: car alone was
;; 16% of queens(12). This n-queens search walks lists constantly. The
;; benchmark profiles it and records the share spent in car and cdr, which
;; should be plain loads (ADRs 0002 and 0008).

;; For now the harness records its time under each memory strategy, in
;; target/vali-benchmarks/; the share described above needs a profiler.

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
             (try (append (cdr rows-left) rows-tried)
                  '()
                  (cons (car rows-left) placed))
             0)
         (try (cdr rows-left)
              (cons (car rows-left) rows-tried)
              placed))))

(define (iota1 n)
  (let loop ((i n) (acc '()))
    (if (= i 0) acc (loop (- i 1) (cons i acc)))))

(define (queens n)
  (try (iota1 n) '() '()))

(queens 10)
