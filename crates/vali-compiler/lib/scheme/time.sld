;;; (scheme time): the clock (R7RS 6.14). The clock is not ambient
;;; authority either (ADR 0009 #5 and #6): reading it needs the clock
;;; capability, which Vali's own version of this library is never given.
;;; A host that grants it serves its own (scheme time) through its loader,
;;; which Vali consults first for this library.

(define-library (scheme time)
  (import (scheme base))
  (export current-second current-jiffy jiffies-per-second)
  (begin
    (define (%needs-clock name)
      (lambda args
        (error (string-append
                (symbol->string name)
                ": needs the clock capability, which this program was not given"))))

    (define current-second (%needs-clock 'current-second))
    (define current-jiffy (%needs-clock 'current-jiffy))
    ;; A constant, so reading it reads no clock.
    (define (jiffies-per-second) 1000000)))
