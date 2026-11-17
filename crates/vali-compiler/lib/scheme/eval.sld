;;; (scheme eval): evaluating data as code (R7RS 6.12). An environment is a
;;; set of imports, checked when it is made; evaluated code sees only
;;; those, and its definitions stay its own.
;;;
;;; A Rust procedure cannot run Scheme code, so %eval-prepare compiles and
;;; loads the code, with any library it needs that is not loaded yet, and
;;; returns thunks: each new library's body, then the code's. eval calls
;;; them in order, the last in tail position.

(define-library (scheme eval)
  (import (scheme base) (only (vali primitive) %environment %eval-prepare))
  (export eval environment)
  (begin
    (define (environment . sets) (apply %environment sets))

    (define (eval expr-or-def environment)
      (let loop ((thunks (%eval-prepare expr-or-def environment)))
        (if (null? (cdr thunks))
            ((car thunks))
            (begin ((car thunks)) (loop (cdr thunks))))))))
