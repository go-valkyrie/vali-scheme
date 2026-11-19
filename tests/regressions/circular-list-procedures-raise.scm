;;; requirement: List procedures raise on circular lists
;;; found-in: R7RS-small audit
;;; kind: scheme

;; length and the other procedures that walk a whole list used to loop
;; forever on a circular list, which hangs an embedding host. They raise
;; instead, and still work on every proper list.

(import (scheme base) (scheme write) (srfi 64))

(define (circular n)
  (let ((c (iota* n)))
    (set-cdr! (last-pair* c) c)
    c))
(define (iota* n)
  (let loop ((i n) (acc '()))
    (if (= i 0) acc (loop (- i 1) (cons i acc)))))
(define (last-pair* l)
  (if (pair? (cdr l)) (last-pair* (cdr l)) l))

;; A list with a tail before its cycle.
(define (rho)
  (let ((c (circular 3)))
    (cons 0 c)))

(define-syntax raises
  (syntax-rules ()
    ((_ expr)
     (guard (e (#t (error-object? e)))
       expr
       #f))))

(test-begin "circular-list-procedures-raise")
(test-assert (raises (length (circular 1))))
(test-assert (raises (length (circular 3))))
(test-assert (raises (length (rho))))
(test-assert (raises (list-copy (circular 3))))
(test-assert (raises (append (circular 3) '(1))))
(test-assert (raises (reverse (circular 3))))
(test-assert (raises (map (lambda (x) x) (circular 3))))
(test-assert (raises (for-each (lambda (x) x) (circular 3))))
(test-assert (raises (list->vector (circular 3))))
(test-assert (raises (list->string (let ((c (list #\a #\b))) (set-cdr! (cdr c) c) c))))
(test-assert (raises (apply + (circular 3))))
(test-assert (raises (member 'x (circular 3))))
(test-assert (raises (member 'x (circular 3) eq?)))
(test-assert (raises (memq 'x (circular 3))))
(test-assert (raises (assoc 'x (circular 3))))
(test-assert (raises (assq 'x (circular 3))))

;; Finding an element still works before the cycle is noticed.
(test-equal 2 (car (member 2 (circular 3))))
;; Walking a circular list by count is fine.
(test-equal 2 (list-ref (circular 3) 100))

(test-equal 0 (length '()))
(test-equal 5 (length '(1 2 3 4 5)))
(test-equal '(1 2 3) (list-copy '(1 2 3)))
(test-equal '(2 4) (map (lambda (x) (* 2 x)) '(1 2)))
(test-end)
