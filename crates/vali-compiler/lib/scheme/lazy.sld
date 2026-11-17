;;; (scheme lazy): promises (R7RS 4.2.5), as R7RS's own definitions have
;;; them. A chain of delay-forces runs in constant space: force iterates,
;;; and each promise forced through another shares its box.

(define-library (scheme lazy)
  (import (scheme base))
  (export delay delay-force force make-promise promise?)
  (begin
    ;; The box is a pair: whether the promise is done, and its value or
    ;; the thunk that computes it.
    (define-record-type promise
      (%promise box)
      promise?
      (box promise-box set-promise-box!))

    (define (%make-promise done? value)
      (%promise (cons done? value)))

    (define-syntax delay-force
      (syntax-rules ()
        ((_ expression) (%make-promise #f (lambda () expression)))))

    (define-syntax delay
      (syntax-rules ()
        ((_ expression) (delay-force (make-promise expression)))))

    (define (make-promise obj)
      (if (promise? obj) obj (%make-promise #t obj)))

    (define (done? p) (car (promise-box p)))
    (define (value p) (cdr (promise-box p)))

    (define (force p)
      (if (not (promise? p))
          p
          (if (done? p)
              (value p)
              (let ((p* ((value p))))
                (unless (done? p)
                  (let ((box (promise-box p)))
                    (set-car! box (done? p*))
                    (set-cdr! box (value p*))
                    (set-promise-box! p* box)))
                (force p)))))))
