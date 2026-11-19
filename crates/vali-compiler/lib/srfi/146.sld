;;; (srfi 146): mappings, in Scheme over SRFI 128 comparators.
;;;
;;; A mapping is a persistent weight-balanced tree, ordered by the key
;;; comparator's ordering predicate (Adams' trees, with the balance
;;; parameters Hirai and Yamamoto proved correct: delta 3, ratio 2). Every
;;; update shares what it does not change, so the pure procedures cost
;;; O(log n), `mapping-size` is O(1), and splitting and joining (the range
;;; procedures, `mapping-split` and `mapping-catenate`) are O(log n). The
;;; sample implementation of the SRFI, by Marc Nieper-Wisskirchen (MIT
;;; licence, Copyright (C) 2016, 2017), is over red-black trees; this
;;; follows its interface and semantics, not its code.
;;;
;;; The linear-update procedures (`mapping-set!` and the rest) are the
;;; pure ones: the SRFI allows that, and a persistent tree gains nothing
;;; from mutation. So the ordered constructors (`mapping/ordered`,
;;; `alist->mapping/ordered`, `mapping-unfold/ordered`) and
;;; `mapping-map/monotone` are the general ones, which do not need their
;;; input in order. `mapping-update` and `mapping-search` store the key
;;; they are given as the new key, so a replacement key must be equal to
;;; the old one in the comparator's ordering.
;;;
;;; Mappings of different key comparators are a mistake for every
;;; procedure but `mapping=?` (and the other submapping predicates), which
;;; answer #f for them. (srfi 146 hash) has the hash-table side.
;;;
;;; `mapping-comparator` is NOT registered with `(make-default-comparator)`.
;;; Registering changes what `default-hash` computes for the whole program,
;;; so a table built with the default comparator before the import would
;;; lose its keys, and every hash of structure gets slower. A program that
;;; wants mappings as keys of such a table calls
;;; `(comparator-register-default! mapping-comparator)` itself, before it
;;; makes the table; `make-mapping-comparator` makes a comparator for
;;; mappings as keys or values of any other kind of collection.
;;;
;;; The hash of a mapping is its size and the hashes of its (up to four)
;;; least keys, and a mapping inside it counts by its size only, so hashing
;;; data that contains itself ends; values are left out, which
;;; keeps it consistent with `mapping=?` for any value comparator.

