;;; requirement: Hash functions agree with eq?, eqv?, equal? and =
;;; found-in: SRFI audit
;;; kind: scheme

;; A hash table is correct only if objects its equivalence calls the same
;; hash alike. This takes a corpus of objects, made twice so that equal
;; ones are not identical, and checks every pair of them against each
;; comparator's equivalence and hash. Under every memory strategy, so that
;; identity hashes survive whatever a collector does between two calls.

(import (scheme base) (scheme complex) (srfi 64) (srfi 128))

(define (corpus)
  (list 0 -0.0 0.0 1 1.0 -1 2 2.0 1/2 .5 -1/2 1/3 (/ 1. 3) 1e300 1e-300
        (expt 2 62) (expt 2 63) (expt 2 64) (- (expt 2 64)) (inexact (expt 2 64))
        (expt 2 100) (inexact (expt 2 100)) (+ (expt 2 100) 1)
        (expt 10 400) (/ (expt 10 400) 3)
        +inf.0 -inf.0 +nan.0
        (make-rectangular 1 2) (make-rectangular 1.0 2.0) (make-rectangular 1 2.0)
        (make-rectangular 1/2 1/3) (make-rectangular 1.5 0.0) 1.5
        #t #f '() #\a #\A #\x3bb
        "" "abc" (string #\a #\b #\c) "ABC" "Straße" "STRASSE"
        'abc (string->symbol "abc") 'abd '|hello world|
        (bytevector) (bytevector 1 2) (bytevector 1 2 3)
        (list 1 2) (list 1 2.0) (list 1.0 2) (cons 1 2) (list 1 (list 2 "x"))
        (vector) (vector 1 2) (vector 1 2.0) (vector 1 (list 2 "x"))
        (list "a" 'b #\c 4 5.5 #(6) #u8(7))
        (let ((x (list 1 2 3))) (set-cdr! (cddr x) x) x)
        (let ((x (list 1 2 3 1 2 3))) (set-cdr! (cddr (cddr x)) x) x)
        (let ((x (vector 1 #f))) (vector-set! x 1 x) x)
        (let loop ((n 50000) (x '())) (if (= n 0) x (loop (- n 1) (list x))))
        (make-list 50000 1) (make-vector 50000 1)
        car (lambda (x) x) (current-output-port) (make-parameter 1)))

(define in-range-bound (hash-bound))

(define (nan? x) (and (real? x) (not (= x x))))

(define (check-comparator name comparator equiv)
  (let ((a (corpus)) (b (corpus)))
    (for-each
     (lambda (x)
       (let ((hx (comparator-hash comparator x)))
         (test-assert (and (exact-integer? hx) (>= hx 0) (< hx in-range-bound)))
         ;; The same object always hashes the same.
         (test-eqv hx (comparator-hash comparator x))))
     a)
    (for-each
     (lambda (x)
       (for-each
        (lambda (y)
          (when (equiv x y)
            (test-eqv (comparator-hash comparator x) (comparator-hash comparator y))))
        b))
     a)))

(test-begin "hash-functions")

(check-comparator "eq" (make-eq-comparator) eq?)
(check-comparator "eqv" (make-eqv-comparator) eqv?)
(check-comparator "equal" (make-equal-comparator) equal?)
;; The default comparator equates numbers as `=` does, and compares the
;; rest like `equal?`.
(check-comparator "default" (make-default-comparator)
                  (lambda (x y) (and (not (and (number? x) (nan? x)))
                                     (not (and (number? y) (nan? y)))
                                     (or (equal? x y)
                                         (and (number? x) (number? y) (= x y))))))

;; number-hash is consistent with `=` on every pair of numbers.
(let ((numbers (let loop ((xs (corpus)) (acc '()))
                 (cond ((null? xs) acc)
                       ((and (number? (car xs)) (not (nan? (car xs))))
                        (loop (cdr xs) (cons (car xs) acc)))
                       (else (loop (cdr xs) acc))))))
  (for-each
   (lambda (x)
     (for-each
      (lambda (y)
        (when (= x y)
          (test-eqv (number-hash x) (number-hash y))))
      numbers))
   numbers))

(test-end)
