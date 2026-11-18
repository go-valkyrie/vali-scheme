;;; requirement: Compile time is linear in nesting depth
;;; found-in: vali
;;; kind: performance

;; Compiling a form nested n deep took time quadratic in n (cubic when
;; every level rebound one name). The loader copied the subtree under each
;; level to look for includes, and the expander applied each level's
;; pending scope changes to a child one change at a time and looked an
;; identifier's binding up under every scope it carries.
;;
;; The program nests `let`, `if` and named `let` n deep, each level
;; binding a name of its own and using the ones around it, so a pass that
;; does work per level, or per scope, of every node shows up as growth by
;; sixteen when n grows by four.

(import (scheme base) (srfi 1))

(define (name i j)
  (string->symbol
   (string-append "v" (number->string i) "-" (number->string j))))

;; n nested `let`s, each binding thirty-two names of its own and using the
;; ones around it, with `x` at the bottom. A level costs more than a few
;; microseconds, so a depth that is safe to compile times well.
(define (nest i n)
  (if (= i n)
      'x
      `(let ,(map (lambda (j)
                    `(,(name i j) (+ x ,(if (= i 0) 0 (name (- i 1) j)) ,j)))
                  (iota 32))
         ,(nest (+ i 1) n))))

(define (program n)
  `(begin
     (define (f x) ,(nest 0 n))
     (f 5)))
