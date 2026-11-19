;;; requirement: (srfi 146) mappings
;;; found-in: SRFI audit
;;; kind: scheme

;; The SRFI's own tests (by Marc Nieper-Wisskirchen), which already use
;; SRFI 64, and more for the ordered procedures, the shape of the tree
;; under many updates, and the ends of the ranges.

(import (scheme base) (srfi 1) (srfi 8) (srfi 64) (srfi 128) (srfi 132) (srfi 146))

(test-begin "srfi-146")
(test-assert (memq 'srfi-146 (features)))

;; The library does not register mapping-comparator with the default
;; comparator, so the default comparator hashes as it did before.
(let ((t (list 1 "a" #(2))))
  (test-equal (default-hash t) (comparator-hash (make-equal-comparator) t)))

;; The SRFI's tests use mappings as keys of mappings, which needs the
;; default comparator to know mappings; a program asks for that itself.
(comparator-register-default! mapping-comparator)

(define comparator (make-default-comparator))

(test-group "Predicates"
	(define mapping0 (mapping comparator))
	(define mapping1 (mapping comparator 'a 1 'b 2 'c 3))
	(define mapping2 (mapping comparator 'c 1 'd 2 'e 3))
	(define mapping3 (mapping comparator 'd 1 'e 2 'f 3))

	(test-assert "mapping?: a mapping"
	  (mapping? (mapping comparator)))

	(test-assert "mapping?: not a mapping"
	  (not (mapping? (list 1 2 3))))

	(test-assert "mapping-empty?: empty mapping"
	  (mapping-empty? mapping0))

	(test-assert "mapping-empty?: non-empty mapping"
	  (not (mapping-empty? mapping1)))

	(test-assert "mapping-contains?: containing"
	  (mapping-contains? mapping1 'b))

	(test-assert "mapping-contains?: not containing"
	  (not (mapping-contains? mapping1 '2)))

	(test-assert "mapping-disjoint?: disjoint"
	  (mapping-disjoint? mapping1 mapping3))

	(test-assert "mapping-disjoint?: not disjoint"
	  (not (mapping-disjoint? mapping1 mapping2))))

      (test-group "Accessors"
	(define mapping1 (mapping comparator 'a 1 'b 2 'c 3))

	(test-equal "mapping-ref: key found"
	  2
	  (mapping-ref mapping1 'b))

	(test-equal "mapping-ref: key not found/with failure"
	  42
	  (mapping-ref mapping1 'd (lambda () 42)))

	(test-error "mapping-ref: key not found/without failure"
	  (mapping-ref mapping1 'd))

	(test-equal "mapping-ref: with success procedure"
	  (* 2 2)
	  (mapping-ref mapping1 'b (lambda () #f) (lambda (x) (* x x))))

	(test-equal "mapping-ref/default: key found"
	  3
	  (mapping-ref/default mapping1 'c 42))

	(test-equal "mapping-ref/default: key not found"
	  42
	  (mapping-ref/default mapping1 'd 42))

	(test-equal "mapping-key-comparator"
	  comparator
	  (mapping-key-comparator mapping1)))

      (test-group "Updaters"
	(define mapping1 (mapping comparator 'a 1 'b 2 'c 3))
	(define mapping2 (mapping-set mapping1 'c 4 'd 4 'd 5))
	(define mapping3 (mapping-update mapping1 'b (lambda (x) (* x x))))
	(define mapping4 (mapping-update/default mapping1 'd (lambda (x) (* x x)) 4))
	(define mapping5 (mapping-adjoin mapping1 'c 4 'd 4 'd 5))
	(define mapping0 (mapping comparator))

	(test-equal "mapping-adjoin: key already in mapping"
	  3
	  (mapping-ref mapping5 'c))

	(test-equal "mapping-adjoin: key set earlier"
	  4
	  (mapping-ref mapping5 'd))

	(test-equal "mapping-set: key already in mapping"
	  4
	  (mapping-ref mapping2 'c))

	(test-equal "mapping-set: key set earlier"
	  5
	  (mapping-ref mapping2 'd))

	(test-equal "mapping-replace: key not in mapping"
	  #f
	  (mapping-ref/default (mapping-replace mapping1 'd 4) 'd #f))

	(test-equal "mapping-replace: key in mapping"
	  6
	  (mapping-ref (mapping-replace mapping1 'c 6) 'c))

	(test-equal "mapping-delete"
	  42
	  (mapping-ref/default (mapping-delete mapping1 'b) 'b 42))

	(test-equal "mapping-delete-all"
	  42
	  (mapping-ref/default (mapping-delete-all mapping1 '(a b)) 'b 42))

	(test-equal "mapping-intern: key in mapping"
	  (list mapping1 2)
	  (receive result
	      (mapping-intern mapping1 'b (lambda () (error "should not have been invoked")))
	    result))

	(test-equal "mapping-intern: key not in mapping"
	  (list 42 42)
	  (receive (mapping value)
	      (mapping-intern mapping1 'd (lambda () 42))
	    (list value (mapping-ref mapping 'd))))

	(test-equal "mapping-update"
	  4
	  (mapping-ref mapping3 'b))

	(test-equal "mapping-update/default"
	  16
	  (mapping-ref mapping4 'd))

	(test-equal "mapping-pop: empty mapping"
	  'empty
	  (mapping-pop mapping0 (lambda () 'empty)))

	(test-equal "mapping-pop: non-empty mapping"
	  (list 2 'a 1)
	  (receive (mapping key value)
	      (mapping-pop mapping1)
	    (list (mapping-size mapping) key value)))

	(test-equal
	    '("success updated"
	      "failure ignored"
	      ((0 . "zero") (1 . "one") (2 . "two [seen]") (3 . "three")
	       (4 . "four") (5 . "five")))
          (let ((m1 (mapping (make-default-comparator)
                             1 "one"
                             3 "three"
                             0 "zero"
                             4 "four"
                             2 "two"
                             5 "five")))
            (define (f/ignore insert ignore)
              (ignore "failure ignored"))
            (define (s/update key val update remove)
              (update key
                      (string-append val " [seen]")
                      "success updated"))
            (let*-values (((m2 v2) (mapping-search m1 2 f/ignore s/update))
                          ((m3 v3) (mapping-search m2 42 f/ignore s/update)))
              (list v2 v3 (mapping->alist m3))))))

      (test-group "The whole mapping"
	(define mapping0 (mapping comparator))
	(define mapping1 (mapping comparator 'a 1 'b 2 'c 3))

	(test-equal "mapping-size: empty mapping"
	  0
	  (mapping-size mapping0))

	(test-equal "mapping-size: non-empty mapping"
	  3
	  (mapping-size mapping1))

	(test-equal "mapping-find: found in mapping"
	  (list 'b 2)
	  (receive result
	      (mapping-find (lambda (key value)
			  (and (eq? key 'b)
			       (= value 2)))
			mapping1
			(lambda () (error "should not have been called")))
	    result))

	(test-equal "mapping-find: not found in mapping"
	  (list 42)
	  (receive result
	      (mapping-find (lambda (key value)
			  (eq? key 'd))
			mapping1
			(lambda ()
			  42))
	    result))

	(test-equal "mapping-count"
	  2
	  (mapping-count (lambda (key value)
		       (>= value 2))
		     mapping1))

	(test-assert "mapping-any?: found"
	  (mapping-any? (lambda (key value)
		      (= value 3))
		    mapping1))

	(test-assert "mapping-any?: not found"
	  (not (mapping-any? (lambda (key value)
			   (= value 4))
			 mapping1)))

	(test-assert "mapping-every?: true"
	  (mapping-every? (lambda (key value)
			(<= value 3))
		      mapping1))

	(test-assert "mapping-every?: false"
	  (not (mapping-every? (lambda (key value)
			     (<= value 2))
			   mapping1)))

	(test-equal "mapping-keys"
	  3
	  (length (mapping-keys mapping1)))

	(test-equal "mapping-values"
	  6
	  (fold + 0 (mapping-values mapping1)))

	(test-equal "mapping-entries"
	  (list 3 6)
	  (receive (keys values)
	      (mapping-entries mapping1)
	    (list (length keys) (fold + 0 values)))))

      (test-group "Mapping and folding"
  (define mapping1 (mapping comparator 'a 1 'b 2 'c 3))
  (define mapping2 (mapping-map (lambda (key value)
				(values (symbol->string key)
					(* 10 value)))
			      comparator
			      mapping1))
  (test-equal "mapping-map"
	  20
	  (mapping-ref mapping2 "b"))
  (test-equal "mapping-for-each"
	  6
	  (let ((counter 0))
	    (mapping-for-each (lambda (key value)
			    (set! counter (+ counter value)))
			  mapping1)
	    counter))
  (test-equal "mapping-fold"
	  6
	  (mapping-fold (lambda (key value acc)
		      (+ value acc))
		    0
		    mapping1))
  (test-equal "mapping-map->list"
	  (+ (* 1 1) (* 2 2) (* 3 3))
	  (fold + 0 (mapping-map->list (lambda (key value)
				     (* value value))
				   mapping1)))
  (test-equal "mapping-filter"
	  2
	  (mapping-size (mapping-filter (lambda (key value)
				  (<= value 2))
				mapping1)))
  (test-equal "mapping-remove"
	  1
	  (mapping-size (mapping-remove (lambda (key value)
				  (<= value 2))
				mapping1)))
  (test-equal "mapping-partition"
	  (list 1 2)
	  (receive result
	      (mapping-partition (lambda (key value)
			       (eq? 'b key))
			     mapping1)
	    (map mapping-size result)))
  (test-group "Copying and conversion"
	  (define mapping1 (mapping comparator 'a 1 'b 2 'c 3))
	  (define mapping2 (alist->mapping comparator '((a . 1) (b . 2) (c . 3))))
	  (define mapping3 (alist->mapping! (mapping-copy mapping1) '((d . 4) '(c . 5))))

	  (test-equal "mapping-copy: same size"
	    3
	    (mapping-size (mapping-copy mapping1)))

	  (test-equal "mapping-copy: same comparator"
	    comparator
	    (mapping-key-comparator (mapping-copy mapping1)))

	  (test-equal "mapping->alist"
	    (cons 'b 2)
	    (assq 'b (mapping->alist mapping1)))

	  (test-equal "alist->mapping"
	    2
	    (mapping-ref mapping2 'b)
	    )

	  (test-equal "alist->mapping!: new key"
	    4
	    (mapping-ref mapping3 'd))

	  (test-equal "alist->mapping!: existing key"
	    3
	    (mapping-ref mapping3 'c)))
  (test-group "Submappings"
	  (define mapping1 (mapping comparator 'a 1 'b 2 'c 3))
	  (define mapping2 (mapping comparator 'a 1 'b 2 'c 3))
	  (define mapping3 (mapping comparator 'a 1 'c 3))
	  (define mapping4 (mapping comparator 'a 1 'c 3 'd 4))
	  (define mapping5 (mapping comparator 'a 1 'b 2 'c 6))
	  (define mapping6 (mapping (make-comparator (comparator-type-test-predicate comparator)
						     (comparator-equality-predicate comparator)
						     (comparator-ordering-predicate comparator)
						     (lambda (obj) 42))
				    'a 1 'b 2 'c 3))


	  (test-assert "mapping=?: equal mappings"
	    (mapping=? comparator mapping1 mapping2))

	  (test-assert "mapping=?: unequal mappings"
	    (not (mapping=? comparator mapping1 mapping4)))

	  (test-assert "mapping=?: different comparators"
	    (not (mapping=? comparator mapping1 mapping6)))

	  (test-assert "mapping<?: proper subset"
	    (mapping<? comparator mapping3 mapping1))

	  (test-assert "mapping<?: improper subset"
	    (not (mapping<? comparator mapping3 mapping1 mapping2)))

	  (test-assert "mapping>?: proper superset"
	    (mapping>? comparator mapping2 mapping3))

	  (test-assert "mapping>?: improper superset"
	    (not (mapping>? comparator mapping1 mapping2 mapping3)))

	  (test-assert "mapping<=?: subset"
	    (mapping<=? comparator mapping3 mapping2 mapping1))

	  (test-assert "mapping<=?: non-matching values"
	    (not (mapping<=? comparator mapping3 mapping5)))

	  (test-assert "mapping<=?: not a subset"
	    (not (mapping<=? comparator mapping2 mapping4)))

	  (test-assert "mapping>=?: superset"
	    (mapping>=? comparator mapping4 mapping3))

	  (test-assert "mapping>=?: not a superset"
	    (not (mapping>=? comparator mapping5 mapping3))))
  (test-group "Set theory operations"
	  (define mapping1 (mapping comparator 'a 1 'b 2 'c 3))
	  (define mapping2 (mapping comparator 'a 1 'b 2 'd 4))
	  (define mapping3 (mapping comparator 'a 1 'b 2))
	  (define mapping4 (mapping comparator 'a 1 'b 2 'c 4))
	  (define mapping5 (mapping comparator 'a 1 'c 3))
	  (define mapping6 (mapping comparator 'd 4 'e 5 'f 6))

	  (test-equal "mapping-union: new association"
	    4
	    (mapping-ref (mapping-union mapping1 mapping2) 'd))

	  (test-equal "mapping-union: existing association"
	    3
	    (mapping-ref (mapping-union mapping1 mapping4) 'c))

	  (test-equal "mapping-union: three mappings"
	    6
	    (mapping-size (mapping-union mapping1 mapping2 mapping6)))

	  (test-equal "mapping-intersection: existing association"
	    3
	    (mapping-ref (mapping-intersection mapping1 mapping4) 'c))

	  (test-equal "mapping-intersection: removed association"
	    42
	    (mapping-ref/default (mapping-intersection mapping1 mapping5) 'b 42))

	  (test-equal "mapping-difference"
	    2
	    (mapping-size (mapping-difference mapping2 mapping6)))

	  (test-equal "mapping-xor"
	    4
	    (mapping-size (mapping-xor mapping2 mapping6))))
  (test-group "Additional procedures for mappings with ordered keys"
	  (define mapping1 (mapping comparator 'a 1 'b 2 'c 3))
	  (define mapping2 (mapping comparator 'a 1 'b 2 'c 3 'd 4))
	  (define mapping3 (mapping comparator 'a 1 'b 2 'c 3 'd 4 'e 5))
	  (define mapping4 (mapping comparator 'a 1 'b 2 'c 3 'd 4 'e 5 'f 6))
	  (define mapping5 (mapping comparator 'f 6 'g 7 'h 8))

	  (test-equal "mapping-min-key"
	    '(a a a a)
	    (map mapping-min-key (list mapping1 mapping2 mapping3 mapping4)))

	  (test-equal "mapping-max-key"
	    '(c d e f)
	    (map mapping-max-key (list mapping1 mapping2 mapping3 mapping4)))

	  (test-equal "mapping-min-value"
	    '(1 1 1 1)
	    (map mapping-min-value (list mapping1 mapping2 mapping3 mapping4)))

	  (test-equal "mapping-max-value"
	    '(3 4 5 6)
	    (map mapping-max-value (list mapping1 mapping2 mapping3 mapping4)))

	  (test-equal "mapping-key-predecessor"
	    '(c d d d)
	    (map (lambda (mapping)
		   (mapping-key-predecessor mapping 'e (lambda () #f)))
		 (list mapping1 mapping2 mapping3 mapping4)))

	  (test-equal "mapping-key-successor"
	    '(#f #f e e)
	    (map (lambda (mapping)
		   (mapping-key-successor mapping 'd (lambda () #f)))
		 (list mapping1 mapping2 mapping3 mapping4)))

	  (test-equal "mapping-range=: contained"
	    '(4)
	    (mapping-values (mapping-range= mapping4 'd)))

	  (test-equal "mapping-range=: not contained"
	    '()
	    (mapping-values (mapping-range= mapping4 'z)))

	  (test-equal "mapping-range<"
	    '(1 2 3)
	    (mapping-values (mapping-range< mapping4 'd)))

	  (test-equal "mapping-range<="
	    '(1 2 3 4)
	    (mapping-values (mapping-range<= mapping4 'd)))

	  (test-equal "mapping-range>"
	    '(5 6)
	    (mapping-values (mapping-range> mapping4 'd)))

	  (test-equal "mapping-range>="
	    '(4 5 6)
	    (mapping-values (mapping-range>= mapping4 'd)))

	  (test-equal "mapping-split"
	    '((1 2 3) (1 2 3 4) (4) (4 5 6) (5 6))
	    (receive mappings
		(mapping-split mapping4 'd)
	      (map mapping-values mappings)))

	  (test-equal "mapping-catenate"
	    '((a . 1) (b . 2) (c . 3) (d . 4) (e . 5) (f . 6) (g . 7) (h . 8))
	    (mapping->alist (mapping-catenate comparator mapping2 'e 5 mapping5)))

	  (test-equal "mapping-map/monotone"
	    '((1 . 1) (2 . 4) (3 . 9))
	    (mapping->alist
	     (mapping-map/monotone (lambda (key value)
				     (values value (* value value)))
				   comparator
				   mapping1)))

	  (test-equal "mapping-fold/reverse"
	    '(1 2 3)
	    (mapping-fold/reverse (lambda (key value acc)
				    (cons value acc))
				  '() mapping1)))
  (test-group "Comparators"
  (define mapping1 (mapping comparator 'a 1 'b 2 'c 3))
  (define mapping2 (mapping comparator 'a 1 'b 2 'c 3))
  (define mapping3 (mapping comparator 'a 1 'b 2))
  (define mapping4 (mapping comparator 'a 1 'b 2 'c 4))
  (define mapping5 (mapping comparator 'a 1 'c 3))
  (define mapping0 (mapping comparator mapping1 "a" mapping2 "b" mapping3 "c" mapping4 "d" mapping5 "e"))
  (test-assert "mapping-comparator"
	    (comparator? mapping-comparator))
  (test-equal "mapping-keyed mapping"
	    (list "a" "a" "c" "d" "e")
	    (list (mapping-ref mapping0 mapping1)
		  (mapping-ref mapping0 mapping2)
		  (mapping-ref mapping0 mapping3)
		  (mapping-ref mapping0 mapping4)
		  (mapping-ref mapping0 mapping5)))
  (test-group "Ordering comparators"
	    (test-assert "=?: equal mappings"
	      (=? comparator mapping1 mapping2))

	    (test-assert "=?: unequal mappings"
	      (not (=? comparator mapping1 mapping4)))

	    (test-assert "<?: case 1"
	      (<? comparator mapping3 mapping4))

	    (test-assert "<?: case 2"
	      (<? comparator mapping1 mapping4))

	    (test-assert "<?: case 3"
	      (<? comparator mapping1 mapping5)))))

      
(define int-comparator
  (make-comparator exact-integer? = < (lambda (n) (abs n))))

;; A pseudo-random sequence, the same every run.
(define (make-rng seed)
  (lambda (n)
    (set! seed (modulo (+ (* seed 1103515245) 12345) 2147483648))
    (modulo (quotient seed 65536) n)))

(define (model-set model k v)
  (cons (cons k v) (filter (lambda (p) (not (= (car p) k))) model)))
(define (model-sorted model)
  (list-sort (lambda (a b) (< (car a) (car b))) model))

(test-group "more"
  ;; Many updates against a model, checking order, size and lookups.
  (let ((rng (make-rng 42)))
    (let loop ((i 0) (m (mapping int-comparator)) (model '()))
      (cond
       ((= i 3000)
        (test-equal (model-sorted model) (mapping->alist m))
        (test-equal (length model) (mapping-size m))
        (test-equal (map car (model-sorted model)) (mapping-keys m))
        (test-equal (map cdr (model-sorted model)) (mapping-values m)))
       (else
        (let ((k (rng 200)) (op (rng 3)))
          (case op
            ((0 1) (loop (+ i 1) (mapping-set m k i) (model-set model k i)))
            (else (loop (+ i 1) (mapping-delete m k)
                        (filter (lambda (p) (not (= (car p) k))) model)))))))))
  ;; Persistence: an old mapping is unchanged by what came after it.
  (let* ((m0 (mapping int-comparator 1 'a 2 'b))
         (m1 (mapping-set m0 3 'c))
         (m2 (mapping-delete m1 1)))
    (test-equal '((1 . a) (2 . b)) (mapping->alist m0))
    (test-equal '((1 . a) (2 . b) (3 . c)) (mapping->alist m1))
    (test-equal '((2 . b) (3 . c)) (mapping->alist m2)))
  ;; Big ordered and reverse-ordered builds stay balanced enough to be fast.
  (let loop ((i 0) (m (mapping int-comparator)))
    (if (< i 5000)
        (loop (+ i 1) (mapping-set m i (* i i)))
        (begin
          (test-equal 5000 (mapping-size m))
          (test-equal 0 (mapping-min-key m))
          (test-equal 4999 (mapping-max-key m))
          (test-equal 2500 (mapping-size (mapping-range< m 2500)))
          (test-equal 2500 (mapping-size (mapping-range>= m 2500)))
          (test-equal 2499 (mapping-size (mapping-range> m 2500)))
          (let-values (((lo lo= eq hi= hi) (mapping-split m 1000)))
            (test-equal '(1000 1001 1 4000 3999)
                        (list (mapping-size lo) (mapping-size lo=)
                              (mapping-size eq) (mapping-size hi=)
                              (mapping-size hi)))
            (test-equal 999 (mapping-max-key lo))
            (test-equal 1001 (mapping-min-key hi))
            (let ((back (mapping-catenate int-comparator lo 1000 'x hi)))
              (test-equal 5000 (mapping-size back))
              (test-equal 'x (mapping-ref back 1000))
              (test-equal (mapping-keys m) (mapping-keys back)))))))
  ;; Catenating mappings of very different sizes.
  (let ((small (mapping int-comparator 1 'a))
        (big (alist->mapping int-comparator
                             (map (lambda (i) (cons i i)) (iota 1000 10)))))
    (test-equal 1002 (mapping-size (mapping-catenate int-comparator small 5 'm big)))
    (test-equal 1002 (mapping-size (mapping-catenate int-comparator big 2000 'm
                                                     (mapping int-comparator 3000 'z))))
    (test-equal '((1 . a) (5 . m) (10 . 10))
                (take (mapping->alist (mapping-catenate int-comparator small 5 'm big)) 3)))
  ;; The ordered queries.
  (let ((m (mapping int-comparator 10 'a 20 'b 30 'c)))
    (test-equal 10 (mapping-key-successor m 5 (lambda () 'none)))
    (test-equal 20 (mapping-key-successor m 10 (lambda () 'none)))
    (test-equal 'none (mapping-key-successor m 30 (lambda () 'none)))
    (test-equal 'none (mapping-key-predecessor m 10 (lambda () 'none)))
    (test-equal 20 (mapping-key-predecessor m 25 (lambda () 'none)))
    (test-equal 30 (mapping-key-predecessor m 99 (lambda () 'none)))
    (test-equal 'none (mapping-key-predecessor (mapping int-comparator) 1 (lambda () 'none)))
    (test-equal '(10 a) (call-with-values (lambda () (mapping-min-entry m)) list))
    (test-equal '(30 c) (call-with-values (lambda () (mapping-max-entry m)) list))
    (test-equal 'c (mapping-max-value m))
    (test-error (mapping-min-key (mapping int-comparator)))
    (test-error (mapping-max-value (mapping int-comparator)))
    (test-equal '() (mapping->alist (mapping-range< m 10)))
    (test-equal '((10 . a)) (mapping->alist (mapping-range<= m 10)))
    (test-equal '((20 . b) (30 . c)) (mapping->alist (mapping-range> m 10)))
    (test-equal '((10 . a) (20 . b) (30 . c)) (mapping->alist (mapping-range>= m 10)))
    (test-equal '((10 . a) (20 . b) (30 . c)) (mapping->alist (mapping-range>= m -5)))
    (test-equal '() (mapping->alist (mapping-range> m 30)))
    (test-equal '((20 . b)) (mapping->alist (mapping-range= m 20)))
    (test-equal '((10 . a) (20 . b)) (mapping->alist (mapping-range<=! (mapping-copy m) 25))))
  ;; Searching and popping.
  (let ((m (mapping int-comparator 1 'a 2 'b)))
    (let-values (((m2 obj) (mapping-search m 3
                                           (lambda (insert ignore) (insert 'c 'inserted))
                                           (lambda (k v update remove) (remove 'removed)))))
      (test-equal 'inserted obj)
      (test-equal '((1 . a) (2 . b) (3 . c)) (mapping->alist m2)))
    (let-values (((m2 obj) (mapping-search m 2
                                           (lambda (insert ignore) (ignore 'ignored))
                                           (lambda (k v update remove) (remove (list k v))))))
      (test-equal '(2 b) obj)
      (test-equal '((1 . a)) (mapping->alist m2)))
    (let-values (((m2 obj) (mapping-search m 1
                                           (lambda (insert ignore) (ignore 'ignored))
                                           (lambda (k v update remove) (update 1 'z 'updated)))))
      (test-equal 'updated obj)
      (test-equal '((1 . z) (2 . b)) (mapping->alist m2)))
    (let-values (((m2 k v) (mapping-pop m)))
      (test-equal '(1 a ((2 . b))) (list k v (mapping->alist m2))))
    (test-equal 'empty (mapping-pop (mapping int-comparator) (lambda () 'empty)))
    (test-error (mapping-pop (mapping int-comparator)))
    (test-equal '(1 a) (call-with-values (lambda () (mapping-find (lambda (k v) #t) m (lambda () #f))) list)))
  ;; Precedence: earlier wins for constructors and adjoin, later for set.
  (test-equal '((1 . a)) (mapping->alist (mapping int-comparator 1 'a 1 'b)))
  (test-equal '((1 . b)) (mapping->alist (mapping-set (mapping int-comparator) 1 'a 1 'b)))
  (test-equal '((1 . a)) (mapping->alist (alist->mapping int-comparator '((1 . a) (1 . b)))))
  (test-equal '((1 . a) (2 . b)) (mapping->alist (mapping-unfold (lambda (i) (> i 2)) (lambda (i) (values i (if (= i 1) 'a 'b))) (lambda (i) (+ i 1)) 1 int-comparator)))
  ;; Set operations take their associations from the first mapping.
  (let ((a (mapping int-comparator 1 'a 2 'a))
        (b (mapping int-comparator 2 'b 3 'b))
        (c (mapping int-comparator 3 'c 4 'c)))
    (test-equal '((1 . a) (2 . a) (3 . b) (4 . c)) (mapping->alist (mapping-union a b c)))
    (test-equal '((2 . a)) (mapping->alist (mapping-intersection a b)))
    (test-equal '() (mapping->alist (mapping-intersection a b c)))
    (test-equal '((1 . a)) (mapping->alist (mapping-difference a b)))
    (test-equal '((1 . a)) (mapping->alist (mapping-difference a b c)))
    (test-equal '((1 . a) (3 . b)) (mapping->alist (mapping-xor a b)))
    (test-equal '((1 . a) (2 . a)) (mapping->alist (mapping-union a)))
    (test-assert (mapping=? comparator a a (mapping-copy a)))
    (test-assert (mapping<? comparator (mapping int-comparator 1 'a) a))
    (test-assert (not (mapping<? comparator a a)))
    (test-assert (mapping<=? comparator a a))
    (test-assert (mapping>? comparator a (mapping int-comparator 1 'a)))
    (test-assert (mapping>=? comparator a a))
    (test-assert (not (mapping=? comparator a b)))
    (test-assert (not (mapping=? comparator a (mapping (make-default-comparator) 1 'a 2 'a))))
    (test-assert (mapping-empty? (mapping-difference a a)))
    (test-assert (mapping-disjoint? a (mapping int-comparator 5 'x))))
  ;; Errors.
  (test-error (mapping int-comparator 1))
  (test-error (mapping-adjoin (mapping int-comparator) 1))
  (test-error (mapping int-comparator 'a 1))
  (test-error (mapping (make-comparator #t equal? #f default-hash)))
  (test-error (mapping-ref (mapping int-comparator) 1))
  (test-error (mapping-set 5 1 2))
  (test-error (mapping-update (mapping int-comparator) 1 (lambda (x) x)))
  (test-equal '((1 . 1)) (mapping->alist (mapping-update/default (mapping int-comparator) 1 (lambda (x) (+ x 1)) 0)))
  (test-equal '(2 . ((1 . 2))) (let-values (((m v) (mapping-intern (mapping int-comparator) 1 (lambda () 2)))) (cons v (list (car (mapping->alist m))))))
  ;; Mapping keys and the registered comparator.
  (test-assert (=? (make-default-comparator) (mapping int-comparator 1 'a) (mapping int-comparator 1 'a)))
  (test-assert (<? (make-default-comparator) (mapping int-comparator 1 'a) (mapping int-comparator 2 'a)))
  (test-assert (= (default-hash (mapping int-comparator 1 'a 2 'b))
                  (default-hash (mapping int-comparator 2 'b 1 'a))))
  (let ((mc (make-mapping-comparator comparator)))
    (test-assert (<? mc (mapping int-comparator 1 'a) (mapping int-comparator 1 'a 2 'b)))
    (test-assert (not (<? mc (mapping int-comparator 2 'a) (mapping int-comparator 1 'a 2 'b))))
    (test-assert (<? mc (mapping int-comparator 1 'a) (mapping int-comparator 1 'b)))))

(test-group "review fixes"
  (define c (make-comparator exact-integer? = < (lambda (n) (abs n))))
  ;; mapping-catenate refuses keys out of order, and nothing is damaged.
  (let ((m1 (mapping c 5 'a)) (m2 (mapping c 0 'z)))
    (test-error (mapping-catenate c m1 1 'b m2))
    (test-error (mapping-catenate c m1 9 'b m2))
    (test-error (mapping-catenate c m1 5 'b (mapping c 7 'q)))
    (test-error (mapping-catenate c (mapping c 1 'a) 1 'b (mapping c 7 'q)))
    (test-error (mapping-catenate c (mapping c 1 'a) 3 'b (mapping c 3 'q)))
    (test-equal '((5 . a)) (mapping->alist m1))
    (test-assert (mapping-contains? m1 5))
    (test-equal '((1 . a) (3 . b) (7 . q))
                (mapping->alist (mapping-catenate c (mapping c 1 'a) 3 'b (mapping c 7 'q))))
    (test-equal '((3 . b)) (mapping->alist (mapping-catenate c (mapping c) 3 'b (mapping c))))
    (test-equal '((3 . b) (7 . q)) (mapping->alist (mapping-catenate c (mapping c) 3 'b (mapping c 7 'q)))))
  ;; Hashing mappings that contain themselves ends.
  (let* ((cc (make-comparator vector? (lambda (a b) (eq? a b))
                              (lambda (a b) (< (vector-ref a 1) (vector-ref b 1)))
                              (lambda (v)
                                (if (vector-ref v 0)
                                    (comparator-hash mapping-comparator (vector-ref v 0))
                                    0))))
         (v (vector #f 1))
         (m (mapping cc v 'x)))
    (vector-set! v 0 m)
    (test-assert (integer? (comparator-hash mapping-comparator m))))
  ;; A mapping hashes by its keys, so equal mappings hash alike, and a key
  ;; comparator with no hash function does not break it.
  (let ((m (mapping (make-comparator real? = < #f) 1 'a 2 'b)))
    (test-assert (integer? (comparator-hash mapping-comparator m)))
    (test-assert (= (comparator-hash mapping-comparator m)
                    (comparator-hash mapping-comparator
                                     (mapping (make-comparator real? = < #f) 2 'b 1 'a)))))
  (test-assert (= (comparator-hash mapping-comparator (mapping c 1 'a 2 'b 3 'c 4 'd 5 'e 6 'f))
                  (comparator-hash mapping-comparator
                                   (mapping c 6 'f 5 'e 4 'd 3 'c 2 'b 1 'a))))
  (test-assert (not (= (comparator-hash mapping-comparator (mapping c 1 'a))
                       (comparator-hash mapping-comparator (mapping c 1 'a 2 'b)))))
  ;; Hashed by keys only: equal in the comparator's sense means equal hash.
  (let ((mc (make-mapping-comparator (make-comparator number? = < number-hash))))
    (test-assert (=? mc (mapping c 1 2) (mapping c 1 2.0)))
    (test-assert (= (comparator-hash mc (mapping c 1 2)) (comparator-hash mc (mapping c 1 2.0))))))

(test-end "srfi-146")
