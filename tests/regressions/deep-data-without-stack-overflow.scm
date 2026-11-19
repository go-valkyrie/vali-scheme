;;; requirement: Deeply nested or very long data and code cannot overflow the native stack
;;; kind: scheme

;; A stack overflow aborts the whole process, which a host cannot catch,
;; so no input may be able to cause one. Each of these passes recursed on
;; the depth of its input, or on the length of a list through its `cdr`s:
;; converting a heap value to a datum for `eval`, the datum's clone and
;; hash, lowering a quoted datum to a constant, building the constant in
;; the heap, and `write`.
;;
;; Tests run on a thread with a 2 MiB stack, which a recursive pass
;; overflows at a depth of a few thousand, so the depths here are far past
;; what would pass by luck. Compiling code nested this deep is not the
;; subject (and is checked at a smaller depth): deeply nested quoted data
;; and long lists are, which a program can build at any size.

(import (scheme base) (scheme write) (scheme read) (scheme eval)
        (srfi 64))

(define depth 50000)

;; ((((...x...)))): `n` lists, each holding the next.
(define (nest-lists n x)
  (let loop ((n n) (x x))
    (if (= n 0) x (loop (- n 1) (list x)))))

;; #(#(...x...)): `n` vectors, each holding the next.
(define (nest-vectors n x)
  (let loop ((n n) (x x))
    (if (= n 0) x (loop (- n 1) (vector x)))))

;; (1 1 1 ...): `n` elements, nested through the `cdr`s.
(define (long-list n)
  (let loop ((n n) (acc '()))
    (if (= n 0) acc (loop (- n 1) (cons 1 acc)))))

;; (if x (if x ... 1 2) 2): `n` `if`s nested in the consequent.
(define (nest-ifs n)
  (let loop ((n n) (body 1))
    (if (= n 0) body (loop (- n 1) `(if #t ,body 2)))))

(define env (environment '(scheme base)))

(define (written x)
  (let ((port (open-output-string)))
    (write x port)
    (get-output-string port)))

(test-begin "deep-data-without-stack-overflow")

;; Printing.
(test-equal (+ (* 2 depth) 2) (string-length (written (nest-lists depth '()))))
(test-equal (+ (* 3 depth) 3) (string-length (written (nest-vectors depth #()))))
(test-equal (+ (* 2 depth) 1) (string-length (written (long-list depth))))

;; Reading what was written, and comparing it.
(define deep (nest-lists depth '()))
(test-assert (equal? deep (read (open-input-string (written deep)))))
(test-assert (equal? deep (nest-lists depth '())))
(test-assert (not (equal? deep (nest-lists depth '(1)))))
(define deep-vector (nest-vectors depth #()))
(test-assert (equal? deep-vector (read (open-input-string (written deep-vector)))))

;; Evaluating a quoted datum compiles it into a constant and builds that in
;; the heap.
(test-assert (equal? deep (eval (list 'quote deep) env)))
(test-assert (equal? deep-vector (eval (list 'quote deep-vector) env)))
(test-equal depth (eval (list 'length (list 'quote (long-list depth))) env))
(test-equal depth
            (eval (list 'vector-length
                        (list 'quote (list->vector (long-list depth))))
                  env))

;; A quoted datum that is deep and long at once.
(define (nest-long n)
  (let loop ((n n) (x '()))
    (if (= n 0) x (loop (- n 1) (list 1 2 x 3)))))
(test-assert (equal? (nest-long 20000)
                     (eval (list 'quote (nest-long 20000)) env)))

;; A transformer returning deep or long data: the expander converts the heap
;; value to syntax, and a quoted datum to a constant.
(define-syntax deep-quote
  (lambda (stx)
    (define (nest n x)
      (let loop ((n n) (x x))
        (if (= n 0) x (loop (- n 1) (list x)))))
    (syntax-case stx ()
      ((k n)
       (datum->syntax #'k (list 'quote (nest (syntax->datum #'n) '())))))))
(define-syntax long-quote
  (lambda (stx)
    (define (long n)
      (let loop ((n n) (acc '()))
        (if (= n 0) acc (loop (- n 1) (cons 1 acc)))))
    (syntax-case stx ()
      ((k n)
       (datum->syntax #'k (list 'quote (long (syntax->datum #'n))))))))
(define-syntax round-trips?
  (lambda (stx)
    (define (nest n x)
      (let loop ((n n) (x x))
        (if (= n 0) x (loop (- n 1) (list x)))))
    (syntax-case stx ()
      ((k n)
       (let ((deep (nest (syntax->datum #'n) '())))
         (datum->syntax
          #'k (equal? deep (syntax->datum (datum->syntax #'k deep)))))))))
(test-assert (equal? deep (deep-quote 50000)))
(test-equal depth (length (long-quote 50000)))
(test-assert (round-trips? 50000))

;; Code nested deeply is expanded and lowered by passes that grow their
;; stack, and a procedure that nests this deep runs.
(test-equal 1 (eval (nest-ifs 5000) env))

;; Dropping it all: nothing holds the structures any more.
(set! deep #f)
(set! deep-vector #f)
(let loop ((i 0))
  (when (< i 3)
    (nest-lists depth '())
    (long-list depth)
    (loop (+ i 1))))
(test-assert #t)

(test-end)
