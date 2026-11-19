;;; (srfi 128): comparators, objects that bundle a type test, an equality
;;; predicate, and optionally an ordering predicate and a hash function.
;;;
;;; The hash functions are the runtime's (`%equal-hash` and its kin, in
;;; vali-runtime's hashing.rs): deterministic, 60 bits, and bounded work on
;;; any structure, so deep and circular data cannot hang them. `eq` and
;;; `eqv` comparators hash objects by identity, which every memory strategy
;;; keeps stable (ADR 0003), and the equal and default comparators hash
;;; by content.
;;;
;;; The default comparator orders values of different types by their type,
;;; in the SRFI's order: the empty list, pairs, booleans, characters,
;;; strings, symbols, numbers, vectors, bytevectors, then the types
;;; registered with `comparator-register-default!`, in the order they were
;;; registered. An object of no known type is equal only to itself (`eqv?`)
;;; and has no order: comparing it with `<?` is an error.
;;;
;;; `hash-bound` is 2^60, the exclusive bound of every hash here, and
;;; `hash-salt` is 0: hashes of content (numbers, strings, symbols and the
;;; pairs and vectors holding them) do not vary between runs. Identity
;;; hashes do, because they come from an object's address: those of the eq
;;; and eqv comparators, and `default-hash` of anything that reaches a
;;; record or a procedure. Do not rely on an eq table's iteration order.
;;;
;;; The standard hash functions accept an optional bound, as SRFI 69's do
;;; (SRFI 128 allows an extra argument): the hash is then taken modulo it.
;;; (srfi 125) and (srfi 69) export the same procedures, so importing both
;;; with this library does not conflict.

