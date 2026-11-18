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
;;
;; Rebinding one name at every level was cubic, since a reference at depth d
;; had d bindings of its name to choose between. `rebind` nests n `let`s,
;; named `let`s and `lambda`s that all bind `x` and use it in the next
;; level's init, so the pass that picks among a name's bindings shows up too.

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

;; n levels of `(let ((x (+ x 1))) ...)`, then n of a named `let` reusing
;; `loop` and `x`, then n of `((lambda (x) ...) (+ x 1))`.
(define (rebind n)
  (define (nest-let n inner)
    (if (= n 0) inner `(let ((x (+ x 1))) ,(nest-let (- n 1) inner))))
  (define (nest-named n inner)
    (if (= n 0) inner `(let loop ((x (+ x 1))) ,(nest-named (- n 1) inner))))
  (define (nest-lambda n inner)
    (if (= n 0) inner `((lambda (x) ,(nest-lambda (- n 1) inner)) (+ x 1))))
  `((define (g1 x) ,(nest-let n 'x))
    (define (g2 x) ,(nest-named n 'x))
    (define (g3 x) ,(nest-lambda n 'x))))

(define (program n)
  `(begin
     (define (f x) ,(nest 0 n))
     ,@(rebind n)
     (f 5)))
