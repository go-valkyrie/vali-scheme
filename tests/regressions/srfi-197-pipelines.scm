;;; requirement: (srfi 197) pipeline operators are available
;;; found-in: SRFI audit
;;; kind: scheme

(import (scheme base) (srfi 64) (srfi 197))

(define (describe-number n)
  (chain-when '()
    ((odd? n) (cons "odd" _))
    ((even? n) (cons "even" _))
    ((zero? n) (cons "zero" _))
    ((positive? n) (cons "positive" _))))

(test-begin "srfi-197")
(test-assert (memq 'srfi-197 (features)))

;; chain
(test-equal '(a b x) (let ((x 'x)) (chain x (list 'a 'b _))))
(test-equal 7 (chain 3 (+ _ 1) (* _ 2) (- _ 1)))
(test-equal 5 (chain 4 (+ 1 _)))
(test-equal 4 (chain 4))
(test-equal '(2 3 4) (chain '(1 2 3) (map (lambda (x) (+ x 1)) _)))
(test-equal 3 (chain (values 1 2) (+ _ _)))
(test-equal 10 (chain (values 1 2 3) (+ _ _ _ 4)))
(test-equal 10 (chain (values 1 2 3 4) (+ _ ...)))
(test-equal 6 (chain (values 1 2 3) (+ _ _ _) ))
(test-equal 13 (chain (values 1 2) (+ 10 _ ...) (+ _ 0)))
(test-equal '(1 0 2 3) (chain (values 1 2 3) (list _ 0 _ ...)))
;; Custom placeholder and ellipsis.
(test-equal 7 (chain 3 <> (+ <> 4)))
(test-equal 10 (chain (values 1 2 3 4) - --- (+ - ---)))
;; Steps run in order.
(test-equal '(1 2 3)
  (let ((log '()))
    (define (note x) (set! log (cons x log)) x)
    (chain (note 1) (+ (note 2) _) (+ (note 3) _))
    (reverse log)))

;; chain-and
(test-equal 5 (chain-and 3 (+ _ 2)))
(test-equal #f (chain-and 3 (memv _ '(1 2)) (cdr _)))
(test-equal #f (chain-and #f (+ _ 1)))
(test-equal 8 (chain-and 3 (+ _ 1) (* _ 2)))
(test-equal 'b (chain-and '((a . b)) (assq 'a _) (cdr _)))
(test-equal #f (chain-and '((a . b)) (assq 'z _) (cdr _)))

;; chain-when
(test-equal '("positive" "odd") (describe-number 3))
(test-equal '("positive" "even") (describe-number 4))
(test-equal '("zero" "even") (describe-number 0))
(test-equal 5 (chain-when 5 (#f (+ _ 1))))

;; chain-lambda
(test-equal 3 ((chain-lambda (+ _ 1) (* _ 2) (- _ 3)) 2))
(test-equal 9 ((chain-lambda (+ _ _) (* _ 3)) 1 2))
(test-equal 6 ((chain-lambda (+ _ ...)) 1 2 3))
(test-equal 12 ((chain-lambda (+ _ ...) (* _ 2)) 1 2 3))
(test-equal 3 ((chain-lambda (+ _ 1)) 2))

;; nest
(test-equal '(a b (c d e)) (nest (list 'a 'b _) (list 'c 'd _) 'e))
(test-equal '(1 2 (3 (4) 5)) (nest '_ (1 2 _) (3 _ 5) (_) 4))
(test-equal '(1 2 3 (4 5 6)) (nest (nest _2 '_2 (1 2 3 _2) _ 6) (_ 5 _2) 4))
(test-equal 5 (nest 5))
(test-equal 6 (nest (let ((x 5)) _) (+ x 1)))
(test-equal '(1 (2 (3))) (nest (list 1 _) (list 2 _) '(3)))

;; nest-reverse
(test-equal '(a b (c d e)) (nest-reverse 'e (list 'c 'd _) (list 'a 'b _)))
(test-equal 3 (nest-reverse 3))
(test-end)
