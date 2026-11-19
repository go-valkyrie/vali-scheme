;;; (srfi 111): boxes, single-celled mutable objects. A box is a record
;;; type of its own, disjoint from every other type. (The VM's internal
;;; box is the cell of an assigned variable, never a first-class value.)

(define-library (srfi 111)
  (import (scheme base))
  (export box box? unbox set-box!)
  (begin
    (define-record-type %box
      (box value)
      box?
      (value unbox set-box!))))
