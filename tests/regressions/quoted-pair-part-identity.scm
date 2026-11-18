;;; requirement: car and cdr of a quoted pair return the object the pair holds
;;; found-in: vali
;;; kind: scheme

;; The optimiser folded (car '(...)) into a fresh constant, so the result
;; was no longer eq? to the element inside the list.

(import (scheme base) (srfi 64))

(test-begin "quoted-pair-part-identity")
(test-assert (let ((alist '((a . 1) (b . 2))))
               (eq? (assq 'a alist) (car alist))))
(test-equal '((1) (2) (3))
            (let ((l '((1) (2) (3)))) (memq (car l) l)))
(test-assert (let ((l '(1 2 3))) (eq? (cdr l) (cdr l))))
(test-assert (let ((l '("a" "b"))) (eq? (car l) (car l))))
(test-end)
