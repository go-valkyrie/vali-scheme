;;; (srfi 116): immutable lists, written from the SRFI's text over the
;;; runtime's native immutable pair (ADR 0002 #14). An ipair is a type of
;;; its own: `pair?` is false on it and nothing can change it. `ipair`,
;;; `ipair?`, `icar` and `icdr` are the runtime's primitives, which the
;;; compiler open-codes; everything else is Scheme over them.
;;;
;;; Every procedure walks with a loop, never by recursion on the length, so
;;; long ilists are as safe as long lists. Procedures that build an ilist
;;; collect its elements in an ordinary list, last first, and then link
;;; ipairs from the end. Several ilists given to one procedure are walked
;;; to the shortest, as SRFI 1 does. The SRFI makes a dotted ilist an error
;;; for most procedures, and here they do not check: a procedure that walks
;;; an ilist stops at the first object that is not an ipair and ignores it
;;; (`(ifold + 0 (ipair 1 2))` is 1, `(iappend (ipair 1 2) (ilist 3))` is
;;; `(1 3)`). Only `ilength` and `null-ilist?` raise on one. `ilist-copy`,
;;; `list->ilist`, `ilist->list`, `idrop`, `itake-right` and the searches
;;; that return a tail keep it, and `iappend` keeps the last argument as is.
;;;
;;; Written for Vali. Differences from the reference implementation:
;;; - `(iq datum ...)` converts a quoted tree when it runs. Vali has no
;;;   reader syntax for an ipair (a literal would need a constant in the
;;;   distribution format), so it is not a compile-time constant.
;;; - The comparators are SRFI 128's, not SRFI 114's, which Vali does not
;;;   ship. `ipair-comparator` and `ilist-comparator` compare elements with
;;;   the default comparator extended to ipairs, so ilists of ilists agree
;;;   with `equal?` (an ipair is an element or an element's element, and
;;;   sorts after every other object). An ipair inside an ordinary pair or
;;;   vector is still the default comparator's business, which compares it
;;;   with `eqv?`. The default comparator itself is unchanged.

