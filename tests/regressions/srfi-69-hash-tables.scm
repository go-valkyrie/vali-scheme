;;; requirement: (srfi 69) hash tables, as a view of (srfi 125)
;;; found-in: SRFI audit
;;; kind: scheme

;; The SRFI 69 names, and that importing it beside (srfi 125) and
;; (srfi 128) does not conflict, since the shared names are the same
;; procedures.

(import (scheme base) (scheme char) (srfi 64) (srfi 69) (srfi 125) (srfi 128))

(test-begin "srfi-69")
(test-assert (memq 'srfi-69 (features)))

(test-group "tables"
  (define t (make-hash-table))
  (test-assert (hash-table? t))
  (test-eq equal? (hash-table-equivalence-function t))
  (test-assert (procedure? (hash-table-hash-function t)))
  (hash-table-set! t '(1 2) 'a)
  (test-eq 'a (hash-table-ref t (list 1 2)))
  (test-eq 'b (hash-table-ref t 'x (lambda () 'b)))
  (test-error (hash-table-ref t 'x))
  (test-eq 'c (hash-table-ref/default t 'x 'c))
  (test-assert (hash-table-exists? t (list 1 2)))
  (hash-table-delete! t (list 1 2))
  (hash-table-delete! t 'not-there)
  (test-assert (not (hash-table-exists? t (list 1 2))))
  (test-eqv 0 (hash-table-size t))
  (hash-table-update! t 'n (lambda (v) (+ v 1)) (lambda () 0))
  (hash-table-update!/default t 'n (lambda (v) (+ v 1)) 0)
  (test-eqv 2 (hash-table-ref t 'n))
  (test-error (hash-table-update! t 'missing (lambda (v) v)))
  (test-equal '(n) (hash-table-keys t))
  (test-equal '(2) (hash-table-values t))
  (test-equal '((n . 2)) (hash-table->alist t))
  (let ((seen '()))
    (hash-table-walk t (lambda (k v) (set! seen (cons (list k v) seen))))
    (test-equal '((n 2)) seen))
  (test-eqv 12 (hash-table-fold t (lambda (k v acc) (+ v acc)) 10))
  (let ((copy (hash-table-copy t)))
    (hash-table-set! copy 'm 1)
    (test-eqv 1 (hash-table-size t))
    (test-eqv 2 (hash-table-size copy))
    (hash-table-merge! t copy)
    (test-eqv 2 (hash-table-size t))))

(test-group "constructors"
  (test-eq string=? (hash-table-equivalence-function (make-hash-table string=? string-hash)))
  (let ((t (make-hash-table string-ci=? string-ci-hash)))
    (hash-table-set! t "Key" 1)
    (test-eqv 1 (hash-table-ref t "KEY")))
  (let ((t (make-hash-table string-ci=?)))
    (hash-table-set! t "Key" 1)
    (test-eqv 1 (hash-table-ref t "KEY")))
  (let ((t (make-hash-table = number-hash)))
    (hash-table-set! t 2 'two)
    (test-eq 'two (hash-table-ref t 2.0)))
  (let ((t (make-hash-table eq? hash-by-identity 64)))
    (hash-table-set! t 'k 1)
    (test-eqv 1 (hash-table-ref t 'k)))
  (let ((t (alist->hash-table '((1 . a) (2 . b) (1 . c)))))
    (test-eqv 2 (hash-table-size t))
    (test-eq 'a (hash-table-ref t 1)))
  (let ((t (alist->hash-table '((1 . a)) eqv?)))
    (test-eq eqv? (hash-table-equivalence-function t))))

(test-group "hash functions"
  (test-assert (< (hash "string" 100) 100))
  (test-assert (< (hash '(1 2 3) 100) 100))
  (test-eqv (hash '(1 "a")) (hash (list 1 (string #\a))))
  (test-assert (< (string-hash "abc" 100) 100))
  (test-eqv (string-ci-hash "abc" 1000) (string-ci-hash "ABC" 1000))
  (test-assert (< (hash-by-identity 'a 100) 100))
  (test-eqv (hash-by-identity 'a) (hash-by-identity 'a)))

(test-end)
