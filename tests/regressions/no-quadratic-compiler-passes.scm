;;; requirement: No quadratic compiler passes
;;; found-in: scheme-rs
;;; kind: performance

;; In scheme-rs, the reduce pass took 784 ms of a 1.1 s load, because it
;; recomputed use counts for every rewrite; a use-count census fixed it.
;;
;; The check compiles (program n) and (program (* 4 n)) and requires the
;; compile time to grow by at most about four times, not sixteen.

(import (scheme base) (srfi 1))

(define (name i)
  (string->symbol (string-append "f" (number->string i))))

;; n definitions in one body, each calling the one before, so that
;; reductions have plenty to rewrite.
(define (program n)
  `(begin
     (define (f0 x) x)
     ,@(map (lambda (i)
              `(define (,(name i) x)
                 (let ((y (+ x 1)))
                   (,(name (- i 1)) y))))
            (iota n 1))))
