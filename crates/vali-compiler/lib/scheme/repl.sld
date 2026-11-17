;;; (scheme repl): the interaction environment (R7RS 6.12), which a REPL
;;; evaluates in. It imports (scheme base); what is evaluated in it can
;;; define, and define again, names that later evaluations see, and
;;; (import set ...) evaluated in it adds to its imports.

(define-library (scheme repl)
  (import (scheme base) (only (vali primitive) %interaction-environment))
  (export interaction-environment)
  (begin
    (define (interaction-environment) (%interaction-environment))))
