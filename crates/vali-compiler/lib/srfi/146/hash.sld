;;; (srfi 146 hash): hashmaps, in Scheme over (srfi 125) hash tables.
;;;
;;; A hashmap is a record around a hash table made from the key comparator,
;;; or, for a comparator with no hash function, from its equality predicate
;;; when that is `eq?`, `eqv?`, `equal?`, `string=?` or `string-ci=?` (the
;;; SRFI's rule). The SRFI's sample implementation is over a hash array
;;; mapped trie, which is persistent; this is not. The linear-update
;;; procedures (`hashmap-set!` and the rest) change the hash table in
;;; place and return the hashmap, as the SRFI allows, and each pure
;;; procedure is a copy of the hashmap given to its linear-update twin. So
;;; a pure update costs O(n), where the SRFI's is O(log n), and a program
;;; that builds a hashmap by repeated pure updates should use the `!`
;;; forms on a copy it owns, or use (srfi 146) when it can order its keys.
;;;
;;; Associations are visited in no particular order, the same for
;;; `hashmap-keys`, `hashmap-values` and `hashmap-entries`. The names are
;;; those of (srfi 146) with `hashmap` for `mapping`, and without the
;;; ordered procedures. `hashmap-comparator` has a hash function and no
;;; ordering, so a hashmap can be a key of a hash table or a hashmap but
;;; not of a mapping, which needs an ordering. The hash of a hashmap is
;;; its size and the hashes of its keys, and only its size below a depth
;;; of three nested hashmaps, so hashing data that contains itself ends.
;;;
;;; `hashmap-comparator` is NOT registered with `(make-default-comparator)`
;;; (see (srfi 146) for why). A program that wants hashmaps as keys of a
;;; table made with the default comparator calls
;;; `(comparator-register-default! hashmap-comparator)` itself, before it
;;; makes the table.

(define-library (srfi 146 hash)
  (import (scheme base) (scheme case-lambda)
          (srfi 128)
          (srfi 125))
  (export hashmap hashmap-unfold
          hashmap? hashmap-contains? hashmap-empty? hashmap-disjoint?
          hashmap-ref hashmap-ref/default hashmap-key-comparator
          hashmap-adjoin hashmap-adjoin!
          hashmap-set hashmap-set!
          hashmap-replace hashmap-replace!
          hashmap-delete hashmap-delete! hashmap-delete-all hashmap-delete-all!
          hashmap-intern hashmap-intern!
          hashmap-update hashmap-update!
          hashmap-update/default hashmap-update!/default
          hashmap-pop hashmap-pop!
          hashmap-search hashmap-search!
          hashmap-size hashmap-find hashmap-count hashmap-any? hashmap-every?
          hashmap-keys hashmap-values hashmap-entries
          hashmap-map hashmap-map->list hashmap-for-each hashmap-fold
          hashmap-filter hashmap-filter!
          hashmap-remove hashmap-remove!
          hashmap-partition hashmap-partition!
          hashmap-copy hashmap->alist alist->hashmap alist->hashmap!
          hashmap=? hashmap<? hashmap>? hashmap<=? hashmap>=?
          hashmap-union hashmap-intersection hashmap-difference hashmap-xor
          hashmap-union! hashmap-intersection! hashmap-difference! hashmap-xor!
          make-hashmap-comparator hashmap-comparator
          comparator?)
  (begin
    (define-record-type <hashmap>
      (%make-hashmap comparator table)
      hashmap?
      (comparator hashmap-key-comparator)
      (table %table))

    (define (%empty comparator)
      (unless (comparator? comparator)
        (error "not a comparator" comparator))
      (%make-hashmap
       comparator
       (if (comparator-hashable? comparator)
           (make-hash-table comparator)
           ;; SRFI 125 hashes for the predicates the SRFI names, and
           ;; refuses any other.
           (make-hash-table (comparator-equality-predicate comparator)))))

    (define (%check who m)
      (unless (hashmap? m)
        (error (string-append who ": not a hashmap") m)))

    (define (%check-key m key)
      (comparator-check-type (hashmap-key-comparator m) key))

    (define (hashmap-copy m)
      (%check "hashmap-copy" m)
      (%make-hashmap (hashmap-key-comparator m) (hash-table-copy (%table m))))

    ;; The pure form of a linear-update procedure: it runs on a copy.
    (define (%pure! proc m . args)
      (apply proc (hashmap-copy m) args))

    ;; The arguments of `hashmap-adjoin` and `hashmap-set`, as pairs.
    (define (%pairs who args)
      (let loop ((args args) (acc '()))
        (cond ((null? args) (reverse acc))
              ((null? (cdr args))
               (error (string-append who ": a key with no value") (car args)))
              (else (loop (cddr args) (cons (cons (car args) (cadr args)) acc))))))

    (define (%adjoin! m key value)
      (%check-key m key)
      (unless (hash-table-contains? (%table m) key)
        (hash-table-set! (%table m) key value)))

    ;; Sets the value of `key`, and the key held is `key`: a table that
    ;; holds an equal key would keep that one.
    (define (%set! m key value)
      (%check-key m key)
      (hash-table-delete! (%table m) key)
      (hash-table-set! (%table m) key value))

    ;; Constructors.

    (define (hashmap comparator . args)
      (let ((m (%empty comparator)))
        (for-each (lambda (p) (%adjoin! m (car p) (cdr p)))
                  (%pairs "hashmap" args))
        m))

    (define (hashmap-unfold stop? mapper successor seed comparator)
      (let ((m (%empty comparator)))
        (let loop ((seed seed))
          (if (stop? seed)
              m
              (let-values (((key value) (mapper seed)))
                (%adjoin! m key value)
                (loop (successor seed)))))))

    ;; Predicates.

    (define (hashmap-contains? m key)
      (%check "hashmap-contains?" m)
      (hash-table-contains? (%table m) key))

    (define (hashmap-empty? m)
      (%check "hashmap-empty?" m)
      (hash-table-empty? (%table m)))

    (define (hashmap-disjoint? m1 m2)
      (%check "hashmap-disjoint?" m1)
      (%check "hashmap-disjoint?" m2)
      (not (hashmap-any? (lambda (k v) (hashmap-contains? m2 k)) m1)))

    ;; Accessors.

    (define %absent (list 'absent))

    (define hashmap-ref
      (case-lambda
        ((m key)
         (hashmap-ref m key
                      (lambda () (error "hashmap-ref: key not in hashmap" key))))
        ((m key failure) (hashmap-ref m key failure (lambda (value) value)))
        ((m key failure success)
         (%check "hashmap-ref" m)
         (let ((value (hash-table-ref/default (%table m) key %absent)))
           (if (eq? value %absent) (failure) (success value))))))

    (define (hashmap-ref/default m key default)
      (%check "hashmap-ref/default" m)
      (hash-table-ref/default (%table m) key default))

    ;; Updaters.

    (define (hashmap-adjoin! m . args)
      (%check "hashmap-adjoin!" m)
      (for-each (lambda (p) (%adjoin! m (car p) (cdr p)))
                (%pairs "hashmap-adjoin!" args))
      m)
    (define (hashmap-adjoin m . args) (apply %pure! hashmap-adjoin! m args))

    (define (hashmap-set! m . args)
      (%check "hashmap-set!" m)
      (for-each (lambda (p) (%set! m (car p) (cdr p)))
                (%pairs "hashmap-set!" args))
      m)
    (define (hashmap-set m . args) (apply %pure! hashmap-set! m args))

    (define (hashmap-replace! m key value)
      (%check "hashmap-replace!" m)
      (when (hash-table-contains? (%table m) key)
        ;; Delete first, so the new key is the one kept.
        (hash-table-delete! (%table m) key)
        (%set! m key value))
      m)
    (define (hashmap-replace m key value) (%pure! hashmap-replace! m key value))

    (define (hashmap-delete-all! m keys)
      (%check "hashmap-delete-all!" m)
      (for-each (lambda (key) (hash-table-delete! (%table m) key)) keys)
      m)
    (define (hashmap-delete! m . keys) (hashmap-delete-all! m keys))
    (define (hashmap-delete-all m keys) (%pure! hashmap-delete-all! m keys))
    (define (hashmap-delete m . keys) (hashmap-delete-all m keys))

    (define (hashmap-intern! m key failure)
      (%check "hashmap-intern!" m)
      (let ((value (hash-table-ref/default (%table m) key %absent)))
        (if (eq? value %absent)
            (let ((new (failure)))
              (%set! m key new)
              (values m new))
            (values m value))))
    (define (hashmap-intern m key failure)
      (%check "hashmap-intern" m)
      (if (hash-table-contains? (%table m) key)
          (values m (hash-table-ref/default (%table m) key #f))
          (hashmap-intern! (hashmap-copy m) key failure)))

    (define hashmap-update!
      (case-lambda
        ((m key updater)
         (hashmap-update! m key updater
                          (lambda ()
                            (error "hashmap-update!: key not in hashmap" key))))
        ((m key updater failure)
         (hashmap-update! m key updater failure (lambda (value) value)))
        ((m key updater failure success)
         (%check "hashmap-update!" m)
         (let* ((value (hash-table-ref/default (%table m) key %absent))
                (old (if (eq? value %absent) (failure) (success value))))
           (%set! m key (updater old))
           m))))
    (define hashmap-update
      (case-lambda
        ((m key updater) (%pure! hashmap-update! m key updater))
        ((m key updater failure) (%pure! hashmap-update! m key updater failure))
        ((m key updater failure success)
         (%pure! hashmap-update! m key updater failure success))))

    (define (hashmap-update!/default m key updater default)
      (hashmap-update! m key updater (lambda () default)))
    (define (hashmap-update/default m key updater default)
      (hashmap-update m key updater (lambda () default)))

    ;; Removes an association of the hashmap's choosing.
    (define hashmap-pop!
      (case-lambda
        ((m)
         (hashmap-pop! m (lambda ()
                           (error "hashmap-pop!: the hashmap has no association"))))
        ((m failure)
         (%check "hashmap-pop!" m)
         (if (hash-table-empty? (%table m))
             (failure)
             (let-values (((key value) (hash-table-pop! (%table m))))
               (values m key value))))))
    (define hashmap-pop
      (case-lambda
        ((m) (%pure! hashmap-pop! m))
        ((m failure)
         (%check "hashmap-pop" m)
         (if (hash-table-empty? (%table m))
             (failure)
             (hashmap-pop! (hashmap-copy m))))))

    (define (hashmap-search! m key failure success)
      (%check "hashmap-search!" m)
      (let ((value (hash-table-ref/default (%table m) key %absent)))
        (if (eq? value %absent)
            (failure
             ;; insert
             (lambda (value obj) (%set! m key value) (values m obj))
             ;; ignore
             (lambda (obj) (values m obj)))
            (success
             key
             value
             ;; update
             (lambda (new-key new-value obj)
               (hash-table-delete! (%table m) key)
               (%set! m new-key new-value)
               (values m obj))
             ;; remove
             (lambda (obj)
               (hash-table-delete! (%table m) key)
               (values m obj))))))
    (define (hashmap-search m key failure success)
      (%check "hashmap-search" m)
      (let ((value (hash-table-ref/default (%table m) key %absent)))
        (if (eq? value %absent)
            (failure
             ;; insert
             (lambda (value obj) (values (hashmap-set m key value) obj))
             ;; ignore
             (lambda (obj) (values m obj)))
            (success
             key
             value
             ;; update
             (lambda (new-key new-value obj)
               (let ((copy (hashmap-copy m)))
                 (hash-table-delete! (%table copy) key)
                 (%set! copy new-key new-value)
                 (values copy obj)))
             ;; remove
             (lambda (obj)
               (let ((copy (hashmap-copy m)))
                 (hash-table-delete! (%table copy) key)
                 (values copy obj)))))))

    ;; The whole hashmap.

    (define (hashmap-size m)
      (%check "hashmap-size" m)
      (hash-table-size (%table m)))

    (define (hashmap->alist m)
      (%check "hashmap->alist" m)
      (hash-table->alist (%table m)))

    (define (hashmap-find pred m failure)
      (%check "hashmap-find" m)
      (let loop ((entries (hash-table->alist (%table m))))
        (cond ((null? entries) (failure))
              ((pred (caar entries) (cdar entries))
               (values (caar entries) (cdar entries)))
              (else (loop (cdr entries))))))

    (define (hashmap-count pred m)
      (%check "hashmap-count" m)
      (hash-table-count pred (%table m)))

    (define (hashmap-any? pred m)
      (%check "hashmap-any?" m)
      (let loop ((entries (hash-table->alist (%table m))))
        (and (pair? entries)
             (or (and (pred (caar entries) (cdar entries)) #t)
                 (loop (cdr entries))))))

    (define (hashmap-every? pred m)
      (%check "hashmap-every?" m)
      (not (hashmap-any? (lambda (k v) (not (pred k v))) m)))

    (define (hashmap-keys m) (map car (hashmap->alist m)))
    (define (hashmap-values m) (map cdr (hashmap->alist m)))
    (define (hashmap-entries m)
      (let ((entries (hashmap->alist m)))
        (values (map car entries) (map cdr entries))))

    ;; Mapping and folding.

    (define (hashmap-for-each proc m)
      (for-each (lambda (e) (proc (car e) (cdr e))) (hashmap->alist m)))

    (define (hashmap-map->list proc m)
      (map (lambda (e) (proc (car e) (cdr e))) (hashmap->alist m)))

    (define (hashmap-fold proc nil m)
      (let loop ((acc nil) (entries (hashmap->alist m)))
        (if (null? entries)
            acc
            (loop (proc (caar entries) (cdar entries) acc) (cdr entries)))))

    (define (hashmap-map proc comparator m)
      (%check "hashmap-map" m)
      (let ((result (%empty comparator)))
        (hashmap-for-each
         (lambda (k v)
           (let-values (((k2 v2) (proc k v)))
             (%set! result k2 v2)))
         m)
        result))

    (define (hashmap-filter! pred m)
      (%check "hashmap-filter!" m)
      (hash-table-prune! (lambda (k v) (not (pred k v))) (%table m))
      m)
    (define (hashmap-filter pred m) (hashmap-filter! pred (hashmap-copy m)))
    ;; The linear forms take the predicate first, as in the SRFI.
    (define (hashmap-remove! pred m)
      (hashmap-filter! (lambda (k v) (not (pred k v))) m))
    (define (hashmap-remove pred m)
      (hashmap-filter (lambda (k v) (not (pred k v))) m))
    (define (hashmap-partition pred m)
      (values (hashmap-filter pred m) (hashmap-remove pred m)))
    (define (hashmap-partition! pred m)
      ;; The copy that satisfies `pred` is made before `m` is changed.
      (let* ((yes (hashmap-filter pred m))
             (no (hashmap-remove! pred m)))
        (values yes no)))

    ;; Conversion.

    (define (alist->hashmap! m alist)
      (%check "alist->hashmap!" m)
      (for-each (lambda (p) (%adjoin! m (car p) (cdr p))) alist)
      m)
    (define (alist->hashmap comparator alist)
      (alist->hashmap! (%empty comparator) alist))

    ;; Submappings, as in (srfi 146).

    (define (%submap? comparator m1 m2)
      (and (eq? (hashmap-key-comparator m1) (hashmap-key-comparator m2))
           (<= (hashmap-size m1) (hashmap-size m2))
           (hashmap-every?
            (lambda (k v)
              (let ((v2 (hash-table-ref/default (%table m2) k %absent)))
                (and (not (eq? v2 %absent)) (=? comparator v v2))))
            m1)))

    (define (%chain who test comparator m rest)
      (%check who m)
      (let loop ((m m) (rest rest))
        (or (null? rest)
            (begin (%check who (car rest))
                   (and (test comparator m (car rest))
                        (loop (car rest) (cdr rest)))))))

    (define (%equal? c a b)
      (and (%submap? c a b) (= (hashmap-size a) (hashmap-size b))))
    (define (%proper? c a b)
      (and (%submap? c a b) (< (hashmap-size a) (hashmap-size b))))

    (define (hashmap=? comparator m . rest)
      (%chain "hashmap=?" %equal? comparator m rest))
    (define (hashmap<=? comparator m . rest)
      (%chain "hashmap<=?" %submap? comparator m rest))
    (define (hashmap<? comparator m . rest)
      (%chain "hashmap<?" %proper? comparator m rest))
    (define (hashmap>=? comparator m . rest)
      (%chain "hashmap>=?" (lambda (c a b) (%submap? c b a)) comparator m rest))
    (define (hashmap>? comparator m . rest)
      (%chain "hashmap>?" (lambda (c a b) (%proper? c b a)) comparator m rest))

    ;; Set theory: keys are compared, and the first hashmap in which a key
    ;; appears supplies its association.

    (define (hashmap-union! m . rest)
      (%check "hashmap-union!" m)
      (for-each (lambda (other)
                  (%check "hashmap-union!" other)
                  (hashmap-for-each (lambda (k v) (%adjoin! m k v)) other))
                rest)
      m)
    (define (hashmap-union m . rest) (apply hashmap-union! (hashmap-copy m) rest))

    (define (hashmap-intersection! m . rest)
      (%check "hashmap-intersection!" m)
      (for-each (lambda (other)
                  (%check "hashmap-intersection!" other)
                  (hashmap-filter! (lambda (k v) (hashmap-contains? other k)) m))
                rest)
      m)
    (define (hashmap-intersection m . rest)
      (apply hashmap-intersection! (hashmap-copy m) rest))

    (define (hashmap-difference! m . rest)
      (%check "hashmap-difference!" m)
      (for-each (lambda (other)
                  (%check "hashmap-difference!" other)
                  (hashmap-filter! (lambda (k v) (not (hashmap-contains? other k)))
                                   m))
                rest)
      m)
    (define (hashmap-difference m . rest)
      (apply hashmap-difference! (hashmap-copy m) rest))

    (define (hashmap-xor! m1 m2)
      (%check "hashmap-xor!" m1)
      (%check "hashmap-xor!" m2)
      (let ((from-2 (hashmap-difference m2 m1)))
        (hashmap-difference! m1 m2)
        (hashmap-union! m1 from-2)))
    (define (hashmap-xor m1 m2) (hashmap-xor! (hashmap-copy m1) m2))

    ;; Comparators: equal when `hashmap=?`, and no order.

    (define %hash-depth 0)

    (define (make-hashmap-comparator value-comparator)
      (define (hash m)
        (let ((size (hashmap-size m)))
          (if (>= %hash-depth 3)
              size
              (let ((key-hash (hash-table-hash-function (%table m))))
                (dynamic-wind
                 (lambda () (set! %hash-depth (+ %hash-depth 1)))
                 (lambda ()
                   (modulo (+ size
                              (hashmap-fold
                               (lambda (k v acc)
                                 (modulo (+ acc (key-hash k)) (expt 2 59)))
                               5381 m))
                           (expt 2 59)))
                 (lambda () (set! %hash-depth (- %hash-depth 1))))))))
      (make-comparator hashmap?
                       (lambda (a b) (hashmap=? value-comparator a b))
                       #f
                       hash))

    (define hashmap-comparator
      (make-hashmap-comparator (make-default-comparator)))))
