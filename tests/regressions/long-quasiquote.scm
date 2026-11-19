;;; requirement: quasiquote of a long flat list expands in linear time and takes one macro step, not one per element
;;; found-in: vali
;;; kind: scheme

;; Quasiquote was a recursive macro that took a macro step for each
;; element of a list template, so one of about ten thousand elements failed
;; with "too many macro steps", and expanding it took time quadratic in
;; its length. It is a core form now that walks the template in one pass.
;;
;; The templates here are built as data and evaluated, so the file stays
;; small. Run with VALI_NO_OPT=1 as well.

(import (scheme base) (scheme eval) (srfi 64))

(define env (environment '(scheme base)))

(define (iota* n)
  (let loop ((i (- n 1)) (acc '()))
    (if (< i 0) acc (loop (- i 1) (cons i acc)))))

(define (qq template) (eval (list 'quasiquote template) env))

(test-begin "long-quasiquote")

;; A flat list with nothing to unquote.
(define n 100000)
(define m 20000)
(define flat (qq (iota* n)))
(test-equal n (length flat))
(test-equal (iota* n) flat)
;; The result is fresh, as with a template that has unquotes.
(set-car! flat 'changed)
(test-equal 'changed (car flat))
(test-equal 0 (car (qq (iota* 10))))

;; Unquotes and splices scattered through a long list.
(define (template i)
  (cond ((= 0 (modulo i 7919)) (list 'unquote (list '* i 2)))
        ((= 0 (modulo i 10007)) (list 'unquote-splicing '(list 'a 'b)))
        (else i)))
(define (expected i)
  (cond ((= 0 (modulo i 7919)) (list (* i 2)))
        ((= 0 (modulo i 10007)) (list 'a 'b))
        (else (list i))))
(define scattered
  (qq (let loop ((i (- m 1)) (acc '()))
        (if (< i 0) acc (loop (- i 1) (cons (template i) acc))))))
(test-equal (let loop ((i (- m 1)) (acc '()))
              (if (< i 0) acc (loop (- i 1) (append (expected i) acc))))
            scattered)

;; The same with a dotted tail, a vector and a long run of unquotes.
(test-equal (append (iota* m) 'end)
            (qq (append (iota* m) 'end)))
(test-equal (list->vector (iota* m)) (qq (list->vector (iota* m))))
(test-equal (map (lambda (i) (+ i 1)) (iota* 5000))
            (qq (map (lambda (i) (list 'unquote (list '+ i 1))) (iota* 5000))))

;; Nested deeply.
(define (nest depth inner)
  (let loop ((d 0) (t inner))
    (if (= d depth) t (loop (+ d 1) (list t)))))
(define (unnest t)
  (let loop ((t t) (d 0))
    (if (pair? t) (loop (car t) (+ d 1)) (cons d t))))
(test-equal '(1000 . 3) (unnest (qq (nest 1000 '(unquote (+ 1 2))))))
(test-equal '(1000 . 3) (unnest (qq (nest 1000 '(unquote-splicing (list (+ 1 2)))))) )

;; R7RS 4.2.8.
(test-equal '(list 3 4) `(list ,(+ 1 2) 4))
(test-equal '(list a (quote a))
            (let ((name 'a)) `(list ,name ',name)))
(test-equal '(a 3 4 5 6 b)
            `(a ,(+ 1 2) ,@(map abs '(4 -5 6)) b))
(test-equal '((foo 7) . cons)
            `((foo ,(- 10 3)) ,@(cdr '(c)) . ,(car '(cons))))
(test-equal '#(10 5 2 4 3 8)
            `#(10 5 ,(- 4 2) ,@(map (lambda (x) (- x 1)) '(5 4)) 8))
(test-equal '(a `(b ,(c) ,(foo 4 d) e) f)
            (let ((name1 'x) (name2 'y))
              `(a `(b ,(c) ,(foo ,(+ 1 3) d) e) f)))
(test-equal '(a `(b ,x ,'y d) e)
            (let ((name1 'x) (name2 'y))
              `(a `(b ,,name1 ,',name2 d) e)))
(test-equal '(list 3 4) (quasiquote (list (unquote (+ 1 2)) 4)))
(test-equal '`,(+ 1 2) `(quasiquote (unquote (+ 1 2))))
(test-equal '(1 `(2 ,(3 4 5)))
            `(1 `(2 ,(3 ,@(list 4 5)))))
(test-equal '(1 `(2 ,@(3 4)))
            `(1 `(2 ,@(3 ,@(list 4)))))

;; Splicing at the tail, in a vector, and the empty list.
(test-equal '(1 2 3) `(1 ,@(list 2 3)))
(test-equal '(2 3) `(,@(list 2 3)))
(test-equal '(1 . 2) `(1 . ,(+ 1 1)))
(test-equal '(1 2 . 3) `(1 ,@(list 2) . 3))
(test-equal '#() `#())
(test-equal '#(1 2 3) `#(,@(list 1 2) 3))
(test-equal '#(1 2) `#(1 ,@(list 2)))
(test-equal '() `())
(test-equal '() `(,@'()))
(test-equal '(1 2 (3 #(4 5))) `(1 2 (3 #(4 ,(+ 2 3)))))

;; A spliced list is copied when it is last, so the result is fresh.
(let* ((x (list 1 2)) (y `(,@x)))
  (test-assert (not (eq? x y))))

(test-end)
