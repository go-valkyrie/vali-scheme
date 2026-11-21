;;; requirement: cond-expand tests a library whose name a macro passes in
;;; found-in: vali
;;; kind: scheme

;; The loader walked the source for literal `(library ...)` names only, so
;; a name that reached `cond-expand` as a macro's argument read as missing
;; even when the library existed (#65). Expansion asks for such a name
;; when it meets it.

(define-library (a)
  (import (scheme base))
  (export av)
  (begin (define av 'from-a)))

(import (scheme base) (scheme eval) (srfi 64) (a))

(define-syntax if-lib
  (syntax-rules ()
    ((_ lib yes no) (cond-expand ((library lib) yes) (else no)))))

(test-begin "cond-expand-library-from-macro-argument")
(test-equal 'yes (if-lib (a) 'yes 'no))
(test-equal 'yes (if-lib (scheme base) 'yes 'no))
(test-equal 'no (if-lib (nope) 'yes 'no))
;; Evaluated code, with a macro of its own.
(define (eval-if-lib name)
  (eval `(let-syntax ((m (syntax-rules ()
                           ((_ lib) (cond-expand ((library lib) 'yes) (else 'no))))))
           (m ,name))
        (environment '(scheme base))))
(test-equal 'yes (eval-if-lib '(a)))
(test-equal 'no (eval-if-lib '(nope)))
(test-end)
