;;; (srfi 125): hash tables, in Scheme over (srfi 128) comparators, with
;;; SRFI 69's deprecated names.
;;;
;;; A table is a vector of buckets, each a list of entries `#(hash key
;;; value)`, and it doubles when it holds more entries than buckets.
;;; Storing each entry's hash makes growing cheap, and makes a lookup
;;; compare keys only where the hashes match. The hash and equality
;;; functions come from the table's comparator, or, for a table made the
;;; SRFI 69 way, from an equality predicate and an optional hash function.
;;;
;;; What this implementation leaves out, as the SRFI lets it:
;;; - Weak and ephemeral tables. `weak-keys`, `ephemeral-keys`,
;;;   `weak-values` and `ephemeral-values` are accepted as hints and
;;;   ignored: the table is an ordinary one that keeps its keys and values
;;;   alive. (Ephemerons are not built yet.) So is `thread-safe`: a table
;;;   is not safe to change from several tasks at once.
;;; - Immutable tables. Every table is mutable, `hash-table-mutable?` is
;;;   always true, and the `mutable?` argument of `hash-table-copy` is
;;;   ignored.
;;;
;;; `hash-table-merge!` is `hash-table-union!`, as SRFI 125 has it (SRFI 69
;;; had the second table's values replace the first's).

