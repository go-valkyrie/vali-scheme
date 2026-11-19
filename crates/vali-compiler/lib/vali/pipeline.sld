;;; (vali pipeline): the procedures behind the macros of (srfi 197). A
;;; transformer can call procedures only from the libraries it imports,
;;; not from the one that defines it, so they live here.

(define-library (vali pipeline)
  (import (scheme base))
  (export %elements %build %leading %placeholder? %ellipsis? %fresh
          %fill %scan %chain)
  (begin
    ;; The elements of a syntax list, as a list of syntax objects.
    (define (%elements s)
      (syntax-case s ()
        (() '())
        ((a . rest) (cons (syntax a) (%elements (syntax rest))))))

    ;; A syntax list of the syntax objects in `l`, ending in `tail`.
    (define (%build l tail)
      (if (null? l)
          tail
          (with-syntax ((h (car l)) (t (%build (cdr l) tail)))
            (syntax (h . t)))))

    ;; Takes up to `max` identifiers off the front of a syntax list:
    ;; (values identifiers rest).
    (define (%leading s max)
      (let loop ((s s) (ids '()))
        (syntax-case s ()
          ((a . rest)
           (and (< (length ids) max) (identifier? (syntax a)))
           (loop (syntax rest) (cons (syntax a) ids)))
          (_ (values (reverse ids) s)))))

    (define (%named? id name)
      (and (identifier? id) (eq? (syntax->datum id) name)))

    ;; Predicates for the placeholder and the ellipsis, given the leading
    ;; identifiers (a custom one matches only the identifier itself).
    (define (%placeholder? ids)
      (if (pair? ids)
          (lambda (e) (and (identifier? e) (bound-identifier=? e (car ids))))
          (lambda (e) (%named? e '_))))

    (define (%ellipsis? ids)
      (if (and (pair? ids) (pair? (cdr ids)))
          (lambda (e) (and (identifier? e) (bound-identifier=? e (cadr ids))))
          (lambda (e) (%named? e '...))))

    (define (%fresh like)
      (car (generate-temporaries (list like))))

    ;; A step with each placeholder replaced by `value`.
    (define (%fill step placeholder? value)
      (%build (map (lambda (e) (if (placeholder? e) value e))
                   (%elements step))
              (syntax ())))

    ;; A step's parts: (values call formals). Each placeholder becomes a
    ;; fresh variable of the formals, and a placeholder with the ellipsis
    ;; after it, last, the rest of the formals, in which case the call
    ;; applies its procedure to the arguments and the rest.
    (define (%scan step placeholder? ellipsis?)
      (let loop ((es (%elements step)) (out '()) (vars '()))
        (cond ((null? es)
               (values (%build (reverse out) (syntax ()))
                       (%build (reverse vars) (syntax ()))))
              ((and (placeholder? (car es))
                    (pair? (cdr es))
                    (ellipsis? (cadr es))
                    (null? (cddr es)))
               (let ((rest (%fresh (car es))))
                 (values (with-syntax ((c (%build (reverse (cons rest out)) (syntax ()))))
                           (syntax (apply . c)))
                         (%build (reverse vars) rest))))
              ((placeholder? (car es))
               (let ((v (%fresh (car es))))
                 (loop (cdr es) (cons v out) (cons v vars))))
              (else (loop (cdr es) (cons (car es) out) vars)))))

    ;; `previous`, passed through the `steps`, each binding the values of
    ;; the one before to its placeholders: let*-values.
    (define (%chain previous steps placeholder? ellipsis?)
      (let loop ((steps steps) (previous previous) (bindings '()))
        (if (null? steps)
            (if (null? bindings)
                previous
                (with-syntax ((bs (%build (reverse bindings) (syntax ())))
                              (last previous))
                  (syntax (let*-values bs last))))
            (let-values (((call formals) (%scan (car steps) placeholder? ellipsis?)))
              (with-syntax ((f formals) (p previous))
                (loop (cdr steps) call (cons (syntax (f p)) bindings)))))))))
