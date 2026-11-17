;;; (scheme write): printing values to a textual output port, the current
;;; output port by default (R7RS 6.13.3). write labels what closes a cycle,
;;; write-shared everything shared, and write-simple nothing.

(define-library (scheme write)
  (import (scheme base) (only (vali primitive) %write-to))
  (export display write write-shared write-simple
          ;; Also in (scheme base); kept here for programs written before
          ;; ports.
          newline write-string write-char)
  (begin
    (define (%printer style)
      (lambda (obj . port)
        (%write-to obj (if (pair? port) (car port) (current-output-port)) style)))

    (define display (%printer 0))
    (define write (%printer 1))
    (define write-shared (%printer 2))
    (define write-simple (%printer 3))))
