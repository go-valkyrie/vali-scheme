;;; requirement: a quoted datum label keeps the sharing and cycles the reader produced
;;; found-in: vali
;;; kind: scheme

;; R7RS 2.4 and 7.1.2: datum labels denote the same object, and a quoted
;; constant may be circular. The compiler used to reject labels in
;; constants. Run with VALI_NO_OPT=1 and VALI_ROUND_TRIP=1 as well.

(import (scheme base) (scheme write) (scheme read) (scheme eval) (srfi 64))

(define (written x)
  (let ((port (open-output-string)))
    (write x port)
    (get-output-string port)))

;; Four cdrs on, a circular list is back where it started.
(define (cdrs x n)
  (if (= n 0) x (cdrs (cdr x) (- n 1))))

(define shared-list '(#0=(1 2) #0#))
(define shared-tail '(#1=(b c) a . #1#))
(define shared-vector '#(#0=(x) #0# #0#))
(define vector-of-vectors '#(#0=#(1) #0#))
(define car-cycle '#0=(#0# . b))
(define cdr-cycle '#0=(a . #0#))
(define long-cycle '#0=(a b c . #0#))
(define vector-cycle '#0=#(1 #0# 3))
(define through-vector '#0=(1 #(2 #0#)))
(define nested '(#0=(a) #1=(#0# . #1#)))

(test-begin "quoted-datum-labels-keep-sharing")

(test-assert "a shared sublist is one object"
  (eq? (car shared-list) (cadr shared-list)))
(test-equal "and prints as usual" '((1 2) (1 2)) shared-list)
(test-assert "the same through a let"
  (let ((x '(#0=(1 2) #0#))) (eq? (car x) (cadr x))))
(test-assert "different constants do not share"
  (not (eq? (car shared-list) (car '(#0=(1 2) #0#)))))
(test-assert "a shared tail is one object"
  (eq? (car shared-tail) (cddr shared-tail)))
(test-assert "shared vector elements are one object"
  (and (eq? (vector-ref shared-vector 0) (vector-ref shared-vector 1))
       (eq? (vector-ref shared-vector 1) (vector-ref shared-vector 2))))
(test-assert "a shared vector is one object"
  (eq? (vector-ref vector-of-vectors 0) (vector-ref vector-of-vectors 1)))

(test-assert "a cycle through cdr is circular"
  (not (list? cdr-cycle)))
(test-assert "and its cdr is itself" (eq? cdr-cycle (cdr cdr-cycle)))
(test-eq "and its car stays" 'a (car (cdrs cdr-cycle 5)))
(test-assert "a longer cycle returns to its start"
  (eq? long-cycle (cdrs long-cycle 3)))
(test-eq "with its elements in order" 'b (cadr (cdrs long-cycle 3)))
(test-assert "a cycle through car"
  (eq? car-cycle (car car-cycle)))
(test-eq "with its cdr intact" 'b (cdr car-cycle))
(test-assert "a cycle through a vector"
  (eq? vector-cycle (vector-ref vector-cycle 1)))
(test-eqv "with its other elements intact" 3 (vector-ref vector-cycle 2))
(test-assert "a list through a vector and back"
  (eq? through-vector (vector-ref (cadr through-vector) 1)))
(test-assert "labels nest"
  (let ((second (cadr nested)))
    (and (eq? (car second) (car nested))
         (eq? (cdr second) second))))

(test-equal "write labels a cdr cycle" "#0=(a . #0#)" (written cdr-cycle))
(test-equal "write labels a car cycle" "#0=(#0# . b)" (written car-cycle))
(test-equal "write labels a vector cycle" "#0=#(1 #0# 3)" (written vector-cycle))

;; Constants are immutable, cycles included.
(test-error (set-car! cdr-cycle 'z))
(test-error (vector-set! vector-cycle 0 'z))

;; What `read` makes agrees with what the compiler makes.
(let ((read-cycle (read (open-input-string "#0=(a . #0#)"))))
  (test-equal "read and quote write alike" (written read-cycle) (written cdr-cycle)))

;; The same through eval, which gets its code as a heap value.
(define env (environment (quote (scheme base))))
(define (evaluated text)
  (eval (read (open-input-string text)) env))

(test-assert "eval keeps a shared sublist"
  (let ((x (evaluated "(quote (#0=(1 2) #0#))")))
    (eq? (car x) (cadr x))))
(test-assert "eval keeps a shared string"
  (let ((x (evaluated "(quote (#0=\"ab\" #0#))")))
    (eq? (car x) (cadr x))))
(test-assert "eval accepts a circular constant"
  (let ((x (evaluated "(quote #0=(a b . #0#))")))
    (and (not (list? x)) (eq? x (cddr x)))))
(test-assert "eval keeps a cycle through a vector"
  (let ((x (evaluated "(quote #0=#(1 #0#))")))
    (eq? x (vector-ref x 1))))
(test-equal "eval copies shared code, which is not a constant"
  3
  (let ((code (list (quote +) 1 2)))
    (eval (list (quote car) (list (quote list) code code)) env)))

;; Deep nesting with a label. Tests run on a 2 MiB stack, which a pass that
;; recurses on depth overflows at a few thousand levels. The text is built
;; and read, so this file stays small: a label defined first and referred
;; to at the bottom of 100,000 nested lists, and the reverse nesting in a
;; vector.
(define deep 100000)

(define (repeat-string s n)
  (let ((port (open-output-string)))
    (do ((i 0 (+ i 1))) ((= i n))
      (write-string s port))
    (get-output-string port)))

(define (down x n)
  (if (= n 0) x (down (car x) (- n 1))))

(let* ((text (string-append "(quote (#0=(a) " (repeat-string "(" deep)
                            "#0#" (repeat-string ")" deep) "))"))
       (datum (evaluated text)))
  (test-assert "a label referred to 100,000 lists down is the same object"
    (eq? (car datum) (down (cadr datum) deep))))

(let* ((text (string-append "(quote #0=#(" (repeat-string "#(" deep)
                            "#0#" (repeat-string ")" deep) "))"))
       (datum (evaluated text)))
  (test-assert "a cycle 100,000 vectors down"
    (let loop ((v datum) (n deep))
      (if (= n 0)
          (eq? (vector-ref v 0) datum)
          (loop (vector-ref v 0) (- n 1))))))

(test-end "quoted-datum-labels-keep-sharing")