(define-library (srfi 116)
  (import (scheme base)
          (only (vali primitive) ipair ipair? icar icdr)
          (srfi 128))
  (export
   ;; Constructors.
   ipair ilist xipair ipair* make-ilist ilist-tabulate ilist-copy iiota iq
   ;; Predicates.
   ipair? proper-ilist? ilist? dotted-ilist? not-ipair? null-ilist? ilist=
   ;; Selectors.
   icar icdr
         icaar icadr icdar icddr icaaar icaadr icadar icaddr icdaar icdadr
         icddar icdddr icaaaar icaaadr icaadar icaaddr icadaar icadadr
         icaddar icadddr icdaaar icdaadr icdadar icdaddr icddaar icddadr
         icdddar icddddr
   ilist-ref ifirst isecond ithird ifourth ififth isixth iseventh ieighth
   ininth itenth icar+icdr itake idrop ilist-tail itake-right idrop-right
   isplit-at ilast last-ipair
   ;; Miscellaneous.
   ilength iappend iconcatenate ireverse iappend-reverse izip iunzip1 iunzip2
   iunzip3 iunzip4 iunzip5 icount
   ;; Fold, unfold and map.
   ifold ifold-right ipair-fold ipair-fold-right ireduce ireduce-right
   iunfold iunfold-right imap imap-in-order ifor-each ipair-for-each
   iappend-map ifilter-map
   ;; Filtering and partitioning.
   ifilter iremove ipartition
   ;; Searching.
   ifind ifind-tail iany ievery ilist-index itake-while idrop-while ispan
   ibreak imember imemq imemv
   ;; Deletion.
   idelete idelete-duplicates
   ;; Association lists.
   iassoc iassq iassv ialist-cons ialist-delete
   ;; Replacement.
   replace-icar replace-icdr
   ;; Conversion.
   pair->ipair ipair->pair list->ilist ilist->list tree->itree itree->tree
   gtree->itree gtree->tree
   ;; Application.
   iapply
   ;; Comparators.
   ipair-comparator ilist-comparator make-ilist-comparator
   make-improper-ilist-comparator make-ipair-comparator make-icar-comparator
   make-icdr-comparator)
  (begin
    ;; ---- Internal helpers.

    ;; The elements of `il`, last first, up to the first non-ipair.
    (define (%rlist il)
      (let loop ((l il) (acc '()))
        (if (ipair? l) (loop (icdr l) (cons (icar l) acc)) acc)))

    ;; What ends `il`: the empty list for a proper ilist.
    (define (%end il)
      (let loop ((l il))
        (if (ipair? l) (loop (icdr l)) l)))

    ;; The elements of the ordinary list `rev`, last first, linked as
    ;; ipairs in front of `tail`.
    (define (%rev-onto rev tail)
      (let loop ((r rev) (acc tail))
        (if (null? r) acc (loop (cdr r) (ipair (car r) acc)))))

    ;; The elements of `il` as an ordinary list, in order.
    (define (%elements il)
      (let loop ((r (%rlist il)) (acc '()))
        (if (null? r) acc (loop (cdr r) (cons (car r) acc)))))

    (define (%bad who what x) (error (string-append who ": " what) x))

    ;; Whether any of the ilists in `ls` has ended.
    (define (%ended? ls)
      (let loop ((l ls))
        (and (pair? l) (or (not (ipair? (car l))) (loop (cdr l))))))

    (define (%cars ls) (map icar ls))
    (define (%cdrs ls) (map icdr ls))

    ;; The ordinary lists of each position's elements across `ls`, last
    ;; position first: one list per step of a walk over all of them.
    (define (%rtuples ls)
      (let loop ((ls ls) (acc '()))
        (if (%ended? ls) acc (loop (%cdrs ls) (cons (%cars ls) acc)))))

    (define (%reverse l)
      (let loop ((l l) (acc '()))
        (if (null? l) acc (loop (cdr l) (cons (car l) acc)))))

    ;; ---- Constructors.

    (define (ilist . xs) (list->ilist xs))
    (define (xipair d a) (ipair a d))

    (define (ipair* first . rest)
      ;; The last argument is the tail.
      (let ((r (%reverse (cons first rest))))
        (let loop ((r (cdr r)) (acc (car r)))
          (if (null? r) acc (loop (cdr r) (ipair (car r) acc))))))

    (define (make-ilist n . fill)
      (let ((x (if (pair? fill) (car fill) #f)))
        (let loop ((i 0) (acc '()))
          (if (< i n) (loop (+ i 1) (ipair x acc)) acc))))

    (define (ilist-tabulate n proc)
      (let loop ((i (- n 1)) (acc '()))
        (if (< i 0) acc (loop (- i 1) (ipair (proc i) acc)))))

    (define (ilist-copy il)
      (%rev-onto (%rlist il) (%end il)))

    (define (iiota count . rest)
      (unless (and (exact-integer? count) (>= count 0))
        (%bad "iiota" "expected a non-negative count" count))
      (let ((start (if (pair? rest) (car rest) 0))
            (step (if (and (pair? rest) (pair? (cdr rest))) (cadr rest) 1)))
        (let loop ((i (- count 1)) (acc '()))
          (if (< i 0)
              acc
              (loop (- i 1) (ipair (+ start (* i step)) acc))))))

    ;; `(iq datum ...)` is the ilist of its data, quoted, with every pair
    ;; in them an ipair.
    (define-syntax iq
      (syntax-rules ()
        ((_ x ...) (tree->itree '(x ...)))))

    ;; ---- Predicates.

    (define (proper-ilist? x)
      (let loop ((l x))
        (cond ((null? l) #t)
              ((ipair? l) (loop (icdr l)))
              (else #f))))
    (define (ilist? x) (proper-ilist? x))
    (define (dotted-ilist? x) (not (proper-ilist? x)))
    (define (not-ipair? x) (not (ipair? x)))
    (define (null-ilist? il)
      (cond ((ipair? il) #f)
            ((null? il) #t)
            (else (%bad "null-ilist?" "expected an ilist" il))))

    (define (ilist= elt= . ils)
      (define (same? a b)
        (let loop ((a a) (b b))
          (cond ((ipair? a)
                 (and (ipair? b)
                      (elt= (icar a) (icar b))
                      (loop (icdr a) (icdr b))))
                (else (not (ipair? b))))))
      (let loop ((ls ils))
        (or (null? ls)
            (null? (cdr ls))
            (and (same? (car ls) (cadr ls)) (loop (cdr ls))))))

    ;; ---- Selectors.

    (define (icaar x) (icar (icar x)))
    (define (icadr x) (icar (icdr x)))
    (define (icdar x) (icdr (icar x)))
    (define (icddr x) (icdr (icdr x)))
    (define (icaaar x) (icar (icar (icar x))))
    (define (icaadr x) (icar (icar (icdr x))))
    (define (icadar x) (icar (icdr (icar x))))
    (define (icaddr x) (icar (icdr (icdr x))))
    (define (icdaar x) (icdr (icar (icar x))))
    (define (icdadr x) (icdr (icar (icdr x))))
    (define (icddar x) (icdr (icdr (icar x))))
    (define (icdddr x) (icdr (icdr (icdr x))))
    (define (icaaaar x) (icar (icar (icar (icar x)))))
    (define (icaaadr x) (icar (icar (icar (icdr x)))))
    (define (icaadar x) (icar (icar (icdr (icar x)))))
    (define (icaaddr x) (icar (icar (icdr (icdr x)))))
    (define (icadaar x) (icar (icdr (icar (icar x)))))
    (define (icadadr x) (icar (icdr (icar (icdr x)))))
    (define (icaddar x) (icar (icdr (icdr (icar x)))))
    (define (icadddr x) (icar (icdr (icdr (icdr x)))))
    (define (icdaaar x) (icdr (icar (icar (icar x)))))
    (define (icdaadr x) (icdr (icar (icar (icdr x)))))
    (define (icdadar x) (icdr (icar (icdr (icar x)))))
    (define (icdaddr x) (icdr (icar (icdr (icdr x)))))
    (define (icddaar x) (icdr (icdr (icar (icar x)))))
    (define (icddadr x) (icdr (icdr (icar (icdr x)))))
    (define (icdddar x) (icdr (icdr (icdr (icar x)))))
    (define (icddddr x) (icdr (icdr (icdr (icdr x)))))

    (define (idrop il k)
      (let loop ((l il) (k k))
        (if (= k 0) l (loop (icdr l) (- k 1)))))
    (define ilist-tail idrop)

    (define (ilist-ref il k) (icar (idrop il k)))
    (define (ifirst x) (icar x))
    (define (isecond x) (icar (idrop x 1)))
    (define (ithird x) (icar (idrop x 2)))
    (define (ifourth x) (icar (idrop x 3)))
    (define (ififth x) (icar (idrop x 4)))
    (define (isixth x) (icar (idrop x 5)))
    (define (iseventh x) (icar (idrop x 6)))
    (define (ieighth x) (icar (idrop x 7)))
    (define (ininth x) (icar (idrop x 8)))
    (define (itenth x) (icar (idrop x 9)))
    (define (icar+icdr p) (values (icar p) (icdr p)))

    (define (itake il k)
      (let loop ((l il) (k k) (acc '()))
        (if (= k 0)
            (%rev-onto acc '())
            (loop (icdr l) (- k 1) (cons (icar l) acc)))))

    (define (itake-right il k)
      (let loop ((lag il) (lead (idrop il k)))
        (if (ipair? lead) (loop (icdr lag) (icdr lead)) lag)))

    (define (idrop-right il k)
      (let loop ((lag il) (lead (idrop il k)) (acc '()))
        (if (ipair? lead)
            (loop (icdr lag) (icdr lead) (cons (icar lag) acc))
            (%rev-onto acc '()))))

    (define (isplit-at il k) (values (itake il k) (idrop il k)))

    (define (last-ipair il)
      (let loop ((l il))
        (if (ipair? (icdr l)) (loop (icdr l)) l)))
    (define (ilast il) (icar (last-ipair il)))

    ;; ---- Miscellaneous.

    (define (ilength il)
      (let loop ((l il) (n 0))
        (cond ((ipair? l) (loop (icdr l) (+ n 1)))
              ((null? l) n)
              (else (%bad "ilength" "expected a proper ilist" il)))))

    ;; The ilists of the ordinary list `rev`, last first, appended; the
    ;; first of them may be any object, as `iappend`'s last argument.
    (define (%append-reversed rev)
      (if (null? rev)
          '()
          (let loop ((r (cdr rev)) (acc (car rev)))
            (if (null? r)
                acc
                (loop (cdr r) (%rev-onto (%rlist (car r)) acc))))))

    (define (iappend . ils) (%append-reversed (%reverse ils)))

    ;; Without `apply`, which cannot take as many arguments as an ilist
    ;; can hold.
    (define (iconcatenate ils) (%append-reversed (%rlist ils)))

    (define (ireverse il)
      (let loop ((l il) (acc '()))
        (if (ipair? l) (loop (icdr l) (ipair (icar l) acc)) acc)))

    (define (iappend-reverse rev tail)
      (let loop ((l rev) (acc tail))
        (if (ipair? l) (loop (icdr l) (ipair (icar l) acc)) acc)))

    (define (izip il . ils) (apply imap ilist il ils))

    (define (%unzip il n)
      (let loop ((l il) (accs (make-list n '())))
        (if (ipair? l)
            (loop (icdr l)
                  (let inner ((e (icar l)) (accs accs) (out '()))
                    (if (null? accs)
                        (%reverse out)
                        (inner (icdr e) (cdr accs)
                               (cons (cons (icar e) (car accs)) out)))))
            (map (lambda (r) (%rev-onto r '())) accs))))
    (define (iunzip1 il) (car (%unzip il 1)))
    (define (iunzip2 il) (apply values (%unzip il 2)))
    (define (iunzip3 il) (apply values (%unzip il 3)))
    (define (iunzip4 il) (apply values (%unzip il 4)))
    (define (iunzip5 il) (apply values (%unzip il 5)))

    (define (icount pred il . ils)
      (if (null? ils)
          (let loop ((l il) (n 0))
            (if (ipair? l)
                (loop (icdr l) (if (pred (icar l)) (+ n 1) n))
                n))
          (let loop ((r (%reverse (%rtuples (cons il ils)))) (n 0))
            (if (null? r)
                n
                (loop (cdr r) (if (apply pred (car r)) (+ n 1) n))))))

    ;; ---- Fold, unfold and map.

    (define (ifold kons knil il . ils)
      (if (null? ils)
          (let loop ((acc knil) (l il))
            (if (ipair? l) (loop (kons (icar l) acc) (icdr l)) acc))
          (let loop ((acc knil) (ls (cons il ils)))
            (if (%ended? ls)
                acc
                (loop (apply kons (append (%cars ls) (list acc)))
                      (%cdrs ls))))))

    (define (ifold-right kons knil il . ils)
      (if (null? ils)
          (let loop ((r (%rlist il)) (acc knil))
            (if (null? r) acc (loop (cdr r) (kons (car r) acc))))
          (let loop ((r (%rtuples (cons il ils))) (acc knil))
            (if (null? r)
                acc
                (loop (cdr r) (apply kons (append (car r) (list acc))))))))

    (define (ipair-fold kons knil il . ils)
      (if (null? ils)
          (let loop ((acc knil) (l il))
            (if (ipair? l)
                (let ((next (icdr l))) (loop (kons l acc) next))
                acc))
          (let loop ((acc knil) (ls (cons il ils)))
            (if (%ended? ls)
                acc
                (let ((next (%cdrs ls)))
                  (loop (apply kons (append ls (list acc))) next))))))

    (define (ipair-fold-right kons knil il . ils)
      (let loop ((ls (cons il ils)) (cells '()))
        (if (%ended? ls)
            (let fold ((cells cells) (acc knil))
              (if (null? cells)
                  acc
                  (fold (cdr cells) (apply kons (append (car cells) (list acc))))))
            (loop (%cdrs ls) (cons ls cells)))))

    (define (ireduce f ridentity il)
      (if (ipair? il)
          (ifold f (icar il) (icdr il))
          ridentity))

    (define (ireduce-right f ridentity il)
      (let ((r (%rlist il)))
        (if (null? r)
            ridentity
            (let loop ((r (cdr r)) (acc (car r)))
              (if (null? r) acc (loop (cdr r) (f (car r) acc)))))))

    (define (iunfold stop? mapper successor seed . tail-gen)
      (let loop ((seed seed) (acc '()))
        (if (stop? seed)
            (%rev-onto acc (if (pair? tail-gen) ((car tail-gen) seed) '()))
            (loop (successor seed) (cons (mapper seed) acc)))))

    (define (iunfold-right stop? mapper successor seed . tail)
      (let loop ((seed seed) (acc (if (pair? tail) (car tail) '())))
        (if (stop? seed)
            acc
            (loop (successor seed) (ipair (mapper seed) acc)))))

    ;; The results of `proc` over the ilists, last first.
    (define (%map-results proc il ils)
      (if (null? ils)
          (let loop ((l il) (acc '()))
            (if (ipair? l) (loop (icdr l) (cons (proc (icar l)) acc)) acc))
          (let loop ((ls (cons il ils)) (acc '()))
            (if (%ended? ls)
                acc
                (let ((args (%cars ls)))
                  (loop (%cdrs ls) (cons (apply proc args) acc)))))))

    (define (imap proc il . ils) (%rev-onto (%map-results proc il ils) '()))
    (define imap-in-order imap)

    (define (ifor-each proc il . ils)
      (if (null? ils)
          (let loop ((l il))
            (when (ipair? l) (proc (icar l)) (loop (icdr l))))
          (let loop ((ls (cons il ils)))
            (unless (%ended? ls)
              (let ((args (%cars ls)))
                (apply proc args)
                (loop (%cdrs ls)))))))

    (define (ipair-for-each proc il . ils)
      (let loop ((ls (cons il ils)))
        (unless (%ended? ls)
          (let ((next (%cdrs ls)))
            (apply proc ls)
            (loop next)))))

    (define (iappend-map proc il . ils)
      (%append-reversed (%map-results proc il ils)))

    (define (ifilter-map proc il . ils)
      (let loop ((r (%map-results proc il ils)) (acc '()))
        (cond ((null? r) (%rev-onto (%reverse acc) '()))
              ((car r) (loop (cdr r) (cons (car r) acc)))
              (else (loop (cdr r) acc)))))

    ;; ---- Filtering and partitioning.

    (define (ifilter pred il)
      (let loop ((l il) (acc '()))
        (cond ((not (ipair? l)) (%rev-onto acc '()))
              ((pred (icar l)) (loop (icdr l) (cons (icar l) acc)))
              (else (loop (icdr l) acc)))))

    (define (iremove pred il) (ifilter (lambda (x) (not (pred x))) il))

    (define (ipartition pred il)
      (let loop ((l il) (in '()) (out '()))
        (cond ((not (ipair? l)) (values (%rev-onto in '()) (%rev-onto out '())))
              ((pred (icar l)) (loop (icdr l) (cons (icar l) in) out))
              (else (loop (icdr l) in (cons (icar l) out))))))

    ;; ---- Searching.

    (define (ifind-tail pred il)
      (let loop ((l il))
        (cond ((not (ipair? l)) #f)
              ((pred (icar l)) l)
              (else (loop (icdr l))))))

    (define (ifind pred il)
      (let ((tail (ifind-tail pred il)))
        (and tail (icar tail))))

    (define (iany pred il . ils)
      (if (null? ils)
          (let loop ((l il))
            (cond ((not (ipair? l)) #f)
                  ((not (ipair? (icdr l))) (pred (icar l)))
                  ((pred (icar l)))
                  (else (loop (icdr l)))))
          (let loop ((ls (cons il ils)))
            (if (%ended? ls)
                #f
                (let ((next (%cdrs ls)))
                  (if (%ended? next)
                      (apply pred (%cars ls))
                      (or (apply pred (%cars ls)) (loop next))))))))

    (define (ievery pred il . ils)
      (if (null? ils)
          (let loop ((l il))
            (cond ((not (ipair? l)) #t)
                  ((not (ipair? (icdr l))) (pred (icar l)))
                  ((pred (icar l)) (loop (icdr l)))
                  (else #f)))
          (let loop ((ls (cons il ils)))
            (if (%ended? ls)
                #t
                (let ((next (%cdrs ls)))
                  (if (%ended? next)
                      (apply pred (%cars ls))
                      (and (apply pred (%cars ls)) (loop next))))))))

    (define (ilist-index pred il . ils)
      (let loop ((ls (cons il ils)) (i 0))
        (cond ((%ended? ls) #f)
              ((apply pred (%cars ls)) i)
              (else (loop (%cdrs ls) (+ i 1))))))

    (define (itake-while pred il)
      (let loop ((l il) (acc '()))
        (if (and (ipair? l) (pred (icar l)))
            (loop (icdr l) (cons (icar l) acc))
            (%rev-onto acc '()))))

    (define (idrop-while pred il)
      (let loop ((l il))
        (if (and (ipair? l) (pred (icar l))) (loop (icdr l)) l)))

    (define (ispan pred il)
      (let loop ((l il) (acc '()))
        (if (and (ipair? l) (pred (icar l)))
            (loop (icdr l) (cons (icar l) acc))
            (values (%rev-onto acc '()) l))))

    (define (ibreak pred il) (ispan (lambda (x) (not (pred x))) il))

    (define (imember x il . rest)
      (let ((= (if (pair? rest) (car rest) equal?)))
        (ifind-tail (lambda (y) (= x y)) il)))
    (define (imemq x il) (ifind-tail (lambda (y) (eq? x y)) il))
    (define (imemv x il) (ifind-tail (lambda (y) (eqv? x y)) il))

    ;; ---- Deletion.

    (define (idelete x il . rest)
      (let ((= (if (pair? rest) (car rest) equal?)))
        (ifilter (lambda (y) (not (= x y))) il)))

    (define (idelete-duplicates il . rest)
      (let ((= (if (pair? rest) (car rest) equal?)))
        (let loop ((l il) (kept '()))
          (cond ((not (ipair? l)) (%rev-onto kept '()))
                ((let seen ((k kept))
                   (and (pair? k) (or (= (car k) (icar l)) (seen (cdr k)))))
                 (loop (icdr l) kept))
                (else (loop (icdr l) (cons (icar l) kept)))))))

    ;; ---- Association lists.

    (define (iassoc key alist . rest)
      (let ((= (if (pair? rest) (car rest) equal?)))
        (ifind (lambda (entry) (= key (icar entry))) alist)))
    (define (iassq key alist) (iassoc key alist eq?))
    (define (iassv key alist) (iassoc key alist eqv?))
    (define (ialist-cons key datum alist) (ipair (ipair key datum) alist))
    (define (ialist-delete key alist . rest)
      (let ((= (if (pair? rest) (car rest) equal?)))
        (ifilter (lambda (entry) (not (= key (icar entry)))) alist)))

    ;; ---- Replacement.

    (define (replace-icar ip obj) (ipair obj (icdr ip)))
    (define (replace-icdr ip obj) (ipair (icar ip) obj))

    ;; ---- Conversion.

    (define (pair->ipair p) (ipair (car p) (cdr p)))
    (define (ipair->pair ip) (cons (icar ip) (icdr ip)))

    (define (list->ilist l)
      (let loop ((l l) (acc '()))
        (if (pair? l)
            (loop (cdr l) (cons (car l) acc))
            (%rev-onto acc l))))

    (define (ilist->list il)
      (let loop ((l il) (acc '()))
        (if (ipair? l)
            (loop (icdr l) (cons (icar l) acc))
            (let back ((r acc) (out l))
              (if (null? r) out (back (cdr r) (cons (car r) out)))))))

    ;; Converts the trees of `x` whose spine `spine?` accepts, to ipairs or
    ;; to pairs as `make` and the accessors say; a car is converted by
    ;; recursion, a cdr by iteration, so a long list does not nest.
    (define (%convert x spine? first rest make)
      (let walk ((x x))
        (if (spine? x)
            (let loop ((l x) (acc '()))
              (if (spine? l)
                  (loop (rest l) (cons (walk (first l)) acc))
                  (let back ((r acc) (out (walk l)))
                    (if (null? r) out (back (cdr r) (make (car r) out))))))
            x)))

    (define (tree->itree x) (%convert x pair? car cdr ipair))
    (define (itree->tree x) (%convert x ipair? icar icdr cons))
    (define (%either? x) (or (pair? x) (ipair? x)))
    (define (%first x) (if (pair? x) (car x) (icar x)))
    (define (%rest x) (if (pair? x) (cdr x) (icdr x)))
    (define (gtree->itree x) (%convert x %either? %first %rest ipair))
    (define (gtree->tree x) (%convert x %either? %first %rest cons))

    ;; ---- Application.

    (define (iapply proc . args)
      (let ((r (%reverse args)))
        (apply proc (append (%reverse (cdr r)) (ilist->list (car r))))))

    ;; ---- Comparators.

    (define %default (make-default-comparator))

    (define (%mix h1 h2) (modulo (+ (* 31 h1) h2) (hash-bound)))

    (define (make-ipair-comparator icar-comparator icdr-comparator)
      (make-comparator
       (lambda (x)
         (and (ipair? x)
              (comparator-test-type icar-comparator (icar x))
              (comparator-test-type icdr-comparator (icdr x))))
       (lambda (a b)
         (and (=? icar-comparator (icar a) (icar b))
              (=? icdr-comparator (icdr a) (icdr b))))
       (lambda (a b)
         (if (=? icar-comparator (icar a) (icar b))
             (<? icdr-comparator (icdr a) (icdr b))
             (<? icar-comparator (icar a) (icar b))))
       (lambda (x)
         (%mix (comparator-hash icar-comparator (icar x))
               (comparator-hash icdr-comparator (icdr x))))))

    (define (make-icar-comparator comparator)
      (make-comparator
       ipair?
       (lambda (a b) (=? comparator (icar a) (icar b)))
       (lambda (a b) (<? comparator (icar a) (icar b)))
       (lambda (x) (comparator-hash comparator (icar x)))))

    (define (make-icdr-comparator comparator)
      (make-comparator
       ipair?
       (lambda (a b) (=? comparator (icdr a) (icdr b)))
       (lambda (a b) (<? comparator (icdr a) (icdr b)))
       (lambda (x) (comparator-hash comparator (icdr x)))))

    (define (make-ilist-comparator element-comparator)
      (make-list-comparator element-comparator proper-ilist? null? icar icdr))

    ;; Ordered: the empty list, then ipairs, then every other object.
    (define (%rank x) (cond ((null? x) 0) ((ipair? x) 1) (else 2)))

    ;; The improper ilist comparator of the SRFI: the empty list precedes
    ;; all ipairs, which precede all other objects. Ipairs compare as if
    ;; by `(make-ipair-comparator c c)` for the comparator `c` of the
    ;; elements, the tail included; other objects compare with `c`.
    (define (make-improper-ilist-comparator c)
      (make-comparator
       (lambda (x) #t)
       (lambda (a b)
         (let loop ((a a) (b b))
           (let ((r (%rank a)))
             (and (= r (%rank b))
                  (case r
                    ((0) #t)
                    ((1) (and (=? c (icar a) (icar b))
                              (loop (icdr a) (icdr b))))
                    (else (=? c a b)))))))
       (lambda (a b)
         (let loop ((a a) (b b))
           (let ((ra (%rank a)) (rb (%rank b)))
             (cond ((< ra rb) #t)
                   ((> ra rb) #f)
                   ((= ra 0) #f)
                   ((= ra 1)
                    (if (=? c (icar a) (icar b))
                        (loop (icdr a) (icdr b))
                        (<? c (icar a) (icar b))))
                   (else (<? c a b))))))
       (lambda (x)
         (let loop ((x x) (h 0) (n 64))
           (cond ((null? x) h)
                 ((and (ipair? x) (> n 0))
                  (loop (icdr x) (%mix h (comparator-hash c (icar x)))
                        (- n 1)))
                 ((ipair? x) h)
                 (else (%mix h (comparator-hash c x))))))))

    ;; The default comparator, extended to ipairs at any depth: what
    ;; `ipair-comparator` and `ilist-comparator` compare elements with, so
    ;; they agree with `equal?`. An ipair sorts after every other object.
    (define (%element=? a b)
      (cond ((ipair? a) (and (ipair? b) (=? ipair-comparator a b)))
            ((ipair? b) #f)
            (else (=? %default a b))))
    (define (%element<? a b)
      (cond ((ipair? a) (and (ipair? b) (<? ipair-comparator a b)))
            ((ipair? b) #t)
            (else (<? %default a b))))
    ;; Like SRFI 128's list comparator, it looks at no more than 64 elements
    ;; of a spine, so hashing a long ilist is cheap and does not nest.
    (define (%element-hash x)
      (if (ipair? x)
          (let loop ((x x) (h 0) (n 64))
            (cond ((and (ipair? x) (> n 0))
                   (loop (icdr x) (%mix h (%element-hash (icar x))) (- n 1)))
                  ((ipair? x) h)
                  (else (%mix h (%element-hash x)))))
          (comparator-hash %default x)))
    (define %element-comparator
      (make-comparator #t %element=? %element<? %element-hash))

    (define ipair-comparator
      (make-ipair-comparator %element-comparator %element-comparator))
    (define ilist-comparator (make-ilist-comparator %element-comparator))))
