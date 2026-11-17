;;; (scheme process-context): the command line, the environment and exiting
;;; (R7RS 6.14). The process is not ambient authority (ADR 0009 #5 and #6):
;;; importing this library always succeeds, but reading the command line or
;;; the environment needs the process-context capability, which Vali's own
;;; version of this library is never given, so those raise a condition
;;; saying so. A host that grants it serves its own (scheme
;;; process-context) through its loader, which Vali consults first for
;;; this library. Exiting ends the run, and needs nothing.

(define-library (scheme process-context)
  (import (scheme base) (vali exit))
  (export command-line emergency-exit exit
          get-environment-variable get-environment-variables)
  (begin
    (define (%needs-process name)
      (lambda args
        (error (string-append
                (symbol->string name)
                ": needs the process-context capability, which this program was not given"))))

    (define command-line (%needs-process 'command-line))
    (define get-environment-variable (%needs-process 'get-environment-variable))
    (define get-environment-variables (%needs-process 'get-environment-variables))))
