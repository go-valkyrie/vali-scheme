;;; (scheme process-context): the command line, the environment and exiting
;;; (R7RS 6.14). The process is not ambient authority (ADR 0009 #5 and #6):
;;; importing this library always succeeds, but each procedure needs the
;;; process-context capability, which no evaluation is given yet, so it
;;; raises a condition saying so.

(define-library (scheme process-context)
  (import (scheme base))
  (export command-line emergency-exit exit
          get-environment-variable get-environment-variables)
  (begin
    (define (%needs-process name)
      (lambda args
        (error (string-append
                (symbol->string name)
                ": needs the process-context capability, which this program was not given"))))

    (define command-line (%needs-process 'command-line))
    (define emergency-exit (%needs-process 'emergency-exit))
    (define exit (%needs-process 'exit))
    (define get-environment-variable (%needs-process 'get-environment-variable))
    (define get-environment-variables (%needs-process 'get-environment-variables))))