(define-library (srfi 146)
  (import (scheme base) (scheme case-lambda) (srfi 128))
  (export mapping mapping-unfold
          mapping/ordered mapping-unfold/ordered
          mapping? mapping-contains? mapping-empty? mapping-disjoint?
          mapping-ref mapping-ref/default mapping-key-comparator
          mapping-adjoin mapping-adjoin!
          mapping-set mapping-set!
          mapping-replace mapping-replace!
          mapping-delete mapping-delete! mapping-delete-all mapping-delete-all!
          mapping-intern mapping-intern!
          mapping-update mapping-update!
          mapping-update/default mapping-update!/default
          mapping-pop mapping-pop!
          mapping-search mapping-search!
          mapping-size mapping-find mapping-count mapping-any? mapping-every?
          mapping-keys mapping-values mapping-entries
          mapping-map mapping-map->list mapping-for-each mapping-fold
          mapping-filter mapping-filter!
          mapping-remove mapping-remove!
          mapping-partition mapping-partition!
          mapping-copy mapping->alist alist->mapping alist->mapping!
          alist->mapping/ordered alist->mapping/ordered!
          mapping=? mapping<? mapping>? mapping<=? mapping>=?
          mapping-union mapping-intersection mapping-difference mapping-xor
          mapping-union! mapping-intersection! mapping-difference! mapping-xor!
          make-mapping-comparator mapping-comparator
          mapping-min-key mapping-max-key mapping-min-value mapping-max-value
          mapping-min-entry mapping-max-entry
          mapping-key-predecessor mapping-key-successor
          mapping-range= mapping-range< mapping-range> mapping-range<=
          mapping-range>=
          mapping-range=! mapping-range<! mapping-range>! mapping-range<=!
          mapping-range>=!
          mapping-split mapping-split!
          mapping-catenate mapping-catenate!
          mapping-map/monotone mapping-map/monotone!
          mapping-fold/reverse
          comparator?)
  (begin
    ;; ---- The tree
    ;;
    ;; The empty tree is '(). A node holds a key, a value, the number of
    ;; entries below and including it, and two subtrees. `lt` is the key
    ;; comparator's ordering predicate throughout.

    (define-record-type <node>
      (%make-node key value size left right)
      node?
      (key node-key)
      (value node-value)
      (size node-size)
      (left node-left)
      (right node-right))

    (define (tree-size t) (if (null? t) 0 (node-size t)))

    (define (bin k v l r)
      (%make-node k v (+ 1 (tree-size l) (tree-size r)) l r))

    (define %delta 3)
    (define %ratio 2)

    ;; Rebuilds a node whose subtrees were balanced, but one of which may
    ;; have grown or shrunk by one entry (or been joined).
    (define (balance k v l r)
      (let ((sl (tree-size l)) (sr (tree-size r)))
        (cond ((<= (+ sl sr) 1) (bin k v l r))
              ((> sr (* %delta sl)) (rotate-left k v l r))
              ((> sl (* %delta sr)) (rotate-right k v l r))
              (else (bin k v l r)))))

    (define (rotate-left k v l r)
      (let ((rl (node-left r)) (rr (node-right r)))
        (if (< (tree-size rl) (* %ratio (tree-size rr)))
            ;; Single rotation.
            (bin (node-key r) (node-value r) (bin k v l rl) rr)
            ;; Double rotation.
            (bin (node-key rl) (node-value rl)
                 (bin k v l (node-left rl))
                 (bin (node-key r) (node-value r) (node-right rl) rr)))))

    (define (rotate-right k v l r)
      (let ((ll (node-left l)) (lr (node-right l)))
        (if (< (tree-size lr) (* %ratio (tree-size ll)))
            (bin (node-key l) (node-value l) ll (bin k v lr r))
            (bin (node-key lr) (node-value lr)
                 (bin (node-key l) (node-value l) ll (node-left lr))
                 (bin k v (node-right lr) r)))))

    ;; The node holding `k`, or #f.
    (define (tree-lookup lt t k)
      (let loop ((t t))
        (cond ((null? t) #f)
              ((lt k (node-key t)) (loop (node-left t)))
              ((lt (node-key t) k) (loop (node-right t)))
              (else t))))

    ;; Adds `k` and `v`. If `k` is there, `replace?` says whether the new
    ;; key and value replace the old, or the tree is left as it is.
    (define (tree-insert lt t k v replace?)
      (let loop ((t t))
        (cond ((null? t) (bin k v '() '()))
              ((lt k (node-key t))
               (balance (node-key t) (node-value t) (loop (node-left t))
                        (node-right t)))
              ((lt (node-key t) k)
               (balance (node-key t) (node-value t) (node-left t)
                        (loop (node-right t))))
              (replace? (bin k v (node-left t) (node-right t)))
              (else t))))

    (define (tree-min-node t)
      (if (null? (node-left t)) t (tree-min-node (node-left t))))
    (define (tree-max-node t)
      (if (null? (node-right t)) t (tree-max-node (node-right t))))

    (define (tree-delete-min t)
      (if (null? (node-left t))
          (node-right t)
          (balance (node-key t) (node-value t) (tree-delete-min (node-left t))
                   (node-right t))))
    (define (tree-delete-max t)
      (if (null? (node-right t))
          (node-left t)
          (balance (node-key t) (node-value t) (node-left t)
                   (tree-delete-max (node-right t)))))

    ;; Joins two trees of keys in order, where every key of `l` is less than
    ;; every key of `r`, and they are balanced against each other.
    (define (glue l r)
      (cond ((null? l) r)
            ((null? r) l)
            ((> (tree-size l) (tree-size r))
             (let ((m (tree-max-node l)))
               (balance (node-key m) (node-value m) (tree-delete-max l) r)))
            (else
             (let ((m (tree-min-node r)))
               (balance (node-key m) (node-value m) l (tree-delete-min r))))))

    (define (tree-delete lt t k)
      (let loop ((t t))
        (cond ((null? t) '())
              ((lt k (node-key t))
               (balance (node-key t) (node-value t) (loop (node-left t))
                        (node-right t)))
              ((lt (node-key t) k)
               (balance (node-key t) (node-value t) (node-left t)
                        (loop (node-right t))))
              (else (glue (node-left t) (node-right t))))))

    (define (insert-min k v t)
      (if (null? t)
          (bin k v '() '())
          (balance (node-key t) (node-value t) (insert-min k v (node-left t))
                   (node-right t))))
    (define (insert-max k v t)
      (if (null? t)
          (bin k v '() '())
          (balance (node-key t) (node-value t) (node-left t)
                   (insert-max k v (node-right t)))))

    ;; The tree of `l`, `k` and `v`, and `r`, for any sizes of `l` and `r`.
    (define (link k v l r)
      (cond ((null? l) (insert-min k v r))
            ((null? r) (insert-max k v l))
            ((< (* %delta (tree-size l)) (tree-size r))
             (balance (node-key r) (node-value r) (link k v l (node-left r))
                      (node-right r)))
            ((< (* %delta (tree-size r)) (tree-size l))
             (balance (node-key l) (node-value l) (node-left l)
                      (link k v (node-right l) r)))
            (else (bin k v l r))))

    ;; The tree of `l` and `r`, for any sizes.
    (define (link2 l r)
      (cond ((null? l) r)
            ((null? r) l)
            ((< (* %delta (tree-size l)) (tree-size r))
             (balance (node-key r) (node-value r) (link2 l (node-left r))
                      (node-right r)))
            ((< (* %delta (tree-size r)) (tree-size l))
             (balance (node-key l) (node-value l) (node-left l)
                      (link2 (node-right l) r)))
            (else (glue l r))))

    ;; Splits around `k` into the tree of the lesser keys, the node of `k`
    ;; (or #f), and the tree of the greater keys.
    (define (tree-split lt t k)
      (let loop ((t t))
        (cond ((null? t) (values '() #f '()))
              ((lt k (node-key t))
               (let-values (((ll found lr) (loop (node-left t))))
                 (values ll found
                         (link (node-key t) (node-value t) lr (node-right t)))))
              ((lt (node-key t) k)
               (let-values (((rl found rr) (loop (node-right t))))
                 (values (link (node-key t) (node-value t) (node-left t) rl)
                         found rr)))
              (else (values (node-left t) t (node-right t))))))

    (define (tree-fold proc acc t)
      (if (null? t)
          acc
          (tree-fold proc
                     (proc (node-key t) (node-value t)
                           (tree-fold proc acc (node-left t)))
                     (node-right t))))

    (define (tree-fold/reverse proc acc t)
      (if (null? t)
          acc
          (tree-fold/reverse proc
                             (proc (node-key t) (node-value t)
                                   (tree-fold/reverse proc acc (node-right t)))
                             (node-left t))))

    ;; The first node, in key order, whose entry satisfies `pred`, or #f.
    (define (tree-find pred t)
      (and (pair-tree? t)
           (or (tree-find pred (node-left t))
               (and (pred (node-key t) (node-value t)) t)
               (tree-find pred (node-right t)))))
    (define (pair-tree? t) (not (null? t)))

    ;; The tree of the entries of `t` that satisfy `pred`, in order.
    (define (tree-filter pred t)
      (cond ((null? t) '())
            (else
             (let* ((l (tree-filter pred (node-left t)))
                    (keep? (pred (node-key t) (node-value t)))
                    (r (tree-filter pred (node-right t))))
               (if keep?
                   (link (node-key t) (node-value t) l r)
                   (link2 l r))))))

    ;; The greatest key below `k`, or the least above it, as a node or #f.
    (define (tree-predecessor lt t k)
      (let loop ((t t) (best #f))
        (cond ((null? t) best)
              ((lt (node-key t) k) (loop (node-right t) t))
              (else (loop (node-left t) best)))))
    (define (tree-successor lt t k)
      (let loop ((t t) (best #f))
        (cond ((null? t) best)
              ((lt k (node-key t)) (loop (node-left t) t))
              (else (loop (node-right t) best)))))

    ;; ---- Mappings

    (define-record-type <mapping>
      (%make-mapping comparator lt tree)
      mapping?
      (comparator mapping-key-comparator)
      (lt %lt)
      (tree %tree))

    (define (%empty comparator)
      (unless (comparator? comparator)
        (error "not a comparator" comparator))
      (unless (comparator-ordered? comparator)
        (error "the comparator of a mapping needs an ordering predicate"
               comparator))
      (%make-mapping comparator (comparator-ordering-predicate comparator) '()))

    (define (%check who m)
      (unless (mapping? m)
        (error (string-append who ": not a mapping") m)))

    ;; A mapping like `m`, with `tree`.
    (define (%with m tree)
      (%make-mapping (mapping-key-comparator m) (%lt m) tree))

    (define (%check-key m key)
      (comparator-check-type (mapping-key-comparator m) key))

    ;; The arguments of `mapping-adjoin` and `mapping-set`, as pairs.
    (define (%pairs who args)
      (let loop ((args args) (acc '()))
        (cond ((null? args) (reverse acc))
              ((null? (cdr args))
               (error (string-append who ": a key with no value") (car args)))
              (else (loop (cddr args) (cons (cons (car args) (cadr args)) acc))))))

    (define (%insert m key value replace?)
      (%check-key m key)
      (%with m (tree-insert (%lt m) (%tree m) key value replace?)))

    ;; Constructors.

    (define (mapping comparator . args)
      (let loop ((m (%empty comparator)) (ps (%pairs "mapping" args)))
        (if (null? ps)
            m
            (loop (%insert m (caar ps) (cdar ps) #f) (cdr ps)))))

    (define (mapping-unfold stop? mapper successor seed comparator)
      (let loop ((m (%empty comparator)) (seed seed))
        (if (stop? seed)
            m
            (let-values (((key value) (mapper seed)))
              (loop (%insert m key value #f) (successor seed))))))

    (define mapping/ordered mapping)
    (define mapping-unfold/ordered mapping-unfold)

    ;; Predicates.

    (define (mapping-contains? m key)
      (%check "mapping-contains?" m)
      (and (tree-lookup (%lt m) (%tree m) key) #t))

    (define (mapping-empty? m)
      (%check "mapping-empty?" m)
      (null? (%tree m)))

    (define (mapping-disjoint? m1 m2)
      (%check "mapping-disjoint?" m1)
      (%check "mapping-disjoint?" m2)
      (not (tree-find (lambda (k v) (mapping-contains? m2 k)) (%tree m1))))

    ;; Accessors.

    (define mapping-ref
      (case-lambda
        ((m key)
         (mapping-ref m key
                      (lambda () (error "mapping-ref: key not in mapping" key))))
        ((m key failure) (mapping-ref m key failure (lambda (value) value)))
        ((m key failure success)
         (%check "mapping-ref" m)
         (let ((node (tree-lookup (%lt m) (%tree m) key)))
           (if node (success (node-value node)) (failure))))))

    (define (mapping-ref/default m key default)
      (%check "mapping-ref/default" m)
      (let ((node (tree-lookup (%lt m) (%tree m) key)))
        (if node (node-value node) default)))

    ;; Updaters.

    (define (mapping-adjoin m . args)
      (%check "mapping-adjoin" m)
      (let loop ((m m) (ps (%pairs "mapping-adjoin" args)))
        (if (null? ps)
            m
            (loop (%insert m (caar ps) (cdar ps) #f) (cdr ps)))))
    (define mapping-adjoin! mapping-adjoin)

    (define (mapping-set m . args)
      (%check "mapping-set" m)
      (let loop ((m m) (ps (%pairs "mapping-set" args)))
        (if (null? ps)
            m
            (loop (%insert m (caar ps) (cdar ps) #t) (cdr ps)))))
    (define mapping-set! mapping-set)

    (define (mapping-replace m key value)
      (%check "mapping-replace" m)
      (if (tree-lookup (%lt m) (%tree m) key)
          (%insert m key value #t)
          m))
    (define mapping-replace! mapping-replace)

    (define (mapping-delete-all m keys)
      (%check "mapping-delete-all" m)
      (let ((lt (%lt m)))
        (%with m (let loop ((t (%tree m)) (keys keys))
                   (if (null? keys)
                       t
                       (loop (tree-delete lt t (car keys)) (cdr keys)))))))
    (define (mapping-delete m . keys) (mapping-delete-all m keys))
    (define mapping-delete! mapping-delete)
    (define mapping-delete-all! mapping-delete-all)

    (define (mapping-intern m key failure)
      (%check "mapping-intern" m)
      (let ((node (tree-lookup (%lt m) (%tree m) key)))
        (if node
            (values m (node-value node))
            (let ((value (failure)))
              (values (%insert m key value #f) value)))))
    (define mapping-intern! mapping-intern)

    (define mapping-update
      (case-lambda
        ((m key updater)
         (mapping-update m key updater
                         (lambda ()
                           (error "mapping-update: key not in mapping" key))))
        ((m key updater failure)
         (mapping-update m key updater failure (lambda (value) value)))
        ((m key updater failure success)
         (%check "mapping-update" m)
         (let* ((node (tree-lookup (%lt m) (%tree m) key))
                (old (if node (success (node-value node)) (failure))))
           (%insert m key (updater old) #t)))))
    (define mapping-update! mapping-update)

    (define (mapping-update/default m key updater default)
      (mapping-update m key updater (lambda () default)))
    (define mapping-update!/default mapping-update/default)

    (define mapping-pop
      (case-lambda
        ((m)
         (mapping-pop m (lambda ()
                          (error "mapping-pop: the mapping has no association"))))
        ((m failure)
         (%check "mapping-pop" m)
         (if (null? (%tree m))
             (failure)
             (let ((node (tree-min-node (%tree m))))
               (values (%with m (tree-delete-min (%tree m)))
                       (node-key node)
                       (node-value node)))))))
    (define mapping-pop! mapping-pop)

    (define (mapping-search m key failure success)
      (%check "mapping-search" m)
      (let* ((lt (%lt m)) (tree (%tree m))
             (node (tree-lookup lt tree key)))
        (if node
            (success
             (node-key node)
             (node-value node)
             ;; update
             (lambda (new-key new-value obj)
               (%check-key m new-key)
               (values
                (%with m
                       (if (or (lt new-key key) (lt key new-key))
                           (tree-insert lt (tree-delete lt tree key)
                                        new-key new-value #t)
                           (tree-insert lt tree new-key new-value #t)))
                obj))
             ;; remove
             (lambda (obj) (values (%with m (tree-delete lt tree key)) obj)))
            (failure
             ;; insert
             (lambda (value obj) (values (%insert m key value #f) obj))
             ;; ignore
             (lambda (obj) (values m obj))))))
    (define mapping-search! mapping-search)

    ;; The whole mapping.

    (define (mapping-size m)
      (%check "mapping-size" m)
      (tree-size (%tree m)))

    (define (mapping-find pred m failure)
      (%check "mapping-find" m)
      (let ((node (tree-find pred (%tree m))))
        (if node
            (values (node-key node) (node-value node))
            (failure))))

    (define (mapping-count pred m)
      (%check "mapping-count" m)
      (tree-fold (lambda (k v n) (if (pred k v) (+ n 1) n)) 0 (%tree m)))

    (define (mapping-any? pred m)
      (%check "mapping-any?" m)
      (and (tree-find pred (%tree m)) #t))

    (define (mapping-every? pred m)
      (%check "mapping-every?" m)
      (not (tree-find (lambda (k v) (not (pred k v))) (%tree m))))

    (define (mapping-keys m)
      (%check "mapping-keys" m)
      (tree-fold/reverse (lambda (k v acc) (cons k acc)) '() (%tree m)))

    (define (mapping-values m)
      (%check "mapping-values" m)
      (tree-fold/reverse (lambda (k v acc) (cons v acc)) '() (%tree m)))

    (define (mapping-entries m)
      (values (mapping-keys m) (mapping-values m)))

    ;; Mapping and folding.

    (define (mapping-fold proc nil m)
      (%check "mapping-fold" m)
      (tree-fold proc nil (%tree m)))

    (define (mapping-fold/reverse proc nil m)
      (%check "mapping-fold/reverse" m)
      (tree-fold/reverse proc nil (%tree m)))

    (define (mapping-for-each proc m)
      (%check "mapping-for-each" m)
      (tree-fold (lambda (k v acc) (proc k v) acc) #f (%tree m))
      (if #f #f))

    (define (mapping-map->list proc m)
      (%check "mapping-map->list" m)
      (tree-fold/reverse (lambda (k v acc) (cons (proc k v) acc)) '() (%tree m)))

    (define (mapping-map proc comparator m)
      (%check "mapping-map" m)
      (tree-fold (lambda (k v acc)
                   (let-values (((k2 v2) (proc k v)))
                     (%insert acc k2 v2 #t)))
                 (%empty comparator)
                 (%tree m)))
    (define mapping-map/monotone mapping-map)
    (define mapping-map/monotone! mapping-map)

    (define (mapping-filter pred m)
      (%check "mapping-filter" m)
      (%with m (tree-filter pred (%tree m))))
    (define mapping-filter! mapping-filter)

    (define (mapping-remove pred m)
      (mapping-filter (lambda (k v) (not (pred k v))) m))
    (define mapping-remove! mapping-remove)

    (define (mapping-partition pred m)
      (values (mapping-filter pred m) (mapping-remove pred m)))
    (define mapping-partition! mapping-partition)

    ;; Copying and conversion.

    (define (mapping-copy m)
      (%check "mapping-copy" m)
      m)

    (define (mapping->alist m)
      (%check "mapping->alist" m)
      (tree-fold/reverse (lambda (k v acc) (cons (cons k v) acc)) '() (%tree m)))

    (define (alist->mapping! m alist)
      (%check "alist->mapping!" m)
      (let loop ((m m) (alist alist))
        (if (null? alist)
            m
            (loop (%insert m (caar alist) (cdar alist) #f) (cdr alist)))))

    (define (alist->mapping comparator alist)
      (alist->mapping! (%empty comparator) alist))

    (define alist->mapping/ordered alist->mapping)
    (define alist->mapping/ordered! alist->mapping!)

    ;; Submappings. `m1` is a submapping of `m2` when every association
    ;; of `m1` is in `m2`, its values equal in the sense of `comparator`.

    (define (%submapping? comparator m1 m2)
      (and (eq? (mapping-key-comparator m1) (mapping-key-comparator m2))
           (<= (mapping-size m1) (mapping-size m2))
           (not (tree-find
                 (lambda (k v)
                   (let ((node (tree-lookup (%lt m2) (%tree m2) k)))
                     (not (and node (=? comparator v (node-value node))))))
                 (%tree m1)))))

    (define (%chain who test comparator m rest)
      (%check who m)
      (let loop ((m m) (rest rest))
        (or (null? rest)
            (begin (%check who (car rest))
                   (and (test comparator m (car rest))
                        (loop (car rest) (cdr rest)))))))

    (define (%equal? comparator m1 m2)
      (and (%submapping? comparator m1 m2)
           (= (mapping-size m1) (mapping-size m2))))
    (define (%proper? comparator m1 m2)
      (and (%submapping? comparator m1 m2)
           (< (mapping-size m1) (mapping-size m2))))

    (define (mapping=? comparator m . rest)
      (%chain "mapping=?" %equal? comparator m rest))
    (define (mapping<=? comparator m . rest)
      (%chain "mapping<=?" %submapping? comparator m rest))
    (define (mapping<? comparator m . rest)
      (%chain "mapping<?" %proper? comparator m rest))
    (define (mapping>=? comparator m . rest)
      (%chain "mapping>=?" (lambda (c a b) (%submapping? c b a)) comparator m rest))
    (define (mapping>? comparator m . rest)
      (%chain "mapping>?" (lambda (c a b) (%proper? c b a)) comparator m rest))

    ;; Set theory. Only keys are compared, and the first mapping in which
    ;; a key appears supplies its association.

    (define (mapping-union m . rest)
      (%check "mapping-union" m)
      (let loop ((m m) (rest rest))
        (if (null? rest)
            m
            (begin
              (%check "mapping-union" (car rest))
              (loop (tree-fold (lambda (k v acc) (%insert acc k v #f))
                               m (%tree (car rest)))
                    (cdr rest))))))
    (define mapping-union! mapping-union)

    (define (mapping-intersection m . rest)
      (%check "mapping-intersection" m)
      (let loop ((m m) (rest rest))
        (if (null? rest)
            m
            (begin
              (%check "mapping-intersection" (car rest))
              (loop (mapping-filter (lambda (k v) (mapping-contains? (car rest) k))
                                    m)
                    (cdr rest))))))
    (define mapping-intersection! mapping-intersection)

    (define (mapping-difference m . rest)
      (%check "mapping-difference" m)
      (let loop ((m m) (rest rest))
        (if (null? rest)
            m
            (begin
              (%check "mapping-difference" (car rest))
              (loop (mapping-filter
                     (lambda (k v) (not (mapping-contains? (car rest) k)))
                     m)
                    (cdr rest))))))
    (define mapping-difference! mapping-difference)

    (define (mapping-xor m1 m2)
      (%check "mapping-xor" m1)
      (%check "mapping-xor" m2)
      (mapping-union (mapping-difference m1 m2) (mapping-difference m2 m1)))
    (define mapping-xor! mapping-xor)

    ;; Ordered keys.

    (define (%nonempty who m)
      (%check who m)
      (when (null? (%tree m))
        (error (string-append who ": the mapping is empty") m)))

    (define (mapping-min-key m)
      (%nonempty "mapping-min-key" m)
      (node-key (tree-min-node (%tree m))))
    (define (mapping-max-key m)
      (%nonempty "mapping-max-key" m)
      (node-key (tree-max-node (%tree m))))
    (define (mapping-min-value m)
      (%nonempty "mapping-min-value" m)
      (node-value (tree-min-node (%tree m))))
    (define (mapping-max-value m)
      (%nonempty "mapping-max-value" m)
      (node-value (tree-max-node (%tree m))))
    (define (mapping-min-entry m)
      (%nonempty "mapping-min-entry" m)
      (let ((n (tree-min-node (%tree m))))
        (values (node-key n) (node-value n))))
    (define (mapping-max-entry m)
      (%nonempty "mapping-max-entry" m)
      (let ((n (tree-max-node (%tree m))))
        (values (node-key n) (node-value n))))

    (define (mapping-key-predecessor m obj failure)
      (%check "mapping-key-predecessor" m)
      (let ((node (tree-predecessor (%lt m) (%tree m) obj)))
        (if node (node-key node) (failure))))
    (define (mapping-key-successor m obj failure)
      (%check "mapping-key-successor" m)
      (let ((node (tree-successor (%lt m) (%tree m) obj)))
        (if node (node-key node) (failure))))

    ;; The entry for `node`, as a one-entry tree, or the empty tree.
    (define (%single node)
      (if node (bin (node-key node) (node-value node) '() '()) '()))

    (define (mapping-range< m obj)
      (%check "mapping-range<" m)
      (let-values (((l found r) (tree-split (%lt m) (%tree m) obj)))
        (%with m l)))
    (define (mapping-range> m obj)
      (%check "mapping-range>" m)
      (let-values (((l found r) (tree-split (%lt m) (%tree m) obj)))
        (%with m r)))
    (define (mapping-range= m obj)
      (%check "mapping-range=" m)
      (let-values (((l found r) (tree-split (%lt m) (%tree m) obj)))
        (%with m (%single found))))
    (define (mapping-range<= m obj)
      (%check "mapping-range<=" m)
      (let-values (((l found r) (tree-split (%lt m) (%tree m) obj)))
        (%with m (if found (insert-max (node-key found) (node-value found) l) l))))
    (define (mapping-range>= m obj)
      (%check "mapping-range>=" m)
      (let-values (((l found r) (tree-split (%lt m) (%tree m) obj)))
        (%with m (if found (insert-min (node-key found) (node-value found) r) r))))
    (define mapping-range=! mapping-range=)
    (define mapping-range<! mapping-range<)
    (define mapping-range>! mapping-range>)
    (define mapping-range<=! mapping-range<=)
    (define mapping-range>=! mapping-range>=)

    (define (mapping-split m obj)
      (%check "mapping-split" m)
      (let-values (((l found r) (tree-split (%lt m) (%tree m) obj)))
        (let ((l+ (if found (insert-max (node-key found) (node-value found) l) l))
              (r+ (if found (insert-min (node-key found) (node-value found) r) r)))
          (values (%with m l) (%with m l+) (%with m (%single found))
                  (%with m r+) (%with m r)))))
    (define mapping-split! mapping-split)

    (define (mapping-catenate comparator m1 key value m2)
      (%check "mapping-catenate" m1)
      (%check "mapping-catenate" m2)
      (let* ((m (%empty comparator))
             (lt (%lt m))
             (t1 (%tree m1))
             (t2 (%tree m2)))
        (%check-key m key)
        ;; The keys of `m1`, `key` and the keys of `m2` must be in order,
        ;; or the tree would be corrupt for good.
        (unless (and (or (null? t1) (lt (node-key (tree-max-node t1)) key))
                     (or (null? t2) (lt key (node-key (tree-min-node t2)))))
          (error "mapping-catenate: the keys are not in increasing order" key))
        (%with m (link key value t1 t2))))
    (define (mapping-catenate! m1 key value m2)
      (mapping-catenate (mapping-key-comparator m1) m1 key value m2))

    ;; ---- Comparators

    ;; True while a mapping is being hashed: one inside it counts by its
    ;; size only, so hashing data that contains itself ends.
    (define %hashing? #f)

    ;; The lexicographic order of the associations in key order, by key
    ;; and then by value; a mapping that is a prefix of another is less.
    (define (make-mapping-comparator value-comparator)
      (define (equal m1 m2) (mapping=? value-comparator m1 m2))
      (define (less m1 m2)
        (unless (eq? (mapping-key-comparator m1) (mapping-key-comparator m2))
          (error "mappings with different key comparators cannot be ordered"
                 m1 m2))
        (let ((key-lt (%lt m1)) (value-lt (comparator-ordering-predicate value-comparator)))
          (let loop ((a (mapping->alist m1)) (b (mapping->alist m2)))
            (cond ((null? b) #f)
                  ((null? a) #t)
                  ((key-lt (caar a) (caar b)) #t)
                  ((key-lt (caar b) (caar a)) #f)
                  ((value-lt (cdar a) (cdar b)) #t)
                  ((value-lt (cdar b) (cdar a)) #f)
                  (else (loop (cdr a) (cdr b)))))))
      (define (hash m)
        (let ((size (mapping-size m))
              (key-comparator (mapping-key-comparator m)))
          (if (or %hashing? (not (comparator-hashable? key-comparator)))
              size
              (let ((key-hash (comparator-hash-function key-comparator)))
                (dynamic-wind
                 (lambda () (set! %hashing? #t))
                 (lambda ()
                   (let loop ((keys (mapping-keys-up-to m 4)) (sum size))
                     (if (null? keys)
                         sum
                         (loop (cdr keys)
                               (modulo (+ (* sum 31) (key-hash (car keys)))
                                       (expt 2 59))))))
                 (lambda () (set! %hashing? #f)))))))
      (make-comparator mapping? equal
                       (and (comparator-ordered? value-comparator) less)
                       hash))

    ;; The least `n` keys, in order.
    (define (mapping-keys-up-to m n)
      (let ((out '()) (count 0))
        (let walk ((t (%tree m)))
          (when (and (not (null? t)) (< count n))
            (walk (node-left t))
            (when (< count n)
              (set! out (cons (node-key t) out))
              (set! count (+ count 1))
              (walk (node-right t)))))
        (reverse out)))

    (define mapping-comparator
      (make-mapping-comparator (make-default-comparator)))))
