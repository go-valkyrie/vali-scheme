;;; (srfi 69): basic hash tables, the names SRFI 125 keeps for
;;; compatibility. This is a view of (srfi 125): the same procedures and
;;; the same tables, so a program may import both.
;;;
;;; Where SRFI 125 changed a name's behaviour, this library has 125's:
;;; `hash-table-merge!` keeps the first table's values on a clash, and
;;; `hash-table-copy` and `hash-table-ref` take SRFI 125's extra arguments.

(define-library (srfi 69)
  (import (srfi 125))
  (export make-hash-table hash-table? alist->hash-table
          hash-table-equivalence-function hash-table-hash-function
          hash-table-ref hash-table-ref/default hash-table-set!
          hash-table-delete! hash-table-exists? hash-table-update!
          hash-table-update!/default hash-table-size hash-table-keys
          hash-table-values hash-table-walk hash-table-fold
          hash-table->alist hash-table-copy hash-table-merge!
          hash string-hash string-ci-hash hash-by-identity))
