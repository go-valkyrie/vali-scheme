;;; (vali exit): ending a run (R7RS 6.14). Ending a run reaches nothing
;;; outside the program, so it needs no capability: the host gets an exit
;;; error with the code, and decides what to do with it. A host that
;;; serves its own (scheme process-context) re-exports these.

(define-library (vali exit)
  (import (scheme base)
          (only (vali primitive) %exit %winders %set-winders! %set-handlers!
                %set-parameterization!))
  (export exit emergency-exit)
  (begin
    ;; Runs every outstanding after thunk, innermost first, each in its
    ;; dynamic-wind's dynamic environment. A winder is
    ;; #(before after handlers parameterization), as (scheme base) makes it.
    (define (unwind-all!)
      (let ((ws (%winders)))
        (if (pair? ws)
            (let ((w (car ws)))
              (%set-winders! (cdr ws))
              (%set-handlers! (vector-ref w 2))
              (%set-parameterization! (vector-ref w 3))
              ((vector-ref w 1))
              (unwind-all!)))))

    (define (exit . obj)
      (unwind-all!)
      (%exit (if (pair? obj) (car obj) #t)))

    (define (emergency-exit . obj)
      (%exit (if (pair? obj) (car obj) #t)))))
