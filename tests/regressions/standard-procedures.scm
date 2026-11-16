;;; requirement: Standard procedures present
;;; found-in: Steel, scheme-rs
;;; kind: scheme
;;; expect: fail, needs the standard library (M2)

;; Between them, Steel and scheme-rs lacked list-sort, command-line, string
;; output ports, keyword? and get-environment-variable. The test host grants
;; the process-context capability these need.

(import (scheme base) (scheme write) (scheme process-context)
        (srfi 64) (srfi 88) (srfi 132))

(test-begin "standard-procedures")
(test-equal '(1 2 3) (list-sort < '(3 1 2)))
(test-assert (list? (command-line)))
(test-equal "(a \"b\")"
  (let ((port (open-output-string)))
    (write '(a "b") port)
    (get-output-string port)))
(test-assert (keyword? foo:))
(test-assert (not (keyword? 'foo)))
(test-assert (let ((path (get-environment-variable "PATH")))
               (or (string? path) (not path))))
(test-end)
