;;; Inserts, looks up and deletes 100000 integer keys and 20000 string and
;;; list keys in (srfi 125) tables over the standard comparators: hashing,
;;; bucket search and growth. The tables are in Scheme, over the runtime's
;;; hash functions.
;;; expect: 140000
(import (scheme base) (srfi 125) (srfi 128))

(define (integer-keys n)
  (let ((t (make-hash-table (make-eqv-comparator))))
    (do ((i 0 (+ i 1))) ((= i n)) (hash-table-set! t i i))
    (let loop ((i 0) (sum 0))
      (if (= i n)
          (begin
            (do ((i 0 (+ i 2))) ((>= i n)) (hash-table-delete! t i))
            (+ (* 0 sum) (* 2 (hash-table-size t))))
          (loop (+ i 1) (+ sum (hash-table-ref t i)))))))

(define (string-keys n)
  (let ((t (make-hash-table (make-equal-comparator))))
    (do ((i 0 (+ i 1))) ((= i n)) (hash-table-set! t (number->string i) i))
    (do ((i 0 (+ i 1)) (hits 0 (+ hits (if (hash-table-ref/default t (number->string i) #f) 1 0))))
        ((= i n) hits))))

(define (list-keys n)
  (let ((t (make-hash-table (make-default-comparator))))
    (do ((i 0 (+ i 1))) ((= i n)) (hash-table-set! t (list i (* i 2) "k") i))
    (do ((i 0 (+ i 1)) (hits 0 (+ hits (if (hash-table-ref/default t (list i (* i 2) "k") #f) 1 0))))
        ((= i n) hits))))

(+ (integer-keys 100000) (string-keys 20000) (list-keys 20000))
