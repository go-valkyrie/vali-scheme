;;; requirement: (srfi 26) cut and cute are available
;;; found-in: SRFI audit
;;; kind: scheme

(import (scheme base) (srfi 64) (srfi 26))

(test-begin "srfi-26")
(test-assert (memq 'srfi-26 (features)))
(test-equal '(1 2) ((cut list 1 <>) 2))
(test-equal '(1 2 3) ((cut list <> 2 <>) 1 3))
(test-equal '(1 2 3 4) ((cut list 1 <...>) 2 3 4))
(test-equal '(1) ((cut list 1 <...>)))
(test-equal '(2 1) ((cut <> 1 2) (lambda (a b) (list b a))))
(test-equal 6 ((cut + <> <> 3) 1 2))
(test-equal '(a b) (map (cut car <>) '((a 1) (b 2))))
;; cut evaluates its expressions at each call; cute once.
(test-equal '(2 2)
  (let* ((n 0)
         (next (lambda () (set! n (+ n 1)) n))
         (p (cut list (next) <>)))
    (p 'x)
    (list (car (p 'y)) n)))
(test-equal '(1 1)
  (let* ((n 0)
         (next (lambda () (set! n (+ n 1)) n))
         (p (cute list (next) <>)))
    (p 'x)
    (list (car (p 'y)) n)))
(test-equal '(1 2 3) ((cute list 1 <...>) 2 3))
(test-end)
