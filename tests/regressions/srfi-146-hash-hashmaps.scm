;;; requirement: (srfi 146 hash) hashmaps
;;; found-in: SRFI audit
;;; kind: scheme

;; The SRFI's own tests for hashmaps (by Marc Nieper-Wisskirchen), which
;; already use SRFI 64, and more for the linear-update procedures, which
;; change a hashmap in place, and the pure ones, which never do.

(import (scheme base) (scheme char) (srfi 1) (srfi 8) (srfi 64) (srfi 128) (srfi 132) (srfi 146 hash))

(test-begin "srfi-146-hash")
(test-assert (memq 'srfi-146 (features)))

;; The library does not register hashmap-comparator with the default
;; comparator, so the default comparator hashes as it did before.
(let ((t (list 1 "a" #(2))))
  (test-equal (default-hash t) (comparator-hash (make-equal-comparator) t)))

;; The SRFI's tests use hashmaps as keys of hashmaps, which needs the
;; default comparator to know hashmaps; a program asks for that itself.
(comparator-register-default! hashmap-comparator)

(define comparator (make-default-comparator))

(test-group "Predicates"
	(define hashmap0 (hashmap comparator))
	(define hashmap1 (hashmap comparator 'a 1 'b 2 'c 3))
	(define hashmap2 (hashmap comparator 'c 1 'd 2 'e 3))
	(define hashmap3 (hashmap comparator 'd 1 'e 2 'f 3))

	(test-assert "hashmap?: a hashmap"
	  (hashmap? (hashmap comparator)))

	(test-assert "hashmap?: not a hashmap"
	  (not (hashmap? (list 1 2 3))))

	(test-assert "hashmap-empty?: empty hashmap"
	  (hashmap-empty? hashmap0))

	(test-assert "hashmap-empty?: non-empty hashmap"
	  (not (hashmap-empty? hashmap1)))

	(test-assert "hashmap-contains?: containing"
	  (hashmap-contains? hashmap1 'b))

	(test-assert "hashmap-contains?: not containing"
	  (not (hashmap-contains? hashmap1 '2)))

	(test-assert "hashmap-disjoint?: disjoint"
	  (hashmap-disjoint? hashmap1 hashmap3))

	(test-assert "hashmap-disjoint?: not disjoint"
	  (not (hashmap-disjoint? hashmap1 hashmap2))))

      (test-group "Accessors"
	(define hashmap1 (hashmap comparator 'a 1 'b 2 'c 3))

	(test-equal "hashmap-ref: key found"
	  2
	  (hashmap-ref hashmap1 'b))

	(test-equal "hashmap-ref: key not found/with failure"
	  42
	  (hashmap-ref hashmap1 'd (lambda () 42)))

	(test-error "hashmap-ref: key not found/without failure"
	  (hashmap-ref hashmap1 'd))

	(test-equal "hashmap-ref: with success procedure"
	  (* 2 2)
	  (hashmap-ref hashmap1 'b (lambda () #f) (lambda (x) (* x x))))

	(test-equal "hashmap-ref/default: key found"
	  3
	  (hashmap-ref/default hashmap1 'c 42))

	(test-equal "hashmap-ref/default: key not found"
	  42
	  (hashmap-ref/default hashmap1 'd 42))

	(test-equal "hashmap-key-comparator"
	  comparator
	  (hashmap-key-comparator hashmap1)))

      (test-group "Updaters"
	(define hashmap1 (hashmap comparator 'a 1 'b 2 'c 3))
	(define hashmap2 (hashmap-set hashmap1 'c 4 'd 4 'd 5))
	(define hashmap3 (hashmap-update hashmap1 'b (lambda (x) (* x x))))
	(define hashmap4 (hashmap-update/default hashmap1 'd (lambda (x) (* x x)) 4))
	(define hashmap5 (hashmap-adjoin hashmap1 'c 4 'd 4 'd 5))
	(define hashmap0 (hashmap comparator))

	(test-equal "hashmap-adjoin: key already in hashmap"
	  3
	  (hashmap-ref hashmap5 'c))

	(test-equal "hashmap-adjoin: key set earlier"
	  4
	  (hashmap-ref hashmap5 'd))

	(test-equal "hashmap-set: key already in hashmap"
	  4
	  (hashmap-ref hashmap2 'c))

	(test-equal "hashmap-set: key set earlier"
	  5
	  (hashmap-ref hashmap2 'd))

	(test-equal "hashmap-replace: key not in hashmap"
	  #f
	  (hashmap-ref/default (hashmap-replace hashmap1 'd 4) 'd #f))

	(test-equal "hashmap-replace: key in hashmap"
	  6
	  (hashmap-ref (hashmap-replace hashmap1 'c 6) 'c))

	(test-equal "hashmap-delete"
	  42
	  (hashmap-ref/default (hashmap-delete hashmap1 'b) 'b 42))

	(test-equal "hashmap-delete-all"
	  42
	  (hashmap-ref/default (hashmap-delete-all hashmap1 '(a b)) 'b 42))

	(test-equal "hashmap-intern: key in hashmap"
	  (list hashmap1 2)
	  (receive result
	      (hashmap-intern hashmap1 'b (lambda () (error "should not have been invoked")))
	    result))

	(test-equal "hashmap-intern: key not in hashmap"
	  (list 42 42)
	  (receive (hashmap value)
	      (hashmap-intern hashmap1 'd (lambda () 42))
	    (list value (hashmap-ref hashmap 'd))))

	(test-equal "hashmap-update"
	  4
	  (hashmap-ref hashmap3 'b))

	(test-equal "hashmap-update/default"
	  16
	  (hashmap-ref hashmap4 'd))

	(test-equal "hashmap-pop: empty hashmap"
	  'empty
	  (hashmap-pop hashmap0 (lambda () 'empty)))

	(test-assert "hashmap-pop: non-empty hashmap"
	  (member
	   (receive (hashmap key value)
	       (hashmap-pop hashmap1)
	       (list (hashmap-size hashmap) key value))
	   '((2 a 1) (2 b 2) (2 c 3)))))

      (test-group "The whole hashmap"
	(define hashmap0 (hashmap comparator))
	(define hashmap1 (hashmap comparator 'a 1 'b 2 'c 3))

	(test-equal "hashmap-size: empty hashmap"
	  0
	  (hashmap-size hashmap0))

	(test-equal "hashmap-size: non-empty hashmap"
	  3
	  (hashmap-size hashmap1))

	(test-equal "hashmap-find: found in hashmap"
	  (list 'b 2)
	  (receive result
	      (hashmap-find (lambda (key value)
			  (and (eq? key 'b)
			       (= value 2)))
			hashmap1
			(lambda () (error "should not have been called")))
	    result))

	(test-equal "hashmap-find: not found in hashmap"
	  (list 42)
	  (receive result
	      (hashmap-find (lambda (key value)
			  (eq? key 'd))
			hashmap1
			(lambda ()
			  42))
	    result))

	(test-equal "hashmap-count"
	  2
	  (hashmap-count (lambda (key value)
		       (>= value 2))
		     hashmap1))

	(test-assert "hashmap-any?: found"
	  (hashmap-any? (lambda (key value)
		      (= value 3))
		    hashmap1))

	(test-assert "hashmap-any?: not found"
	  (not (hashmap-any? (lambda (key value)
			   (= value 4))
			 hashmap1)))

	(test-assert "hashmap-every?: true"
	  (hashmap-every? (lambda (key value)
			(<= value 3))
		      hashmap1))

	(test-assert "hashmap-every?: false"
	  (not (hashmap-every? (lambda (key value)
			     (<= value 2))
			   hashmap1)))

	(test-equal "hashmap-keys"
	  3
	  (length (hashmap-keys hashmap1)))

	(test-equal "hashmap-values"
	  6
	  (fold + 0 (hashmap-values hashmap1)))

	(test-equal "hashmap-entries"
	  (list 3 6)
	  (receive (keys values)
	      (hashmap-entries hashmap1)
	    (list (length keys) (fold + 0 values)))))

      (test-group "Hashmap and folding"
  (define hashmap1 (hashmap comparator 'a 1 'b 2 'c 3))
  (define hashmap2 (hashmap-map (lambda (key value)
				(values (symbol->string key)
					(* 10 value)))
			      comparator
			      hashmap1))
  (test-equal "hashmap-map"
	  20
	  (hashmap-ref hashmap2 "b"))
  (test-equal "hashmap-for-each"
	  6
	  (let ((counter 0))
	    (hashmap-for-each (lambda (key value)
			    (set! counter (+ counter value)))
			  hashmap1)
	    counter))
  (test-equal "hashmap-fold"
	  6
	  (hashmap-fold (lambda (key value acc)
		      (+ value acc))
		    0
		    hashmap1))
  (test-equal "hashmap-map->list"
	  (+ (* 1 1) (* 2 2) (* 3 3))
	  (fold + 0 (hashmap-map->list (lambda (key value)
				     (* value value))
				   hashmap1)))
  (test-equal "hashmap-filter"
	  2
	  (hashmap-size (hashmap-filter (lambda (key value)
				  (<= value 2))
				hashmap1)))
  (test-equal "hashmap-remove"
	  1
	  (hashmap-size (hashmap-remove (lambda (key value)
				  (<= value 2))
				hashmap1)))
  (test-equal "hashmap-partition"
	  (list 1 2)
	  (receive result
	      (hashmap-partition (lambda (key value)
			       (eq? 'b key))
			     hashmap1)
	    (map hashmap-size result)))
  (test-group "Copying and conversion"
	  (define hashmap1 (hashmap comparator 'a 1 'b 2 'c 3))
	  (define hashmap2 (alist->hashmap comparator '((a . 1) (b . 2) (c . 3))))
	  (define hashmap3 (alist->hashmap! (hashmap-copy hashmap1) '((d . 4) '(c . 5))))

	  (test-equal "hashmap-copy: same size"
	    3
	    (hashmap-size (hashmap-copy hashmap1)))

	  (test-equal "hashmap-copy: same comparator"
	    comparator
	    (hashmap-key-comparator (hashmap-copy hashmap1)))

	  (test-equal "hashmap->alist"
	    (cons 'b 2)
	    (assq 'b (hashmap->alist hashmap1)))

	  (test-equal "alist->hashmap"
	    2
	    (hashmap-ref hashmap2 'b)
	    )

	  (test-equal "alist->hashmap!: new key"
	    4
	    (hashmap-ref hashmap3 'd))

	  (test-equal "alist->hashmap!: existing key"
	    3
	    (hashmap-ref hashmap3 'c)))
  (test-group "Subhashmaps"
	  (define hashmap1 (hashmap comparator 'a 1 'b 2 'c 3))
	  (define hashmap2 (hashmap comparator 'a 1 'b 2 'c 3))
	  (define hashmap3 (hashmap comparator 'a 1 'c 3))
	  (define hashmap4 (hashmap comparator 'a 1 'c 3 'd 4))
	  (define hashmap5 (hashmap comparator 'a 1 'b 2 'c 6))
	  (define hashmap6 (hashmap (make-comparator (comparator-type-test-predicate comparator)
						     (comparator-equality-predicate comparator)
						     (comparator-ordering-predicate comparator)
						     (comparator-hash-function comparator))
				    'a 1 'b 2 'c 3))


	  (test-assert "hashmap=?: equal hashmaps"
	    (hashmap=? comparator hashmap1 hashmap2))

	  (test-assert "hashmap=?: unequal hashmaps"
	    (not (hashmap=? comparator hashmap1 hashmap4)))

	  (test-assert "hashmap=?: different comparators"
	    (not (hashmap=? comparator hashmap1 hashmap6)))

	  (test-assert "hashmap<?: proper subset"
	    (hashmap<? comparator hashmap3 hashmap1))

	  (test-assert "hashmap<?: improper subset"
	    (not (hashmap<? comparator hashmap3 hashmap1 hashmap2)))

	  (test-assert "hashmap>?: proper superset"
	    (hashmap>? comparator hashmap2 hashmap3))

	  (test-assert "hashmap>?: improper superset"
	    (not (hashmap>? comparator hashmap1 hashmap2 hashmap3)))

	  (test-assert "hashmap<=?: subset"
	    (hashmap<=? comparator hashmap3 hashmap2 hashmap1))

	  (test-assert "hashmap<=?: non-matching values"
	    (not (hashmap<=? comparator hashmap3 hashmap5)))

	  (test-assert "hashmap<=?: not a subset"
	    (not (hashmap<=? comparator hashmap2 hashmap4)))

	  (test-assert "hashmap>=?: superset"
	    (hashmap>=? comparator hashmap4 hashmap3))

	  (test-assert "hashmap>=?: not a superset"
	    (not (hashmap>=? comparator hashmap5 hashmap3))))
  (test-group "Set theory operations"
	  (define hashmap1 (hashmap comparator 'a 1 'b 2 'c 3))
	  (define hashmap2 (hashmap comparator 'a 1 'b 2 'd 4))
	  (define hashmap3 (hashmap comparator 'a 1 'b 2))
	  (define hashmap4 (hashmap comparator 'a 1 'b 2 'c 4))
	  (define hashmap5 (hashmap comparator 'a 1 'c 3))
	  (define hashmap6 (hashmap comparator 'd 4 'e 5 'f 6))

	  (test-equal "hashmap-union: new association"
	    4
	    (hashmap-ref (hashmap-union hashmap1 hashmap2) 'd))

	  (test-equal "hashmap-union: existing association"
	    3
	    (hashmap-ref (hashmap-union hashmap1 hashmap4) 'c))

	  (test-equal "hashmap-union: three hashmaps"
	    6
	    (hashmap-size (hashmap-union hashmap1 hashmap2 hashmap6)))

	  (test-equal "hashmap-intersection: existing association"
	    3
	    (hashmap-ref (hashmap-intersection hashmap1 hashmap4) 'c))

	  (test-equal "hashmap-intersection: removed association"
	    42
	    (hashmap-ref/default (hashmap-intersection hashmap1 hashmap5) 'b 42))

	  (test-equal "hashmap-difference"
	    2
	    (hashmap-size (hashmap-difference hashmap2 hashmap6)))

	  (test-equal "hashmap-xor"
	    4
	    (hashmap-size (hashmap-xor hashmap2 hashmap6))))
  (test-group "Comparators"
  (define hashmap1 (hashmap comparator 'a 1 'b 2 'c 3))
  (define hashmap2 (hashmap comparator 'a 1 'b 2 'c 3))
  (define hashmap3 (hashmap comparator 'a 1 'b 2))
  (define hashmap4 (hashmap comparator 'a 1 'b 2 'c 4))
  (define hashmap5 (hashmap comparator 'a 1 'c 3))
  (define hashmap0 (hashmap comparator
				    hashmap1 "a"
				    hashmap2 "b"
				    hashmap3 "c"
				    hashmap4 "d"
				    hashmap5 "e"))
  (test-assert "hashmap-comparator"
	    (comparator? hashmap-comparator))
  (test-equal "hashmap-keyed hashmap"
	    (list "a" "a" "c" "d" "e")
	    (list (hashmap-ref hashmap0 hashmap1)
		  (hashmap-ref hashmap0 hashmap2)
		  (hashmap-ref hashmap0 hashmap3)
		  (hashmap-ref hashmap0 hashmap4)
		  (hashmap-ref hashmap0 hashmap5)
		  ))
  (test-group "Ordering comparators"
	    (test-assert "=?: equal hashmaps"
	      (=? comparator hashmap1 hashmap2))

	    (test-assert "=?: unequal hashmaps"
	      (not (=? comparator hashmap1 hashmap4))))))

      
(define (sorted-alist hm)
  (list-sort (lambda (a b) (< (car a) (car b))) (hashmap->alist hm)))

(test-group "more"
  (define int-comparator (make-comparator exact-integer? = < (lambda (n) (abs n))))
  (define hm (hashmap int-comparator 1 'a 2 'b 3 'c))
  ;; The pure procedures leave their argument alone.
  (let ((h2 (hashmap-set hm 4 'd)) (h3 (hashmap-delete hm 1)) (h4 (hashmap-adjoin hm 1 'z 9 'y))
        (h5 (hashmap-update hm 1 (lambda (x) 'u))) (h6 (hashmap-replace hm 1 'r))
        (h7 (hashmap-filter (lambda (k v) (odd? k)) hm)))
    (test-equal '((1 . a) (2 . b) (3 . c)) (sorted-alist hm))
    (test-equal '((1 . a) (2 . b) (3 . c) (4 . d)) (sorted-alist h2))
    (test-equal '((2 . b) (3 . c)) (sorted-alist h3))
    (test-equal '((1 . a) (2 . b) (3 . c) (9 . y)) (sorted-alist h4))
    (test-equal '((1 . u) (2 . b) (3 . c)) (sorted-alist h5))
    (test-equal '((1 . r) (2 . b) (3 . c)) (sorted-alist h6))
    (test-equal '((1 . a) (3 . c)) (sorted-alist h7))
    (test-equal 3 (hashmap-size hm)))
  ;; The linear-update ones may change it, and return the result.
  (let ((h (hashmap-copy hm)))
    (test-assert (not (eq? h hm)))
    (let ((h2 (hashmap-set! h 4 'd)))
      (test-equal '((1 . a) (2 . b) (3 . c) (4 . d)) (sorted-alist h2)))
    (let ((h2 (hashmap-delete! h 4 1)))
      (test-equal '((2 . b) (3 . c)) (sorted-alist h2)))
    (let ((h2 (hashmap-filter! (lambda (k v) (= k 2)) h)))
      (test-equal '((2 . b)) (sorted-alist h2)))
    (test-equal '((1 . a) (2 . b) (3 . c)) (sorted-alist hm)))
  ;; Precedence.
  (test-equal '((1 . a)) (sorted-alist (hashmap int-comparator 1 'a 1 'b)))
  (test-equal '((1 . b)) (sorted-alist (hashmap-set (hashmap int-comparator) 1 'a 1 'b)))
  (test-equal '((1 . a)) (sorted-alist (alist->hashmap int-comparator '((1 . a) (1 . b)))))
  (test-equal '((1 . a) (2 . b)) (sorted-alist (alist->hashmap! (hashmap int-comparator 1 'a) '((1 . z) (2 . b)))))
  ;; Set operations.
  (let ((a (hashmap int-comparator 1 'a 2 'a))
        (b (hashmap int-comparator 2 'b 3 'b))
        (c (hashmap int-comparator 3 'c 4 'c)))
    (test-equal '((1 . a) (2 . a) (3 . b) (4 . c)) (sorted-alist (hashmap-union a b c)))
    (test-equal '((2 . a)) (sorted-alist (hashmap-intersection a b)))
    (test-equal '((1 . a)) (sorted-alist (hashmap-difference a b)))
    (test-equal '((1 . a) (3 . b)) (sorted-alist (hashmap-xor a b)))
    (test-equal '((1 . a) (2 . a)) (sorted-alist a))
    (test-assert (hashmap=? comparator a (hashmap-copy a)))
    (test-assert (hashmap<? comparator (hashmap int-comparator 1 'a) a))
    (test-assert (hashmap<=? comparator a a))
    (test-assert (hashmap>? comparator a (hashmap int-comparator 1 'a)))
    (test-assert (hashmap>=? comparator a a (hashmap-copy a)))
    (test-assert (not (hashmap=? comparator a b)))
    (test-assert (hashmap-disjoint? a (hashmap int-comparator 7 'x)))
    (test-assert (not (hashmap-disjoint? a b))))
  ;; Search, pop, intern.
  (let-values (((h obj) (hashmap-search hm 9
                                        (lambda (insert ignore) (insert 'n 'inserted))
                                        (lambda (k v update remove) (remove 'removed)))))
    (test-equal 'inserted obj)
    (test-equal 4 (hashmap-size h))
    (test-equal 3 (hashmap-size hm)))
  (let-values (((h obj) (hashmap-search hm 2
                                        (lambda (insert ignore) (ignore 'ignored))
                                        (lambda (k v update remove) (update 2 'new 'updated)))))
    (test-equal 'updated obj)
    (test-equal 'new (hashmap-ref h 2))
    (test-equal 'b (hashmap-ref hm 2)))
  (let-values (((h obj) (hashmap-search hm 7 (lambda (insert ignore) (ignore 'ignored)) (lambda args #f))))
    (test-equal 'ignored obj)
    (test-assert (eq? h hm)))
  (let-values (((h k v) (hashmap-pop hm)))
    (test-equal 2 (hashmap-size h))
    (test-equal 3 (hashmap-size hm))
    (test-equal v (hashmap-ref hm k)))
  (test-equal 'empty (hashmap-pop (hashmap int-comparator) (lambda () 'empty)))
  (test-error (hashmap-pop (hashmap int-comparator)))
  (let-values (((h v) (hashmap-intern hm 5 (lambda () 'five))))
    (test-equal 'five v)
    (test-equal 4 (hashmap-size h))
    (test-equal 3 (hashmap-size hm)))
  (let-values (((h v) (hashmap-intern hm 1 (lambda () 'x))))
    (test-equal 'a v)
    (test-assert (eq? h hm)))
  ;; Queries and folds.
  (test-equal 3 (hashmap-count (lambda (k v) #t) hm))
  (test-equal 2 (hashmap-count (lambda (k v) (odd? k)) hm))
  (test-equal 6 (hashmap-fold (lambda (k v acc) (+ k acc)) 0 hm))
  (test-equal '(1 2 3) (list-sort < (hashmap-map->list (lambda (k v) k) hm)))
  (test-equal '(1 2 3) (list-sort < (hashmap-keys hm)))
  (test-equal '(a b c) (list-sort (lambda (a b) (string<? (symbol->string a) (symbol->string b))) (hashmap-values hm)))
  (call-with-values (lambda () (hashmap-entries hm))
    (lambda (ks vs)
      (test-equal (map (lambda (k) (hashmap-ref hm k)) ks) vs)))
  (test-assert (hashmap-any? (lambda (k v) (= k 2)) hm))
  (test-assert (not (hashmap-any? (lambda (k v) (= k 9)) hm)))
  (test-assert (hashmap-every? (lambda (k v) (< k 9)) hm))
  (test-assert (hashmap-every? (lambda (k v) #f) (hashmap int-comparator)))
  (test-equal '(2 b) (call-with-values (lambda () (hashmap-find (lambda (k v) (= k 2)) hm (lambda () #f))) list))
  (test-equal 'nope (hashmap-find (lambda (k v) #f) hm (lambda () 'nope)))
  (test-equal '((10 . a) (20 . b) (30 . c))
              (sorted-alist (hashmap-map (lambda (k v) (values (* 10 k) v)) int-comparator hm)))
  (let ((seen '()))
    (hashmap-for-each (lambda (k v) (set! seen (cons k seen))) hm)
    (test-equal '(1 2 3) (list-sort < seen)))
  (call-with-values (lambda () (hashmap-partition (lambda (k v) (odd? k)) hm))
    (lambda (yes no)
      (test-equal '((1 . a) (3 . c)) (sorted-alist yes))
      (test-equal '((2 . b)) (sorted-alist no))))
  ;; Many entries.
  (let ((h (hashmap int-comparator)))
    (do ((i 0 (+ i 1))) ((= i 3000)) (hashmap-set! h i (* i i)))
    (test-equal 3000 (hashmap-size h))
    (test-equal 2500 (hashmap-ref h 50))
    (do ((i 0 (+ i 2))) ((>= i 3000)) (hashmap-delete! h i))
    (test-equal 1500 (hashmap-size h))
    (test-assert (not (hashmap-contains? h 100)))
    (test-assert (hashmap-contains? h 101)))
  ;; Keys that are hashmaps, through the registered comparator.
  (let ((outer (hashmap comparator (hashmap int-comparator 1 'a) "one" (hashmap int-comparator 1 'a) "uno")))
    (test-equal 1 (hashmap-size outer))
    (test-equal "one" (hashmap-ref outer (hashmap int-comparator 1 'a))))
  (test-assert (= (default-hash (hashmap int-comparator 1 'a 2 'b))
                  (default-hash (hashmap int-comparator 2 'b 1 'a))))
  ;; Errors.
  (test-error (hashmap int-comparator 1))
  (test-error (hashmap (make-comparator #t (lambda (a b) (equal? a b)) #f #f)))
  (test-error (hashmap-ref hm 9))
  (test-error (hashmap-set 5 1 2))
  (test-error (hashmap-update hm 9 (lambda (x) x)))
  (test-equal 1 (hashmap-ref (hashmap-update/default (hashmap int-comparator) 5 (lambda (x) (+ x 1)) 0) 5)))

(test-group "review fixes"
  (define c (make-comparator exact-integer? = < (lambda (n) (abs n))))
  (define ci (make-comparator string? string-ci=? string-ci<? string-ci-hash))
  (define (sorted hm) (list-sort (lambda (a b) (string<? (car a) (car b))) (hashmap->alist hm)))
  ;; Comparators with no hash function, for the predicates SRFI 125 hashes.
  (let ((st (make-comparator string? string=? #f #f))
        (q (make-comparator #t equal? #f #f))
        (e (make-comparator #t eq? #f #f))
        (v (make-comparator #t eqv? #f #f))
        (sci (make-comparator string? string-ci=? #f #f)))
    (test-equal 2 (hashmap-ref (hashmap st "a" 1 "b" 2) "b"))
    (test-equal 2 (hashmap-ref (hashmap q '(a) 1 '(b) 2) (list 'b)))
    (test-equal 1 (hashmap-ref (hashmap e 'a 1) 'a))
    (test-equal 1 (hashmap-ref (hashmap v 1.5 1) 1.5))
    (test-equal 1 (hashmap-ref (hashmap sci "A" 1) "a"))
    (test-equal 2 (hashmap-size (hashmap-set (hashmap st "a" 1) "b" 2)))
    (test-error (hashmap (make-comparator #t (lambda (a b) (eqv? a b)) #f #f) 1 2))
    (test-assert (integer? (comparator-hash hashmap-comparator (hashmap st "a" 1)))))
  ;; Setting a key replaces the key held, as mapping-set does.
  (test-equal '("a") (hashmap-keys (hashmap-set (hashmap ci "A" 1) "a" 2)))
  (test-equal '("a") (hashmap-keys (hashmap-set! (hashmap ci "A" 1) "a" 2)))
  (test-equal '(("a" . 5)) (hashmap->alist (hashmap-update (hashmap ci "A" 1) "a" (lambda (x) (+ x 4)))))
  (test-equal '(("a" . 1)) (hashmap->alist (hashmap-update/default (hashmap ci "A" 1) "a" (lambda (x) x) 0)))
  (test-equal '("A") (hashmap-keys (hashmap-adjoin (hashmap ci "A" 1) "a" 2)))
  (test-equal '(("a" . 7)) (hashmap->alist (hashmap-replace (hashmap ci "A" 1) "a" 7)))
  ;; partition! splits and leaves the rest in the hashmap.
  (let ((h (hashmap c 1 'a 2 'b 3 'c)))
    (call-with-values (lambda () (hashmap-partition! (lambda (k v) (odd? k)) h))
      (lambda (yes no)
        (test-equal '(1 3) (list-sort < (hashmap-keys yes)))
        (test-equal '(2) (hashmap-keys no)))))
  ;; Hashing hashmaps that contain themselves ends.
  (let* ((cc (make-comparator vector? eq? #f
                              (lambda (v)
                                (if (vector-ref v 0)
                                    (comparator-hash hashmap-comparator (vector-ref v 0))
                                    0))))
         (v (vector #f))
         (m (hashmap cc v 'x)))
    (vector-set! v 0 m)
    (test-assert (integer? (comparator-hash hashmap-comparator m))))
  ;; Many boxes that each hold the hashmap: hashed in time linear in the boxes.
  (let* ((cc (make-comparator vector? eq? #f
                              (lambda (v)
                                (if (vector-ref v 0)
                                    (comparator-hash hashmap-comparator (vector-ref v 0))
                                    0))))
         (boxes (map (lambda (i) (vector #f)) (iota 60)))
         (m (apply hashmap cc (append-map (lambda (box) (list box 'x)) boxes))))
    (for-each (lambda (box) (vector-set! box 0 m)) boxes)
    (test-assert (integer? (comparator-hash hashmap-comparator m))))
  ;; success of search is given the key the hashmap holds, as mapping-search does.
  (let ((h (hashmap ci "A" 1)))
    (hashmap-search h "a"
                    (lambda (insert ignore) (ignore #f))
                    (lambda (key value update remove)
                      (test-equal "A" key)
                      (remove #f)))
    (hashmap-search! (hashmap-copy h) "a"
                     (lambda (insert ignore) (ignore #f))
                     (lambda (key value update remove)
                       (test-equal "A" key)
                       (remove #f)))
    (call-with-values
        (lambda () (hashmap-search h "a" (lambda (insert ignore) (ignore #f))
                                   (lambda (key value update remove) (update "a2" 5 'ok))))
      (lambda (h2 obj)
        (test-equal '(("a2" . 5)) (hashmap->alist h2))
        (test-equal '(("A" . 1)) (hashmap->alist h)))))
  (test-assert (= (comparator-hash hashmap-comparator (hashmap c 1 'a 2 'b))
                  (comparator-hash hashmap-comparator (hashmap c 2 'b 1 'a)))))

(test-end "srfi-146-hash")
