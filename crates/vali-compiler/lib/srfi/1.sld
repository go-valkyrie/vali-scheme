;;; (srfi 1): list library, written from the SRFI's text.
;;;
;;; The procedures R7RS already has are (scheme base)'s and (scheme cxr)'s,
;;; re-exported; base's map and for-each stop at the shortest list, as SRFI
;;; 1 asks. The linear-update forms (take!, append!, filter! and the rest)
;;; are their pure counterparts, which SRFI 1 allows: they may, not must,
;;; reuse their arguments. Procedures taking several lists stop at the
;;; shortest, so any of them may be circular as long as one is finite.

(define-library (srfi 1)
  (import (scheme base) (scheme cxr))
  (export
   ;; Constructors.
   cons list xcons cons* make-list list-tabulate list-copy circular-list iota
   ;; Predicates.
   pair? null? proper-list? circular-list? dotted-list? not-pair? null-list?
   list=
   ;; Selectors.
   car cdr caar cadr cdar cddr caaar caadr cadar caddr cdaar cdadr cddar
   cdddr caaaar caaadr caadar caaddr cadaar cadadr caddar cadddr cdaaar
   cdaadr cdadar cdaddr cddaar cddadr cdddar cddddr list-ref
   first second third fourth fifth sixth seventh eighth ninth tenth
   car+cdr take drop take-right drop-right take! drop-right! split-at
   split-at! last last-pair
   ;; Miscellaneous.
   length length+ append concatenate reverse append! concatenate! reverse!
   append-reverse append-reverse! zip unzip1 unzip2 unzip3 unzip4 unzip5
   count
   ;; Folding, unfolding and mapping.
   fold unfold pair-fold reduce fold-right unfold-right pair-fold-right
   reduce-right append-map append-map! pair-for-each filter-map map-in-order
   map for-each
   ;; Filtering and partitioning.
   filter partition remove filter! partition! remove!
   ;; Searching.
   member memq memv find find-tail any every list-index take-while
   drop-while take-while! span break span! break!
   ;; Deleting.
   delete delete-duplicates delete! delete-duplicates!
   ;; Association lists.
   assoc assq assv alist-cons alist-copy alist-delete alist-delete!
   ;; Sets as lists.
   lset<= lset= lset-adjoin lset-union lset-union! lset-intersection
   lset-intersection! lset-difference lset-difference! lset-xor lset-xor!
   lset-diff+intersection lset-diff+intersection!
   ;; Side effects.
   set-car! set-cdr!)
  (begin
    ;; Several lists at once: the cars of `lists`, or #f once one has run
    ;; out, and their cdrs.
    (define (%cars lists)
      (let loop ((lists lists) (cars '()))
        (cond ((null? lists) (reverse cars))
              ((pair? (car lists)) (loop (cdr lists) (cons (caar lists) cars)))
              (else #f))))

    (define (%cdrs lists)
      (map cdr lists))

    ;; Constructors.

    (define (xcons d a) (cons a d))

    (define (cons* first . rest)
      (if (null? rest)
          first
          (let loop ((x first) (rest rest) (acc '()))
            (if (null? rest)
                (append-reverse acc x)
                (loop (car rest) (cdr rest) (cons x acc))))))

    (define (list-tabulate n init)
      (let loop ((i (- n 1)) (acc '()))
        (if (< i 0) acc (loop (- i 1) (cons (init i) acc)))))

    (define (circular-list x . rest)
      (let ((l (cons x rest)))
        (set-cdr! (last-pair l) l)
        l))

    ;; Each element computed from `start` and `step` alone, so inexact
    ;; steps do not accumulate error.
    (define (iota count . opt)
      (let ((start (if (pair? opt) (car opt) 0))
            (step (if (and (pair? opt) (pair? (cdr opt))) (cadr opt) 1)))
        (list-tabulate count (lambda (i) (+ start (* i step))))))

    ;; Predicates. The walks run a hare two pairs at a time and a tortoise
    ;; one, so a circular list is found where they meet.

    ;; What `x` is: 'proper, 'dotted or 'circular.
    (define (%shape x)
      (let loop ((hare x) (tortoise x))
        (cond ((null? hare) 'proper)
              ((not (pair? hare)) 'dotted)
              (else
               (let ((hare (cdr hare)))
                 (cond ((null? hare) 'proper)
                       ((not (pair? hare)) 'dotted)
                       (else
                        (let ((hare (cdr hare)) (tortoise (cdr tortoise)))
                          (if (eq? hare tortoise)
                              'circular
                              (loop hare tortoise))))))))))

    (define (proper-list? x) (eq? (%shape x) 'proper))
    (define (circular-list? x) (eq? (%shape x) 'circular))
    (define (dotted-list? x) (eq? (%shape x) 'dotted))
    (define (not-pair? x) (not (pair? x)))

    (define (null-list? l)
      (cond ((pair? l) #f)
            ((null? l) #t)
            (else (error "null-list?: expected a proper or circular list" l))))

    ;; Each list equal to the next, element by element.
    (define (list= same? . lists)
      (define (two a b)
        (cond ((null? a) (null? b))
              ((null? b) #f)
              (else (and (same? (car a) (car b)) (two (cdr a) (cdr b))))))
      (or (null? lists)
          (let loop ((a (car lists)) (rest (cdr lists)))
            (or (null? rest)
                (and (two a (car rest)) (loop (car rest) (cdr rest)))))))

    ;; Selectors.

    (define (first x) (car x))
    (define (second x) (cadr x))
    (define (third x) (caddr x))
    (define (fourth x) (cadddr x))
    (define (fifth x) (car (cddddr x)))
    (define (sixth x) (cadr (cddddr x)))
    (define (seventh x) (caddr (cddddr x)))
    (define (eighth x) (cadddr (cddddr x)))
    (define (ninth x) (car (cddddr (cddddr x))))
    (define (tenth x) (cadr (cddddr (cddddr x))))

    (define (car+cdr pair) (values (car pair) (cdr pair)))

    (define (take x i)
      (let loop ((x x) (i i) (acc '()))
        (if (= i 0)
            (reverse acc)
            (loop (cdr x) (- i 1) (cons (car x) acc)))))

    (define (drop x i)
      (if (= i 0) x (drop (cdr x) (- i 1))))

    ;; The last `i` elements, shared: `lead` runs `i` pairs ahead of `lag`.
    (define (take-right x i)
      (let loop ((lag x) (lead (drop x i)))
        (if (pair? lead) (loop (cdr lag) (cdr lead)) lag)))

    (define (drop-right x i)
      (let loop ((lag x) (lead (drop x i)) (acc '()))
        (if (pair? lead)
            (loop (cdr lag) (cdr lead) (cons (car lag) acc))
            (reverse acc))))

    (define take! take)
    (define drop-right! drop-right)

    (define (split-at x i) (values (take x i) (drop x i)))
    (define split-at! split-at)

    (define (last-pair x)
      (if (pair? (cdr x)) (last-pair (cdr x)) x))

    (define (last x) (car (last-pair x)))

    ;; Miscellaneous.

    ;; The length of a proper or dotted list, or #f for a circular one.
    (define (length+ x)
      (let loop ((hare x) (tortoise x) (n 0))
        (if (pair? hare)
            (let ((hare (cdr hare)) (n (+ n 1)))
              (if (pair? hare)
                  (let ((hare (cdr hare)) (tortoise (cdr tortoise)) (n (+ n 1)))
                    (and (not (eq? hare tortoise)) (loop hare tortoise n)))
                  n))
            n)))

    ;; Without apply, which a long list of lists would overflow.
    (define (concatenate lists) (reduce-right append '() lists))
    (define concatenate! concatenate)
    (define append! append)
    (define reverse! reverse)

    (define (append-reverse rev-head tail)
      (if (pair? rev-head)
          (append-reverse (cdr rev-head) (cons (car rev-head) tail))
          tail))
    (define append-reverse! append-reverse)

    (define (zip list1 . lists) (apply map list list1 lists))

    (define (unzip1 l) (map car l))
    (define (unzip2 l) (values (map car l) (map cadr l)))
    (define (unzip3 l) (values (map car l) (map cadr l) (map caddr l)))
    (define (unzip4 l)
      (values (map car l) (map cadr l) (map caddr l) (map cadddr l)))
    (define (unzip5 l)
      (values (map car l) (map cadr l) (map caddr l) (map cadddr l)
              (map (lambda (x) (car (cddddr x))) l)))

    (define (count pred list1 . lists)
      (if (null? lists)
          (let loop ((l list1) (n 0))
            (if (pair? l)
                (loop (cdr l) (if (pred (car l)) (+ n 1) n))
                n))
          (let loop ((ls (cons list1 lists)) (n 0))
            (let ((cars (%cars ls)))
              (if cars
                  (loop (%cdrs ls) (if (apply pred cars) (+ n 1) n))
                  n)))))

    ;; Folding, unfolding and mapping.

    (define (fold kons knil list1 . lists)
      (if (null? lists)
          (let loop ((l list1) (acc knil))
            (if (pair? l) (loop (cdr l) (kons (car l) acc)) acc))
          (let loop ((ls (cons list1 lists)) (acc knil))
            (let ((cars (%cars ls)))
              (if cars
                  (loop (%cdrs ls) (apply kons (append cars (list acc))))
                  acc)))))

    ;; From the right, without deepening the stack: the rows, reversed,
    ;; then folded.
    (define (fold-right kons knil list1 . lists)
      (if (null? lists)
          (fold kons knil (reverse list1))
          (let loop ((ls (cons list1 lists)) (rows '()))
            (let ((cars (%cars ls)))
              (if cars
                  (loop (%cdrs ls) (cons cars rows))
                  (fold (lambda (row acc) (apply kons (append row (list acc))))
                        knil rows))))))

    ;; Each tail is taken before `kons` sees the pair, so `kons` may change
    ;; it.
    (define (pair-fold kons knil list1 . lists)
      (if (null? lists)
          (let loop ((l list1) (acc knil))
            (if (pair? l)
                (let ((tail (cdr l))) (loop tail (kons l acc)))
                acc))
          (let loop ((ls (cons list1 lists)) (acc knil))
            (if (%cars ls)
                (let ((tails (%cdrs ls)))
                  (loop tails (apply kons (append ls (list acc)))))
                acc))))

    (define (pair-fold-right kons knil list1 . lists)
      (let loop ((ls (cons list1 lists)) (rows '()))
        (if (%cars ls)
            (loop (%cdrs ls) (cons ls rows))
            (fold (lambda (row acc) (apply kons (append row (list acc))))
                  knil rows))))

    (define (reduce f ridentity l)
      (if (null-list? l) ridentity (fold f (car l) (cdr l))))

    (define (reduce-right f ridentity l)
      (if (null-list? l)
          ridentity
          (let ((rev (reverse l)))
            (fold f (car rev) (cdr rev)))))

    (define (unfold stop? mapper successor seed . tail-gen)
      (let loop ((seed seed) (acc '()))
        (if (stop? seed)
            (append-reverse acc (if (pair? tail-gen) ((car tail-gen) seed) '()))
            (loop (successor seed) (cons (mapper seed) acc)))))

    (define (unfold-right stop? mapper successor seed . tail)
      (let loop ((seed seed) (acc (if (pair? tail) (car tail) '())))
        (if (stop? seed)
            acc
            (loop (successor seed) (cons (mapper seed) acc)))))

    (define (append-map f list1 . lists)
      (concatenate (apply map f list1 lists)))
    (define append-map! append-map)

    (define (pair-for-each proc list1 . lists)
      (let loop ((ls (cons list1 lists)))
        (when (%cars ls)
          (let ((tails (%cdrs ls)))
            (apply proc ls)
            (loop tails)))))

    (define (filter-map f list1 . lists)
      (let loop ((ls (cons list1 lists)) (acc '()))
        (let ((cars (%cars ls)))
          (if cars
              (let ((x (apply f cars)))
                (loop (%cdrs ls) (if x (cons x acc) acc)))
              (reverse acc)))))

    ;; Base's map already goes from left to right.
    (define map-in-order map)

    ;; Filtering and partitioning.

    (define (filter pred l)
      (let loop ((l l) (acc '()))
        (cond ((null? l) (reverse acc))
              ((pred (car l)) (loop (cdr l) (cons (car l) acc)))
              (else (loop (cdr l) acc)))))

    (define (partition pred l)
      (let loop ((l l) (in '()) (out '()))
        (cond ((null? l) (values (reverse in) (reverse out)))
              ((pred (car l)) (loop (cdr l) (cons (car l) in) out))
              (else (loop (cdr l) in (cons (car l) out))))))

    (define (remove pred l) (filter (lambda (x) (not (pred x))) l))

    (define filter! filter)
    (define partition! partition)
    (define remove! remove)

    ;; Searching.

    (define (find pred l)
      (let ((tail (find-tail pred l)))
        (and tail (car tail))))

    (define (find-tail pred l)
      (let loop ((l l))
        (and (pair? l)
             (if (pred (car l)) l (loop (cdr l))))))

    ;; The first true value `pred` returns; it is called on the last
    ;; elements in tail position.
    (define (any pred list1 . lists)
      (if (null? lists)
          (and (pair? list1)
               (let loop ((l list1))
                 (if (pair? (cdr l))
                     (or (pred (car l)) (loop (cdr l)))
                     (pred (car l)))))
          (let loop ((ls (cons list1 lists)))
            (let ((cars (%cars ls)))
              (and cars
                   (let ((rest (%cdrs ls)))
                     (if (%cars rest)
                         (or (apply pred cars) (loop rest))
                         (apply pred cars))))))))

    ;; The last value `pred` returns if all are true, or #t for no
    ;; elements.
    (define (every pred list1 . lists)
      (if (null? lists)
          (or (not (pair? list1))
              (let loop ((l list1))
                (if (pair? (cdr l))
                    (and (pred (car l)) (loop (cdr l)))
                    (pred (car l)))))
          (let loop ((ls (cons list1 lists)))
            (let ((cars (%cars ls)))
              (or (not cars)
                  (let ((rest (%cdrs ls)))
                    (if (%cars rest)
                        (and (apply pred cars) (loop rest))
                        (apply pred cars))))))))

    (define (list-index pred list1 . lists)
      (let loop ((ls (cons list1 lists)) (i 0))
        (let ((cars (%cars ls)))
          (and cars
               (if (apply pred cars) i (loop (%cdrs ls) (+ i 1)))))))

    (define (take-while pred l)
      (let loop ((l l) (acc '()))
        (if (and (pair? l) (pred (car l)))
            (loop (cdr l) (cons (car l) acc))
            (reverse acc))))
    (define take-while! take-while)

    (define (drop-while pred l)
      (let loop ((l l))
        (if (and (pair? l) (pred (car l))) (loop (cdr l)) l)))

    (define (span pred l)
      (let loop ((l l) (acc '()))
        (if (and (pair? l) (pred (car l)))
            (loop (cdr l) (cons (car l) acc))
            (values (reverse acc) l))))
    (define (break pred l) (span (lambda (x) (not (pred x))) l))
    (define span! span)
    (define break! break)

    ;; Deleting. `same?` is called with `x` first, as SRFI 1 says.

    (define (delete x l . same)
      (let ((same? (if (pair? same) (car same) equal?)))
        (remove (lambda (y) (same? x y)) l)))
    (define delete! delete)

    ;; Keeps the first of each set of equal elements; `same?` is called
    ;; with the earlier element first.
    (define (delete-duplicates l . same)
      (let ((same? (if (pair? same) (car same) equal?)))
        (let loop ((l l) (kept '()))
          (cond ((null? l) (reverse kept))
                ((any (lambda (k) (same? k (car l))) kept) (loop (cdr l) kept))
                (else (loop (cdr l) (cons (car l) kept)))))))
    (define delete-duplicates! delete-duplicates)

    ;; Association lists.

    (define (alist-cons key datum alist) (cons (cons key datum) alist))

    (define (alist-copy alist)
      (map (lambda (entry) (cons (car entry) (cdr entry))) alist))

    (define (alist-delete key alist . same)
      (let ((same? (if (pair? same) (car same) equal?)))
        (remove (lambda (entry) (same? key (car entry))) alist)))
    (define alist-delete! alist-delete)

    ;; Sets as lists. `=` is called with an element of an earlier list
    ;; first, as SRFI 1 says.

    (define (%in? same? x l)
      (any (lambda (y) (same? y x)) l))

    (define (lset<= same? . lists)
      (or (null? lists)
          (let loop ((a (car lists)) (rest (cdr lists)))
            (or (null? rest)
                (and (every (lambda (x) (any (lambda (y) (same? x y)) (car rest))) a)
                     (loop (car rest) (cdr rest)))))))

    (define (lset= same? . lists)
      (or (null? lists)
          (let loop ((a (car lists)) (rest (cdr lists)))
            (or (null? rest)
                (let ((b (car rest)))
                  (and (every (lambda (x) (any (lambda (y) (same? x y)) b)) a)
                       (every (lambda (y) (any (lambda (x) (same? x y)) a)) b)
                       (loop b (cdr rest))))))))

    (define (lset-adjoin same? l . elts)
      (fold (lambda (elt acc) (if (%in? same? elt acc) acc (cons elt acc)))
            l elts))

    (define (lset-union same? . lists)
      (reduce (lambda (l acc)
                (cond ((null? l) acc)
                      ((null? acc) l)
                      ((eq? l acc) acc)
                      (else
                       (fold (lambda (elt acc)
                               (if (%in? same? elt acc) acc (cons elt acc)))
                             acc l))))
              '() lists))
    (define lset-union! lset-union)

    (define (lset-intersection same? l . lists)
      (filter (lambda (x)
                (every (lambda (other) (any (lambda (y) (same? x y)) other))
                       lists))
              l))
    (define lset-intersection! lset-intersection)

    (define (lset-difference same? l . lists)
      (filter (lambda (x)
                (not (any (lambda (other) (any (lambda (y) (same? x y)) other))
                          lists)))
              l))
    (define lset-difference! lset-difference)

    ;; Pairwise: what is in one of the two but not the other.
    (define (lset-xor same? . lists)
      (reduce (lambda (b a)
                (append (lset-difference same? a b)
                        (lset-difference (lambda (y x) (same? x y)) b a)))
              '() lists))
    (define lset-xor! lset-xor)

    (define (lset-diff+intersection same? l . lists)
      (partition (lambda (x)
                   (not (any (lambda (other) (any (lambda (y) (same? x y)) other))
                             lists)))
                 l))
    (define lset-diff+intersection! lset-diff+intersection)))