(define-library (srfi 128)
  (import (scheme base) (scheme char) (scheme complex)
          (only (vali primitive)
                %eq-hash %eqv-hash %equal-hash %number-hash
                %string-hash %string-ci-hash %symbol-hash))
  (export comparator? comparator-ordered? comparator-hashable?
          make-comparator make-pair-comparator make-list-comparator
          make-vector-comparator make-eq-comparator make-eqv-comparator
          make-equal-comparator
          boolean-hash char-hash char-ci-hash string-hash string-ci-hash
          symbol-hash number-hash hash-bound hash-salt
          make-default-comparator default-hash comparator-register-default!
          comparator-type-test-predicate comparator-equality-predicate
          comparator-ordering-predicate comparator-hash-function
          comparator-test-type comparator-check-type comparator-hash
          =? <? >? <=? >=? comparator-if<=>)
  (begin
    (define-syntax hash-bound
      (syntax-rules () ((_) 1152921504606846976)))
    (define-syntax hash-salt
      (syntax-rules () ((_) 0)))

    ;; The five parts of a comparator. The two flags say whether the
    ;; ordering and the hash were given.
    (define-record-type comparator
      (%make-comparator type-test equality ordering hash ordered? hashable?)
      comparator?
      (type-test comparator-type-test-predicate)
      (equality comparator-equality-predicate)
      (ordering comparator-ordering-predicate)
      (hash comparator-hash-function)
      (ordered? comparator-ordered?)
      (hashable? comparator-hashable?))

    (define (make-comparator type-test equality ordering hash)
      (let* ((test (if (eq? type-test #t) (lambda (x) #t) type-test))
             (order (cond ((procedure? ordering) ordering)
                          ((not ordering)
                           (lambda (a b)
                             (error "comparator has no ordering" a b)))
                          (else
                           (error "make-comparator: bad ordering" ordering))))
             (equal (cond ((procedure? equality) equality)
                          ((and (eq? equality #t) ordering)
                           (lambda (a b)
                             (not (or (ordering a b) (ordering b a)))))
                          (else
                           (error "make-comparator: bad equality" equality))))
             (hasher (cond ((procedure? hash) hash)
                           ((not hash)
                            (lambda (x)
                              (error "comparator has no hash function" x)))
                           (else
                            (error "make-comparator: bad hash" hash)))))
        (unless (procedure? test)
          (error "make-comparator: bad type test" type-test))
        (%make-comparator test equal order hasher
                          (and ordering #t) (and hash #t))))

    (define (comparator-test-type comparator obj)
      ((comparator-type-test-predicate comparator) obj))

    (define (comparator-check-type comparator obj)
      (or (comparator-test-type comparator obj)
          (error "comparator type check failed" comparator obj)))

    (define (comparator-hash comparator obj)
      ((comparator-hash-function comparator) obj))

    ;; The comparison predicates: each is true when the relation holds
    ;; between every two neighbouring objects.
    (define (%chain compare comparator a rest)
      (or (null? rest)
          (and (compare comparator a (car rest))
               (%chain compare comparator (car rest) (cdr rest)))))

    (define (%= comparator a b)
      ((comparator-equality-predicate comparator) a b))
    (define (%< comparator a b)
      ((comparator-ordering-predicate comparator) a b))

    (define (=? comparator a . rest) (%chain %= comparator a rest))
    (define (<? comparator a . rest) (%chain %< comparator a rest))
    (define (>? comparator a . rest)
      (%chain (lambda (c x y) (%< c y x)) comparator a rest))
    (define (<=? comparator a . rest)
      (%chain (lambda (c x y) (or (%< c x y) (%= c x y))) comparator a rest))
    (define (>=? comparator a . rest)
      (%chain (lambda (c x y) (or (%< c y x) (%= c x y))) comparator a rest))

    (define-syntax comparator-if<=>
      (syntax-rules ()
        ((_ a b less equal greater)
         (comparator-if<=> (make-default-comparator) a b less equal greater))
        ((_ comparator a b less equal greater)
         (let ((c comparator) (x a) (y b))
           (cond ((=? c x y) equal)
                 ((<? c x y) less)
                 (else greater))))))

    ;; Hash functions. Each takes an optional bound, as SRFI 69's do.
    (define (%bounded h bound)
      (if (pair? bound) (modulo h (car bound)) h))

    ;; Folds a hash `x` into the running hash `h` in fixnum arithmetic.
    (define %prime 1125899906842597)
    (define (%combine h x)
      (modulo (+ (* (modulo h %prime) 33) (modulo x %prime)) %prime))

    (define (boolean-hash b . bound) (%bounded (%equal-hash b) bound))
    (define (char-hash c . bound) (%bounded (%equal-hash c) bound))
    (define (char-ci-hash c . bound)
      (%bounded (%equal-hash (char-foldcase c)) bound))
    (define (string-hash s . bound) (%bounded (%string-hash s) bound))
    (define (string-ci-hash s . bound) (%bounded (%string-ci-hash s) bound))
    (define (symbol-hash s . bound) (%bounded (%symbol-hash s) bound))
    (define (number-hash n . bound) (%bounded (%number-hash n) bound))

    ;; How many elements of a list or vector a constructed hash looks at,
    ;; so that a long (or circular) value costs a bounded amount.
    (define %hash-elements 64)

    (define (make-pair-comparator car-comparator cdr-comparator)
      (make-comparator
       (lambda (x)
         (and (pair? x)
              (comparator-test-type car-comparator (car x))
              (comparator-test-type cdr-comparator (cdr x))))
       (lambda (a b)
         (and (=? car-comparator (car a) (car b))
              (=? cdr-comparator (cdr a) (cdr b))))
       (lambda (a b)
         (if (=? car-comparator (car a) (car b))
             (<? cdr-comparator (cdr a) (cdr b))
             (<? car-comparator (car a) (car b))))
       (lambda (x)
         (%combine (comparator-hash car-comparator (car x))
                   (comparator-hash cdr-comparator (cdr x))))))

    (define (make-list-comparator element-comparator type-test empty? head tail)
      (make-comparator
       type-test
       (lambda (a b)
         (let loop ((a a) (b b))
           (cond ((empty? a) (empty? b))
                 ((empty? b) #f)
                 (else (and (=? element-comparator (head a) (head b))
                            (loop (tail a) (tail b)))))))
       (lambda (a b)
         (let loop ((a a) (b b))
           (cond ((empty? a) (not (empty? b)))
                 ((empty? b) #f)
                 ((=? element-comparator (head a) (head b))
                  (loop (tail a) (tail b)))
                 (else (<? element-comparator (head a) (head b))))))
       (lambda (x)
         (let loop ((x x) (h 0) (n %hash-elements))
           (if (or (empty? x) (= n 0))
               h
               (loop (tail x)
                     (%combine h (comparator-hash element-comparator (head x)))
                     (- n 1)))))))

    (define (make-vector-comparator element-comparator type-test length ref)
      (make-comparator
       type-test
       (lambda (a b)
         (let ((n (length a)))
           (and (= n (length b))
                (let loop ((i 0))
                  (or (= i n)
                      (and (=? element-comparator (ref a i) (ref b i))
                           (loop (+ i 1))))))))
       (lambda (a b)
         (let ((n (length a)) (m (length b)))
           (cond ((< n m) #t)
                 ((> n m) #f)
                 (else
                  (let loop ((i 0))
                    (cond ((= i n) #f)
                          ((=? element-comparator (ref a i) (ref b i))
                           (loop (+ i 1)))
                          (else
                           (<? element-comparator (ref a i) (ref b i)))))))))
       (lambda (x)
         (let ((n (length x)))
           (let loop ((i 0) (h n))
             (if (or (= i n) (= i %hash-elements))
                 h
                 (loop (+ i 1)
                       (%combine h (comparator-hash element-comparator
                                                    (ref x i))))))))))

    ;; The identity comparators compare by `eq?` and `eqv?`; their hashes
    ;; are identity hashes, which stay the same however the heap is laid
    ;; out. Their ordering is a total order on the objects' hashes that
    ;; breaks ties arbitrarily, so it is an error to rely on it: it is
    ;; provided only to be complete, and raises.
    (define (%no-order a b)
      (error "this comparator has no meaningful ordering" a b))

    ;; Comparators registered for further types, newest last.
    (define %registered '())

    (define (comparator-register-default! comparator)
      (set! %registered (append %registered (list comparator))))

    ;; The registered comparator whose type test accepts `x`, or #f.
    (define (%registered-for x)
      (let loop ((r %registered))
        (cond ((null? r) #f)
              ((comparator-test-type (car r) x) (car r))
              (else (loop (cdr r))))))

    ;; The rank of `x`'s type in the default order.
    (define (%rank x)
      (cond ((null? x) 0)
            ((pair? x) 1)
            ((boolean? x) 2)
            ((char? x) 3)
            ((string? x) 4)
            ((symbol? x) 5)
            ((number? x) 6)
            ((vector? x) 7)
            ((bytevector? x) 8)
            (else
             (let loop ((r %registered) (i 9))
               (cond ((null? r) i)
                     ((comparator-test-type (car r) x) i)
                     (else (loop (cdr r) (+ i 1))))))))

    (define (%registered? rank) (and (>= rank 9) (<= rank (+ 8 (length %registered)))))
    (define (%registered-at rank) (list-ref %registered (- rank 9)))

    (define (%default=? a b)
      (let ((ra (%rank a)) (rb (%rank b)))
        (and (= ra rb)
             (case ra
               ((0) #t)
               ((1) (and (%default=? (car a) (car b))
                         (%default=? (cdr a) (cdr b))))
               ((2) (eq? a b))
               ((3) (char=? a b))
               ((4) (string=? a b))
               ((5) (eq? a b))
               ((6) (= a b))
               ((7) (let ((n (vector-length a)))
                      (and (= n (vector-length b))
                           (let loop ((i 0))
                             (or (= i n)
                                 (and (%default=? (vector-ref a i)
                                                  (vector-ref b i))
                                      (loop (+ i 1))))))))
               ((8) (let ((n (bytevector-length a)))
                      (and (= n (bytevector-length b))
                           (let loop ((i 0))
                             (or (= i n)
                                 (and (= (bytevector-u8-ref a i)
                                         (bytevector-u8-ref b i))
                                      (loop (+ i 1))))))))
               (else
                (if (%registered? ra)
                    (=? (%registered-at ra) a b)
                    (eqv? a b)))))))

    (define (%default<? a b)
      (let ((ra (%rank a)) (rb (%rank b)))
        (if (not (= ra rb))
            (< ra rb)
            (case ra
              ((0) #f)
              ((1) (if (%default=? (car a) (car b))
                       (%default<? (cdr a) (cdr b))
                       (%default<? (car a) (car b))))
              ((2) (and (not a) b #t))
              ((3) (char<? a b))
              ((4) (string<? a b))
              ((5) (string<? (symbol->string a) (symbol->string b)))
              ((6) (if (and (real? a) (real? b))
                       (< a b)
                       (let ((ar (real-part a)) (br (real-part b)))
                         (if (= ar br)
                             (< (imag-part a) (imag-part b))
                             (< ar br)))))
              ((7) (let ((n (vector-length a)) (m (vector-length b)))
                     (cond ((< n m) #t)
                           ((> n m) #f)
                           (else
                            (let loop ((i 0))
                              (cond ((= i n) #f)
                                    ((%default=? (vector-ref a i)
                                                 (vector-ref b i))
                                     (loop (+ i 1)))
                                    (else
                                     (%default<? (vector-ref a i)
                                                 (vector-ref b i)))))))))
              ((8) (let ((n (bytevector-length a)) (m (bytevector-length b)))
                     (cond ((< n m) #t)
                           ((> n m) #f)
                           (else
                            (let loop ((i 0))
                              (cond ((= i n) #f)
                                    ((= (bytevector-u8-ref a i)
                                        (bytevector-u8-ref b i))
                                     (loop (+ i 1)))
                                    (else
                                     (< (bytevector-u8-ref a i)
                                        (bytevector-u8-ref b i)))))))))
              (else
               (if (%registered? ra)
                   (<? (%registered-at ra) a b)
                   (error "no ordering for this type of object" a b)))))))

    ;; `default-hash` is the runtime's `equal?` hash, which treats numbers
    ;; by value as `=` does and looks at a bounded part of a structure, unless
    ;; a comparator is registered. Then pairs and vectors are walked here, so
    ;; that a registered type nested in them hashes as its comparator says.
    (define (default-hash obj . bound)
      (%bounded
       (if (null? %registered)
           (%equal-hash obj)
           (let ((budget (list %hash-elements)))
             (%slow-hash obj budget 0)))
       bound))

    (define (%slow-hash obj budget depth)
      (set-car! budget (- (car budget) 1))
      (cond ((or (<= (car budget) 0) (> depth 8)) 0)
            ((%registered-for obj)
             => (lambda (c) (comparator-hash c obj)))
            ((pair? obj)
             (%combine (%slow-hash (car obj) budget (+ depth 1))
                       (%slow-hash (cdr obj) budget depth)))
            ((vector? obj)
             (let loop ((i 0) (h (vector-length obj)))
               (if (or (= i (vector-length obj)) (<= (car budget) 0))
                   h
                   (loop (+ i 1)
                         (%combine h (%slow-hash (vector-ref obj i)
                                                 budget (+ depth 1)))))))
            (else (%equal-hash obj))))

    (define %default-comparator
      (make-comparator #t %default=? %default<? default-hash))

    (define (make-default-comparator) %default-comparator)

    (define %eq-comparator
      (make-comparator #t eq? %no-order %eq-hash))
    (define %eqv-comparator
      (make-comparator #t eqv? %no-order %eqv-hash))
    (define %equal-comparator
      (make-comparator #t equal? %no-order default-hash))

    (define (make-eq-comparator) %eq-comparator)
    (define (make-eqv-comparator) %eqv-comparator)
    (define (make-equal-comparator) %equal-comparator))
)
