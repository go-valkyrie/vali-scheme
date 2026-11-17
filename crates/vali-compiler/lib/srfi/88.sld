;;; (srfi 88): keyword objects. The reader reads `name:` as a keyword,
;;; which evaluates to itself; these are the procedures.

(define-library (srfi 88)
  (import (only (vali primitive) keyword? keyword->string string->keyword))
  (export keyword? keyword->string string->keyword))
