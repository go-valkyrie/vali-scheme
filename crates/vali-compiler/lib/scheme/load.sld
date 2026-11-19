;;; (scheme load): load (R7RS 6.14). Reading a file is not ambient
;;; authority (ADR 0009 #5 and #6), and load reads it through (scheme
;;; file), which a host serves when it grants the file capability. So
;;; importing this library always succeeds, and load raises the file error
;;; (scheme file) does until a host grants reading.

(define-library (scheme load)
  (import (scheme base) (scheme eval) (scheme file) (scheme read)
          (scheme repl))
  (export load)
  (begin
    ;; Reads each expression or definition of the file, in order, and
    ;; evaluates it in `environment`, the interaction environment by
    ;; default.
    (define (load filename . environment)
      (let ((env (cond ((null? environment) (interaction-environment))
                       ((null? (cdr environment)) (car environment))
                       (else (error "load: expected a file name and at most one environment"
                                    environment)))))
        (let ((port (open-input-file filename)))
          ;; The port closes however the loop ends.
          (dynamic-wind
            (lambda () #f)
            (lambda ()
              (let loop ((datum (read port)))
                (unless (eof-object? datum)
                  (eval datum env)
                  (loop (read port)))))
            (lambda () (close-port port))))))))
