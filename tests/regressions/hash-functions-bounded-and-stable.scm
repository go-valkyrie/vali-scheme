;;; requirement: Hash functions are bounded, stable and in range on every kind of value
;;; found-in: SRFI audit
;;; kind: scheme

;; The primitives hash tables and comparators are built on. Each returns a
;; non-negative fixnum below 2^60 that agrees with its equivalence, and does
;; bounded work on deep or circular data. Identity hashes (`%eq-hash` and
;; `%eqv-hash` on objects) must stay the same however the memory strategy
;; lays out and reclaims the heap, and this test runs under all five.

(import (scheme base) (scheme char) (scheme complex) (srfi 64) (srfi 128))

;; The runtime's primitives, as the comparators and hash functions of
;; (srfi 128) expose them.
(define %eq-hash (comparator-hash-function (make-eq-comparator)))
(define %eqv-hash (comparator-hash-function (make-eqv-comparator)))
(define %equal-hash (comparator-hash-function (make-equal-comparator)))
(define %number-hash number-hash)
(define %string-hash string-hash)
(define %string-ci-hash string-ci-hash)
(define %symbol-hash symbol-hash)

(define (iota-list n)
  (let loop ((i (- n 1)) (acc '())) (if (< i 0) acc (loop (- i 1) (cons i acc)))))

(define (hash? h) (and (exact-integer? h) (>= h 0) (< h (expt 2 60))))

(test-begin "hash-functions-bounded-and-stable")

;; Every hash is in range, for every kind of value.
(for-each
 (lambda (x)
   (test-assert (hash? (%eq-hash x)))
   (test-assert (hash? (%eqv-hash x)))
   (test-assert (hash? (%equal-hash x))))
 (list 0 -1 (expt 2 80) 1/3 1.5 +nan.0 (make-rectangular 1 2) #t #f '() #\a
       "s" 's (list 1) (vector 1) (bytevector 1) car (lambda () 1)
       (current-output-port) (make-parameter 1) (eof-object)))

;; eq: the same object, the same hash; fixnums and immediates by value.
(let ((p (list 1 2)) (v (vector 1)) (s (make-string 2 #\a)) (f (lambda () 1)))
  (test-eqv (%eq-hash p) (%eq-hash p))
  (test-eqv (%eq-hash v) (%eq-hash v))
  (test-eqv (%eq-hash s) (%eq-hash s))
  (test-eqv (%eq-hash f) (%eq-hash f))
  (test-eqv (%eq-hash 42) (%eq-hash 42))
  (test-eqv (%eq-hash #\x) (%eq-hash #\x))
  (test-eqv (%eq-hash 'sym) (%eq-hash (string->symbol "sym")))
  (test-eqv (%eq-hash '()) (%eq-hash '()))
  ;; Allocating and dropping a great deal in between changes nothing.
  (let ((before (list (%eq-hash p) (%eq-hash v) (%eq-hash s) (%eq-hash f))))
    (do ((i 0 (+ i 1))) ((= i 50000)) (make-vector 10 (list i)))
    (test-equal before (list (%eq-hash p) (%eq-hash v) (%eq-hash s) (%eq-hash f)))))
;; Distinct objects almost always differ: a few thousand must spread out.
(let* ((objects (map (lambda (i) (list i)) (iota-list 2000)))
       (hashes (map %eq-hash objects)))
  (define (count-distinct hs)
    (let loop ((hs hs) (seen '()) (n 0))
      (cond ((null? hs) n)
            ((memv (car hs) seen) (loop (cdr hs) seen n))
            (else (loop (cdr hs) (cons (car hs) seen) (+ n 1))))))
  (test-assert (> (count-distinct hashes) 1900)))

;; eqv: numbers by value and exactness' sake, otherwise as eq.
(test-eqv (%eqv-hash 1.5) (%eqv-hash 1.5))
(test-eqv (%eqv-hash (expt 10 30)) (%eqv-hash (* (expt 10 15) (expt 10 15))))
(test-eqv (%eqv-hash 1/3) (%eqv-hash (/ 2 6)))
(test-eqv (%eqv-hash (make-rectangular 1 2)) (%eqv-hash (make-rectangular 1 2)))
(test-eqv (%eqv-hash +nan.0) (%eqv-hash (/ 0. 0.)))
(test-eqv (%eqv-hash #\a) (%eqv-hash #\a))
(let ((p (list 1)))
  (test-eqv (%eqv-hash p) (%eqv-hash p)))

;; equal: by content for pairs, vectors, strings and bytevectors.
(test-eqv (%equal-hash (list 1 "a" #(2 (3)))) (%equal-hash (list 1 (string #\a) (vector 2 (list 3)))))
(test-eqv (%equal-hash (bytevector 1 2)) (%equal-hash (bytevector 1 2)))
(test-eqv (%equal-hash "abc") (%string-hash "abc"))
(test-eqv (%equal-hash 'abc) (%symbol-hash 'abc))
(test-eqv (%equal-hash 3) (%number-hash 3))
(test-eqv (%equal-hash 2) (%equal-hash 2.0))
(test-assert (not (= (%equal-hash '(1 2)) (%equal-hash '(2 1)))))
(test-assert (not (= (%equal-hash '(1 2)) (%equal-hash '(1 2 3)))))
(test-assert (not (= (%equal-hash "ab") (%equal-hash "ba"))))
(test-assert (not (= (%equal-hash #(1 2)) (%equal-hash #(1 3)))))
(test-assert (not (= (%equal-hash (bytevector 1)) (%equal-hash (bytevector 2)))))

;; Strings and symbols.
(test-eqv (%string-hash "") (%string-hash (make-string 0)))
(test-assert (not (= (%string-hash "a") (%string-hash "b"))))
(test-eqv (%string-ci-hash "Hello") (%string-ci-hash "hELLO"))
(test-eqv (%string-ci-hash "Straße") (%string-ci-hash "STRASSE"))
(test-eqv (%string-ci-hash "σ") (%string-ci-hash "Σ"))
(test-assert (not (= (%symbol-hash 'a) (%symbol-hash 'b))))
(test-error (%string-hash 'a))
(test-error (%string-ci-hash 1))
(test-error (%symbol-hash "a"))
(test-error (%number-hash "1"))

;; Numbers hash by value across exactness, as `=` compares them.
(test-eqv (%number-hash 2) (%number-hash 2.0))
(test-eqv (%number-hash 0) (%number-hash 0.0))
(test-eqv (%number-hash 0) (%number-hash -0.0))
(test-eqv (%number-hash -5) (%number-hash -5.0))
(test-eqv (%number-hash 1/2) (%number-hash 0.5))
(test-eqv (%number-hash 3/8) (%number-hash 0.375))
(test-eqv (%number-hash (expt 2 62)) (%number-hash (inexact (expt 2 62))))
(test-eqv (%number-hash (expt 2 63)) (%number-hash (inexact (expt 2 63))))
(test-eqv (%number-hash (- (expt 2 63))) (%number-hash (inexact (- (expt 2 63)))))
(test-eqv (%number-hash (expt 2 200)) (%number-hash (inexact (expt 2 200))))
(test-eqv (%number-hash (make-rectangular 1 2)) (%number-hash (make-rectangular 1. 2.)))
(test-eqv (%number-hash 7) (%number-hash (make-rectangular 7. 0.)))
(test-assert (not (= (%number-hash 1) (%number-hash 2))))
(test-assert (not (= (%number-hash 1/3) (%number-hash 1/4))))
(test-assert (not (= (%number-hash (expt 10 100)) (%number-hash (+ 1 (expt 10 100))))))
(test-assert (not (= (%number-hash (expt 10 400)) (%number-hash (+ 1 (expt 10 400))))))
(test-assert (not (= (%number-hash (make-rectangular 1 2)) (%number-hash (make-rectangular 2 1)))))
(test-assert (not (= (%number-hash 1.5) (%number-hash 2.5))))

;; Deep and circular structures: bounded work, no stack overflow, and
;; bisimilar structures (which `equal?` equates) hash alike.
(define (nest-lists n)
  (let loop ((n n) (x '())) (if (= n 0) x (loop (- n 1) (list x)))))
(define (nest-vectors n)
  (let loop ((n n) (x '())) (if (= n 0) x (loop (- n 1) (vector x)))))
(test-eqv (%equal-hash (nest-lists 1000000)) (%equal-hash (nest-lists 1000000)))
(test-eqv (%equal-hash (nest-vectors 1000000)) (%equal-hash (nest-vectors 1000000)))
(test-eqv (%equal-hash (make-list 1000000 'a)) (%equal-hash (make-list 1000000 'a)))
(test-eqv (%equal-hash (make-vector 1000000 'a)) (%equal-hash (make-vector 1000000 'a)))
(let ((a (list 1)) (b (list 1 1)))
  (set-cdr! a a)
  (set-cdr! (cdr b) b)
  (test-assert (equal? a b))
  (test-assert (hash? (%equal-hash a)))
  (test-eqv (%equal-hash a) (%equal-hash b)))
(let ((a (list 1 2 3)) (b (list 1 2 3)))
  (set-cdr! (cddr a) a)
  (set-cdr! (cddr b) b)
  (test-eqv (%equal-hash a) (%equal-hash b)))
(let ((a (vector 1 #f)))
  (vector-set! a 1 a)
  (test-assert (hash? (%equal-hash a))))
(let ((a (list 0)))
  (set-car! a a)
  (test-assert (hash? (%equal-hash a))))
;; Differences past the part of a structure that is hashed may collide, and
;; differences within it do not.
(test-assert (not (= (%equal-hash (list 1 2 3 4)) (%equal-hash (list 1 2 3 5)))))

(test-end)