(define-library (srfi 125)
  (import (scheme base) (scheme char)
          (srfi 128)
          (only (vali primitive) %eq-hash %eqv-hash %equal-hash %number-hash))
  (export make-hash-table hash-table hash-table-unfold alist->hash-table
          hash-table? hash-table-contains? hash-table-exists?
          hash-table-empty? hash-table=? hash-table-mutable?
          hash-table-ref hash-table-ref/default
          hash-table-set! hash-table-delete! hash-table-intern!
          hash-table-update! hash-table-update!/default
          hash-table-pop! hash-table-clear!
          hash-table-size hash-table-keys hash-table-values
          hash-table-entries hash-table-find hash-table-count
          hash-table-map hash-table-for-each hash-table-walk
          hash-table-map! hash-table-map->list hash-table-fold
          hash-table-prune!
          hash-table-copy hash-table-empty-copy hash-table->alist
          hash-table-union! hash-table-merge! hash-table-intersection!
          hash-table-difference! hash-table-xor!
          hash string-hash string-ci-hash hash-by-identity
          hash-table-equivalence-function hash-table-hash-function)
  (begin
    ;; `buckets` is the vector of entry lists, `size` the number of entries.
    (define-record-type %hash-table
      (%make-table equality hash buckets size)
      hash-table?
      (equality hash-table-equivalence-function)
      (hash %table-hash)
      (buckets %buckets %set-buckets!)
      (size hash-table-size %set-size!))

    (define (hash-table-hash-function table) (%table-hash table))

    (define (%check-table who table)
      (unless (hash-table? table)
        (error (string-append who ": not a hash table") table)))

    (define %initial-capacity 8)

    ;; The equality and hash function for an SRFI 69 style table made from
    ;; `equality` and, optionally, `hash`.
    (define (%functions equality hash)
      (cond (hash (values equality hash))
            ((eq? equality eq?) (values equality %eq-hash))
            ((eq? equality eqv?) (values equality %eqv-hash))
            ((eq? equality equal?) (values equality %equal-hash))
            ((eq? equality =) (values equality %number-hash))
            ((eq? equality string=?) (values equality string-hash))
            ((eq? equality string-ci=?) (values equality string-ci-hash))
            (else (error "make-hash-table: no hash function for this predicate"
                         equality))))

    ;; The capacity asked for among the extra arguments of a constructor,
    ;; or the default. The symbols that name optional features are hints.
    (define (%capacity args)
      (cond ((null? args) %initial-capacity)
            ((and (exact-integer? (car args)) (>= (car args) 0))
             (max 1 (min (car args) 1048576)))
            ((memq (car args) '(thread-safe weak-keys ephemeral-keys
                                weak-values ephemeral-values))
             (%capacity (cdr args)))
            (else (error "make-hash-table: bad argument" (car args)))))

    ;; A table for a comparator, or for an equality predicate and hash
    ;; function, followed by the optional arguments `args`.
    (define (%new who first args)
      (cond ((comparator? first)
             (%make-table (comparator-equality-predicate first)
                          (comparator-hash-function first)
                          (make-vector (%capacity args) '())
                          0))
            ((procedure? first)
             (let ((hash (and (pair? args) (procedure? (car args))
                              (car args))))
               (let-values (((equality hasher)
                             (%functions first hash)))
                 (%make-table equality hasher
                              (make-vector (%capacity (if hash (cdr args) args))
                                           '())
                              0))))
            (else (error (string-append who ": expected a comparator") first))))

    (define (make-hash-table . args)
      (if (null? args)
          (%new "make-hash-table" equal? '())
          (%new "make-hash-table" (car args) (cdr args))))

    ;; Entries.
    (define (%entry hash key value) (vector hash key value))
    (define (%entry-hash e) (vector-ref e 0))
    (define (%entry-key e) (vector-ref e 1))
    (define (%entry-value e) (vector-ref e 2))
    (define (%set-entry-value! e v) (vector-set! e 2 v))

    (define (%index hash buckets)
      (modulo hash (vector-length buckets)))

    ;; The entry for `key` in the chain `chain`, or #f.
    (define (%find equality hash key chain)
      (cond ((null? chain) #f)
            ((and (eqv? (%entry-hash (car chain)) hash)
                  (equality (%entry-key (car chain)) key))
             (car chain))
            (else (%find equality hash key (cdr chain)))))

    (define (%lookup table key)
      (let* ((hash ((%table-hash table) key))
             (buckets (%buckets table)))
        (%find (hash-table-equivalence-function table) hash key
               (vector-ref buckets (%index hash buckets)))))

    ;; Doubles the buckets when there are more entries than buckets.
    (define (%grow! table)
      (let* ((old (%buckets table))
             (n (* 2 (vector-length old)))
             (new (make-vector n '())))
        (let each ((i 0))
          (when (< i (vector-length old))
            (let chain ((es (vector-ref old i)))
              (unless (null? es)
                (let ((j (modulo (%entry-hash (car es)) n))
                      (rest (cdr es)))
                  (vector-set! new j (cons (car es) (vector-ref new j)))
                  (chain rest))))
            (each (+ i 1))))
        (%set-buckets! table new)))

    (define (%add! table hash key value)
      (let* ((buckets (%buckets table))
             (i (%index hash buckets)))
        (vector-set! buckets i
                     (cons (%entry hash key value) (vector-ref buckets i)))
        (%set-size! table (+ (hash-table-size table) 1))
        (when (> (hash-table-size table) (vector-length buckets))
          (%grow! table))))

    (define (%store! table key value)
      (let* ((hash ((%table-hash table) key))
             (buckets (%buckets table))
             (entry (%find (hash-table-equivalence-function table) hash key
                           (vector-ref buckets (%index hash buckets)))))
        (if entry
            (%set-entry-value! entry value)
            (%add! table hash key value))))

    ;; Removes the entry for `key`; returns whether there was one.
    (define (%remove! table key)
      (let* ((hash ((%table-hash table) key))
             (equality (hash-table-equivalence-function table))
             (buckets (%buckets table))
             (i (%index hash buckets)))
        (let loop ((chain (vector-ref buckets i)) (kept '()) (found #f))
          (cond ((null? chain)
                 (and found
                      (begin
                        (vector-set! buckets i (reverse kept))
                        (%set-size! table (- (hash-table-size table) 1))
                        #t)))
                ((and (not found)
                      (eqv? (%entry-hash (car chain)) hash)
                      (equality (%entry-key (car chain)) key))
                 (loop (cdr chain) kept #t))
                (else (loop (cdr chain) (cons (car chain) kept) found))))))

    ;; Calls `(proc key value)` for each entry. The table must not change
    ;; while it runs.
    (define (%walk table proc)
      (let ((buckets (%buckets table)))
        (let each ((i 0))
          (when (< i (vector-length buckets))
            (for-each (lambda (e) (proc (%entry-key e) (%entry-value e)))
                      (vector-ref buckets i))
            (each (+ i 1))))))

    (define (%entries table)
      (let ((out '()))
        (%walk table (lambda (k v) (set! out (cons (cons k v) out))))
        out))

    ;; Constructors.
    (define (hash-table comparator . kvs)
      (let ((table (%new "hash-table" comparator '())))
        (let loop ((kvs kvs))
          (cond ((null? kvs) table)
                ((null? (cdr kvs))
                 (error "hash-table: a key with no value" (car kvs)))
                (else (%store! table (car kvs) (cadr kvs))
                      (loop (cddr kvs)))))))

    (define (hash-table-unfold stop? mapper successor seed comparator . args)
      (let ((table (%new "hash-table-unfold" comparator args)))
        (let loop ((seed seed))
          (if (stop? seed)
              table
              (let-values (((k v) (mapper seed)))
                (%store! table k v)
                (loop (successor seed)))))))

    (define (alist->hash-table alist . args)
      (let ((table (if (null? args)
                       (%new "alist->hash-table" equal? '())
                       (%new "alist->hash-table" (car args) (cdr args)))))
        ;; Earlier associations win: add each key that is not there yet.
        (for-each (lambda (p)
                    (unless (%lookup table (car p))
                      (%store! table (car p) (cdr p))))
                  alist)
        table))

    ;; Predicates and accessors.
    (define (hash-table-contains? table key)
      (%check-table "hash-table-contains?" table)
      (and (%lookup table key) #t))
    (define hash-table-exists? hash-table-contains?)

    (define (hash-table-empty? table)
      (%check-table "hash-table-empty?" table)
      (= (hash-table-size table) 0))

    (define (hash-table-mutable? table)
      (%check-table "hash-table-mutable?" table)
      #t)

    (define (hash-table-ref table key . rest)
      (%check-table "hash-table-ref" table)
      (let ((entry (%lookup table key)))
        (cond (entry
               (if (and (pair? rest) (pair? (cdr rest)))
                   ((cadr rest) (%entry-value entry))
                   (%entry-value entry)))
              ((and (pair? rest) (car rest)) ((car rest)))
              (else (error "hash-table-ref: no such key" key)))))

    (define (hash-table-ref/default table key default)
      (%check-table "hash-table-ref/default" table)
      (let ((entry (%lookup table key)))
        (if entry (%entry-value entry) default)))

    ;; Mutators.
    (define (hash-table-set! table . kvs)
      (%check-table "hash-table-set!" table)
      (let loop ((kvs kvs))
        (cond ((null? kvs))
              ((null? (cdr kvs))
               (error "hash-table-set!: a key with no value" (car kvs)))
              (else (%store! table (car kvs) (cadr kvs))
                    (loop (cddr kvs))))))

    (define (hash-table-delete! table . keys)
      (%check-table "hash-table-delete!" table)
      (let loop ((keys keys) (n 0))
        (if (null? keys)
            n
            (loop (cdr keys) (if (%remove! table (car keys)) (+ n 1) n)))))

    (define (hash-table-intern! table key failure)
      (%check-table "hash-table-intern!" table)
      (let ((entry (%lookup table key)))
        (if entry
            (%entry-value entry)
            (let ((value (failure)))
              (%store! table key value)
              value))))

    (define (hash-table-update! table key updater . rest)
      (%check-table "hash-table-update!" table)
      (%store! table key
               (updater (apply hash-table-ref table key rest))))

    (define (hash-table-update!/default table key updater default)
      (%check-table "hash-table-update!/default" table)
      (%store! table key
               (updater (hash-table-ref/default table key default))))

    (define (hash-table-pop! table)
      (%check-table "hash-table-pop!" table)
      (when (= (hash-table-size table) 0)
        (error "hash-table-pop!: the table is empty" table))
      (let* ((buckets (%buckets table)))
        (let find ((i 0))
          (if (null? (vector-ref buckets i))
              (find (+ i 1))
              (let ((entry (car (vector-ref buckets i))))
                (vector-set! buckets i (cdr (vector-ref buckets i)))
                (%set-size! table (- (hash-table-size table) 1))
                (values (%entry-key entry) (%entry-value entry)))))))

    (define (hash-table-clear! table)
      (%check-table "hash-table-clear!" table)
      (%set-buckets! table (make-vector %initial-capacity '()))
      (%set-size! table 0))

    ;; The whole table.
    (define (hash-table-keys table)
      (%check-table "hash-table-keys" table)
      (map car (%entries table)))

    (define (hash-table-values table)
      (%check-table "hash-table-values" table)
      (map cdr (%entries table)))

    (define (hash-table-entries table)
      (%check-table "hash-table-entries" table)
      (let ((entries (%entries table)))
        (values (map car entries) (map cdr entries))))

    (define (hash-table->alist table)
      (%check-table "hash-table->alist" table)
      (%entries table))

    (define (hash-table-find proc table failure)
      (%check-table "hash-table-find" table)
      (let loop ((entries (%entries table)))
        (if (null? entries)
            (failure)
            (or (proc (car (car entries)) (cdr (car entries)))
                (loop (cdr entries))))))

    (define (hash-table-count pred table)
      (%check-table "hash-table-count" table)
      (let ((n 0))
        (%walk table (lambda (k v) (when (pred k v) (set! n (+ n 1)))))
        n))

    ;; Mapping and folding.
    (define (hash-table-map proc comparator table)
      (%check-table "hash-table-map" table)
      (let ((result (%new "hash-table-map" comparator '())))
        (%walk table (lambda (k v) (%store! result k (proc v))))
        result))

    (define (hash-table-for-each proc table)
      ;; The deprecated `hash-table-walk` order is accepted too.
      (if (hash-table? proc)
          (hash-table-for-each table proc)
          (begin (%check-table "hash-table-for-each" table)
                 (%walk table proc))))

    (define (hash-table-walk table proc) (hash-table-for-each proc table))

    (define (hash-table-map! proc table)
      (%check-table "hash-table-map!" table)
      (for-each (lambda (e) (%set-entry-value!
                             e (proc (%entry-key e) (%entry-value e))))
                (let ((out '()))
                  (let ((buckets (%buckets table)))
                    (vector-for-each
                     (lambda (chain) (set! out (append chain out)))
                     buckets))
                  out)))

    (define (hash-table-map->list proc table)
      (%check-table "hash-table-map->list" table)
      (map (lambda (e) (proc (car e) (cdr e))) (%entries table)))

    (define (hash-table-fold proc seed table)
      ;; The deprecated `(hash-table-fold table proc seed)` order too.
      (if (hash-table? proc)
          (hash-table-fold seed table proc)
          (begin
            (%check-table "hash-table-fold" table)
            (let ((acc seed))
              (%walk table (lambda (k v) (set! acc (proc k v acc))))
              acc))))

    (define (hash-table-prune! proc table)
      (%check-table "hash-table-prune!" table)
      (for-each (lambda (e) (when (proc (car e) (cdr e))
                              (%remove! table (car e))))
                (%entries table)))

    ;; Copies.
    (define (hash-table-empty-copy table)
      (%check-table "hash-table-empty-copy" table)
      (%make-table (hash-table-equivalence-function table)
                   (%table-hash table)
                   (make-vector (vector-length (%buckets table)) '())
                   0))

    (define (hash-table-copy table . mutable?)
      (%check-table "hash-table-copy" table)
      (let ((copy (hash-table-empty-copy table)))
        (%walk table (lambda (k v) (%store! copy k v)))
        copy))

    ;; Tables as sets.
    (define (hash-table-union! table1 table2)
      (%check-table "hash-table-union!" table1)
      (%check-table "hash-table-union!" table2)
      (%walk table2 (lambda (k v)
                      (unless (%lookup table1 k) (%store! table1 k v))))
      table1)
    (define hash-table-merge! hash-table-union!)

    (define (hash-table-intersection! table1 table2)
      (%check-table "hash-table-intersection!" table1)
      (%check-table "hash-table-intersection!" table2)
      (for-each (lambda (e) (unless (%lookup table2 (car e))
                              (%remove! table1 (car e))))
                (%entries table1))
      table1)

    (define (hash-table-difference! table1 table2)
      (%check-table "hash-table-difference!" table1)
      (%check-table "hash-table-difference!" table2)
      (for-each (lambda (e) (when (%lookup table2 (car e))
                              (%remove! table1 (car e))))
                (%entries table1))
      table1)

    (define (hash-table-xor! table1 table2)
      (%check-table "hash-table-xor!" table1)
      (%check-table "hash-table-xor!" table2)
      (for-each (lambda (e)
                  (if (%lookup table1 (car e))
                      (%remove! table1 (car e))
                      (%store! table1 (car e) (cdr e))))
                (%entries table2))
      table1)

    (define (hash-table=? value-comparator table1 table2)
      (%check-table "hash-table=?" table1)
      (%check-table "hash-table=?" table2)
      (and (= (hash-table-size table1) (hash-table-size table2))
           (let ((equal (comparator-equality-predicate value-comparator)))
             (let loop ((entries (%entries table1)))
               (or (null? entries)
                   (let ((other (%lookup table2 (car (car entries)))))
                     (and other
                          (equal (cdr (car entries)) (%entry-value other))
                          (loop (cdr entries)))))))))

    ;; SRFI 69's hash functions, which take an optional bound.
    (define (hash obj . bound) (apply default-hash obj bound))
    (define (hash-by-identity obj . bound)
      (let ((h (%eq-hash obj)))
        (if (pair? bound) (modulo h (car bound)) h)))))
