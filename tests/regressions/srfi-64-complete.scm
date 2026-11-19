;;; requirement: (srfi 64) has groups, runners, skip and expect-fail
;;; found-in: SRFI audit
;;; kind: scheme

;; (srfi 64) once had seven names and a test-end that raised. This tests the
;; whole API by running tests under runners of its own, whose results the
;; outer (default) runner then checks.

(import (scheme base) (srfi 64))

;; Runs thunk under a null runner that logs each test's kind and name, and
;; the groups' events; returns the runner and the log.
(define (record thunk)
  (let ((r (test-runner-null)) (log '()))
    (test-runner-on-test-end! r
      (lambda (r)
        (set! log (cons (cons (test-result-kind r) (test-runner-test-name r))
                        log))))
    (test-runner-on-group-begin! r
      (lambda (r name count) (set! log (cons (list 'begin name count) log))))
    (test-runner-on-group-end! r
      (lambda (r) (set! log (cons (list 'end (test-runner-group-path r)) log))))
    (test-with-runner r (thunk))
    (cons r (reverse log))))

(define (counts r)
  (list (test-runner-pass-count r) (test-runner-fail-count r)
        (test-runner-xpass-count r) (test-runner-xfail-count r)
        (test-runner-skip-count r)))

(define (kinds log)
  (let loop ((l log) (acc '()))
    (cond ((null? l) (reverse acc))
          ((not (string? (cdar l))) (loop (cdr l) acc))
          (else (loop (cdr l) (cons (caar l) acc))))))

(test-begin "srfi-64")

(test-group "kinds and counts"
  (let ((out (record (lambda ()
                       (test-begin "inner")
                       (test-assert "a" #t)
                       (test-assert "b" #f)
                       (test-equal "c" 4 (+ 2 2))
                       (test-equal 4 5)
                       (test-eqv "d" 1.5 1.5)
                       (test-eq "e" 'a 'a)
                       (test-error "f" (car 1))
                       (test-error "g" #t (raise 'x))
                       (test-error "h" (+ 1 2))
                       (test-approximate "i" 1.0 1.05 0.1)
                       (test-approximate "j" 1.0 1.5 0.1)
                       (test-end)))))
    (test-equal '(7 4 0 0 0) (counts (car out)))
    (test-equal '(pass fail pass fail pass pass pass pass fail pass fail)
                (kinds (cdr out)))
    (test-equal '(begin "inner" #f) (cadr out))
    (test-equal '(end ("inner")) (list-ref out (- (length out) 1)))))

(test-group "error types"
  (let ((out (record (lambda ()
                       (test-error "pred" error-object? (error "x"))
                       (test-error "wrong pred" string? (error "x"))
                       (test-error "equal" 'oops (raise 'oops))
                       (test-error "wrong equal" 'oops (raise 'other))
                       (test-error "type only" error-object? (error "x"))))))
    (test-equal '(pass fail pass fail pass) (kinds (cdr out)))))

(test-group "a raise in the expression fails the test"
  (let ((out (record (lambda ()
                       (test-equal 1 (error "boom"))
                       (test-assert (raise 'x))
                       (test-assert #t)))))
    (test-equal '(1 2 0 0 0) (counts (car out)))))

(test-group "groups, names and counts"
  (let ((out (record (lambda ()
                       (test-group "outer"
                         (test-group "inner"
                           (test-assert "t" #t)))))))
    (test-equal '((begin "outer" #f) (begin "inner" #f) (pass . "t")
                  (end ("outer" "inner")) (end ("outer")))
                (cdr out)))
  (let ((bad '()))
    (let ((r (test-runner-null)))
      (test-runner-on-bad-count! r (lambda (r got want)
                                     (set! bad (cons (list got want) bad))))
      (test-runner-on-bad-end-name! r (lambda (r begun ended)
                                        (set! bad (cons (list begun ended) bad))))
      (test-with-runner r
        (test-begin "g" 2)
        (test-assert #t)
        (test-end "g")
        (test-begin "h")
        (test-end "wrong")))
    (test-equal '(("h" "wrong") (1 2)) bad)))

(test-group "cleanup runs however the group ends"
  (let ((cleaned 0))
    (record (lambda ()
              (test-group-with-cleanup "g"
                (test-assert #t)
                (set! cleaned (+ cleaned 1)))))
    (test-equal 1 cleaned)
    (guard (e (#t #f))
      (record (lambda ()
                (test-group-with-cleanup "g"
                  (raise 'escape)
                  (set! cleaned (+ cleaned 10))))))
    (test-equal 11 cleaned)))

(test-group "skip and expect-fail"
  (let ((out (record (lambda ()
                       (test-begin "s")
                       (test-skip "skipped")
                       (test-expect-fail "expected")
                       (test-expect-fail "surprise")
                       (test-assert "skipped" (error "never run"))
                       (test-assert "expected" #f)
                       (test-assert "surprise" #t)
                       (test-assert "ordinary" #t)
                       (test-end)
                       ;; The group's skips end with it.
                       (test-begin "t")
                       (test-assert "skipped" #t)
                       (test-end)))))
    (test-equal '(2 0 1 1 1) (counts (car out)))
    (test-equal '(skip xfail xpass pass pass) (kinds (cdr out)))))

(test-group "matching"
  (let ((out (record (lambda ()
                       (test-begin "m")
                       (test-skip (test-match-nth 2 2))
                       (test-assert "1" #t)
                       (test-assert "2" #t)
                       (test-assert "3" #t)
                       (test-assert "4" #t)
                       (test-end)
                       (test-begin "n")
                       (test-skip (test-match-any "a" "b"))
                       (test-skip (test-match-all (test-match-name "c")
                                                  (lambda (r) #t)))
                       (test-assert "a" #t)
                       (test-assert "b" #t)
                       (test-assert "c" #t)
                       (test-assert "d" #t)
                       (test-end)))))
    (test-equal '(pass skip skip pass skip skip skip pass) (kinds (cdr out)))))

(test-group "test-apply runs only what matches"
  (let ((out (record (lambda ()
                       (test-begin "x")
                       (test-apply "b" (lambda ()
                                         (test-assert "a" #t)
                                         (test-assert "b" #t)))
                       (test-assert "a" #t)
                       (test-end)))))
    (test-equal '(skip pass pass) (kinds (cdr out)))))

(test-group "approximate"
  (let ((out (record (lambda ()
                       (test-approximate 10 10.4 0.5)
                       (test-approximate 10 9.4 0.5)
                       (test-approximate 10 "ten" 0.5)))))
    (test-equal '(pass fail fail) (kinds (cdr out)))))

(test-group "results"
  (let ((seen '()) (r (test-runner-null)))
    (test-runner-on-test-end! r
      (lambda (r)
        (set! seen (cons (list (test-result-kind r)
                               (test-passed? r)
                               (test-result-ref r 'expected-value 'none)
                               (test-result-ref r 'actual-value 'none)
                               (test-result-ref r 'source-form))
                         seen))))
    (test-with-runner r
      (test-equal "n" 3 (+ 1 1))
      (test-assert #t))
    (test-equal '((pass #t #t #t #t) (fail #f 3 2 (+ 1 1))) seen)
    (test-result-set! r 'mine 1)
    (test-equal 1 (test-result-ref r 'mine))
    (test-result-remove r 'mine)
    (test-equal 'gone (test-result-ref r 'mine 'gone))
    (test-result-clear r)
    (test-equal '() (test-result-alist r))))

(test-group "runner API"
  (test-assert (test-runner? (test-runner-null)))
  (test-assert (test-runner? (test-runner-simple)))
  (test-assert (test-runner? (test-runner-create)))
  (let ((r (test-runner-null)))
    (test-runner-aux-value! r 'aux)
    (test-equal 'aux (test-runner-aux-value r))
    (test-with-runner r
      (test-assert (eq? r (test-runner-get)))
      (test-assert #t)
      (test-runner-reset r))
    (test-equal '(0 0 0 0 0) (counts r)))
  (let ((saved (test-runner-factory)) (made 0))
    (test-runner-factory (lambda () (set! made (+ made 1)) (test-runner-null)))
    (test-runner-create)
    (test-runner-factory saved)
    (test-equal 1 made))
  ;; A test-begin with no runner makes one, and test-end forgets it.
  (let ((finals 0) (saved (test-runner-factory)) (after 'unset))
    (test-with-runner #f
      (test-runner-factory
       (lambda ()
         (let ((r (test-runner-null)))
           (test-runner-on-final! r (lambda (r) (set! finals (+ finals 1))))
           r)))
      (test-begin "auto")
      (test-assert #t)
      (test-end)
      (set! after (test-runner-current)))
    (test-runner-factory saved)
    (test-equal #f after)
    (test-equal 1 finals)))

(test-group "test-read-eval-string"
  (test-equal 3 (test-read-eval-string "(+ 1 2)"))
  (test-equal "x" (test-read-eval-string "\"x\"")))

(test-end "srfi-64")
