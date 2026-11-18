;;; requirement: The reader handles R7RS's lexical corner cases, and read takes one datum at a time from a port
;;; kind: scheme

;; Reading from a port takes the next datum and no more: the port is left
;; just after it, `#!fold-case` carries on to later reads, and an error
;; leaves the port where it was. Deep nesting reads without overflowing
;; the native stack, and a datum label must stand for some datum.

(import (scheme base) (scheme read) (scheme char) (srfi 64))

(define (read-string-datum s) (read (open-input-string s)))

(define (read-all s)
  (let ((p (open-input-string s)))
    (let loop ((acc '()))
      (let ((d (read p)))
        (if (eof-object? d) (reverse acc) (loop (cons d acc)))))))

(test-begin "reader-corner-cases")

;; One datum at a time.
(test-equal '((a . b) 42 "s" #(1) c) (read-all "(a . b) 42 \"s\" #(1) c"))
(test-equal '((a) b) (read-all "(a)b"))
(let ((p (open-input-string "abc def")))
  (test-eq 'abc (read p))
  (test-eqv #\space (read-char p))
  (test-eq 'def (read p))
  (test-assert (eof-object? (read p)))
  (test-assert (eof-object? (read p))))
;; Nothing after the datum is looked at, even text that would not read.
(test-eq 'a (read-string-datum "a #| never closed"))
(test-eq 'a (read-string-datum "a )"))
;; Only trivia: the end of the file.
(test-assert (eof-object? (read-string-datum "  ; a comment\n #| block |# #;(a b) ")))
(test-assert (eof-object? (read-string-datum "")))

;; #!fold-case lasts until #!no-fold-case, across reads.
(test-equal '(abc def GHI) (read-all "#!fold-case ABC DEF #!no-fold-case GHI"))
(test-eqv #\space (read-string-datum "#!fold-case #\\SPACE"))
(test-error (read-string-datum "#\\SPACE"))
(test-equal '|ABC| (read-string-datum "#!fold-case |ABC|"))

;; An error leaves the port where it was.
(let ((p (open-input-string "(a . ) b")))
  (test-error (read p))
  (test-eqv #\( (read-char p)))
(test-error (read-string-datum ")"))
(test-error (read-string-datum "(1 2"))
(test-error (read-string-datum "(a]"))
(test-error (read-string-datum "( . a)"))
(test-error (read-string-datum "(a . b c)"))
(test-error (read-string-datum "#(a . b)"))
(test-error (read-string-datum "'"))
(test-error (read-string-datum "#;"))
(test-error (read-string-datum "1+"))
(test-error (read-string-datum "#q"))

;; Brackets, comments and abbreviations.
(test-equal '(a b) (read-string-datum "[a b]"))
(test-eq 'x (read-string-datum "#|a #| nested |# b|# x"))
(test-eq 'c (read-string-datum "#; #; a b c"))
(test-equal ''a (read-string-datum "'a"))
(test-equal '(quasiquote (a (unquote b) (unquote-splicing c)))
            (read-string-datum "`(a ,b ,@c)"))

;; Identifiers.
(test-equal '(... .. +.a -a +a ->x a.b)
            (read-all "... .. +.a -a +a ->x a.b"))
(test-equal "a b" (symbol->string (read-string-datum "|a b|")))
(test-equal "A" (symbol->string (read-string-datum "|\\x41;|")))
(test-equal "" (symbol->string (read-string-datum "||")))

;; Characters and strings.
(test-equal '(#\A #\space #\x #\( #\x3bb)
            (read-all "#\\x41 #\\space #\\x #\\( #\\x3bb"))
(test-error (read-string-datum "#\\x110000"))
(test-error (read-string-datum "#\\xD800"))
(test-error (read-string-datum "#\\bell"))
(test-equal "aAb" (read-string-datum "\"a\\x41;b\""))
(test-equal "ab" (read-string-datum "\"a\\\n   b\""))
(test-error (read-string-datum "\"\\xD800;\""))
(test-error (read-string-datum "\"\\q\""))

;; Booleans and bytevectors.
(test-equal '(#t #t #f #f) (read-all "#t #true #f #false"))
(test-equal #u8(0 1 255) (read-string-datum "#u8(0 1 255)"))
(test-error (read-string-datum "#u8(256)"))
(test-error (read-string-datum "#u8(1.0)"))

;; Datum labels.
(let ((x (read-string-datum "#0=(a . #0#)")))
  (test-eq x (cdr x)))
(let ((x (read-string-datum "#0=#(1 #0#)")))
  (test-eq x (vector-ref x 1)))
(let ((x (read-string-datum "(#1=(b) #1#)")))
  (test-eq (car x) (cadr x)))
(test-error (read-string-datum "#1#"))
(test-error (read-string-datum "(#0=a #0=b)"))
(test-error (read-string-datum "#99999999999999999999999=a"))
;; A label must stand for some datum, not only for a label being defined.
(test-error (read-string-datum "#0=#0#"))
(test-error (read-string-datum "#0=#1=#0#"))

;; Deep nesting reads without overflowing the native stack.
(define deep 100000)
(define (depth d)
  (let loop ((d d) (k 0))
    (if (pair? d) (loop (car d) (+ k 1)) k)))
(test-eqv deep
          (depth (read-string-datum
                  (string-append (make-string deep #\() "x"
                                 (make-string deep #\))))))
(test-eqv deep
          (let loop ((d (read-string-datum
                         (string-append (make-string deep #\') "x")))
                     (k 0))
            (if (pair? d) (loop (cadr d) (+ k 1)) k)))

(test-end)
