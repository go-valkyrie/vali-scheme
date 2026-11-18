;;; requirement: A record type error names the missing field, not an address
;;; found-in: vali
;;; kind: scheme

;; A constructor that named a field the record type lacks reported it as
;; "point has no field #<Symbol 0x...>", a memory address that differs
;; between runs.

(import (scheme base) (scheme eval) (scheme write) (srfi 64))

(test-begin "record-field-error-names-field")
(test-equal "point has no field y"
  (guard (e ((error-object? e) (error-object-message e)))
    (eval '(begin
             (define-record-type point (make-point x y) point? (x point-x))
             (make-point 1 2))
          (environment '(scheme base)))))
(test-end)
