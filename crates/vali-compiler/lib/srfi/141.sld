;;; (srfi 141): integer division. Each of the five rounding rules (floor,
;;; ceiling, truncate, round and the two that keep the remainder in range,
;;; euclidean and balanced) has a `/` procedure returning quotient and
;;; remainder, and one for each alone. The quotient q and remainder r of n
;;; and d always satisfy n = d q + r; the rule decides which q is chosen.
;;; Floor and truncate are R7RS's own. The rest are written here from
;;; floor division, and work on inexact integers too.

(define-library (srfi 141)
  (import (scheme base))
  (export ceiling/ ceiling-quotient ceiling-remainder
          floor/ floor-quotient floor-remainder
          round/ round-quotient round-remainder
          truncate/ truncate-quotient truncate-remainder
          euclidean/ euclidean-quotient euclidean-remainder
          balanced/ balanced-quotient balanced-remainder)
  (begin
    ;; The remainder that goes with `q`.
    (define (%rem n d q) (- n (* d q)))

    (define (ceiling/ n d)
      (let ((q (- (floor-quotient (- n) d))))
        (values q (%rem n d q))))
    (define (ceiling-quotient n d) (- (floor-quotient (- n) d)))
    (define (ceiling-remainder n d)
      (%rem n d (ceiling-quotient n d)))

    ;; Nearest, with ties going to the even quotient. The floor
    ;; remainder `r` is between 0 and `d`, so compare twice its size with
    ;; `d`'s.
    (define (round/ n d)
      (let-values (((q r) (floor/ n d)))
        (let ((twice (abs (* 2 r))) (size (abs d)))
          (let ((q (cond ((< twice size) q)
                         ((> twice size) (+ q 1))
                         ((even? q) q)
                         (else (+ q 1)))))
            (values q (%rem n d q))))))
    (define (round-quotient n d)
      (let-values (((q r) (round/ n d))) q))
    (define (round-remainder n d)
      (let-values (((q r) (round/ n d))) r))

    ;; The remainder is never negative.
    (define (euclidean/ n d)
      (if (negative? d)
          (ceiling/ n d)
          (floor/ n d)))
    (define (euclidean-quotient n d)
      (let-values (((q r) (euclidean/ n d))) q))
    (define (euclidean-remainder n d)
      (let-values (((q r) (euclidean/ n d))) r))

    ;; The remainder is from -|d|/2 up to, not including, |d|/2.
    (define (balanced/ n d)
      (let-values (((q r) (euclidean/ n d)))
        (if (>= (* 2 r) (abs d))
            (values (if (negative? d) (- q 1) (+ q 1))
                    (- r (abs d)))
            (values q r))))
    (define (balanced-quotient n d)
      (let-values (((q r) (balanced/ n d))) q))
    (define (balanced-remainder n d)
      (let-values (((q r) (balanced/ n d))) r))))
