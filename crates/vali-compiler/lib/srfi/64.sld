;;; (srfi 64): a test API for Scheme (SRFI 64).
;;;
;;; Written for Vali from the SRFI's text. It follows the SRFI's design: a
;;; test runner object, which counts results and calls hooks as tests and
;;; groups begin and end, plus the macros that run one test each.
;;;
;;; Where the SRFI leaves it to the implementation:
;;; - test-runner-current is a procedure: called with no argument it
;;;   returns the current runner (or #f), called with one it sets it.
;;;   test-with-runner and test-apply restore it afterwards.
;;; - Errors are whatever was raised. (test-error type expr) passes if expr
;;;   raises something that type accepts: #t accepts anything, a procedure
;;;   (error-object?, a predicate of your own) accepts what it
;;;   returns true for, and any other value accepts a raised object equal?
;;;   to it, so (test-error 'oops (raise 'oops)) works.
;;; - Source locations are not recorded: source-file and source-line are
;;;   absent from a result.
;;; - test-read-eval-string evaluates in the interaction environment.
;;;
;;; The runner a test gets when nobody made one is the simple runner: it
;;; prints each failure and a summary when the outermost group ends. As
;;; test scripts expect, that default also ends the run with exit code 1
;;; (through (vali exit), which needs no capability; the host sees an exit
;;; error) when a test failed. Install (test-runner-null), or another
;;; runner with test-runner-factory or test-with-runner, for a suite that
;;; should only report.

(define-library (srfi 64)
  (import (scheme base) (scheme write) (scheme read) (scheme eval)
          (scheme repl) (vali exit))
  (export test-begin test-end test-group test-group-with-cleanup
          test-assert test-equal test-eqv test-eq test-approximate
          test-error test-skip test-expect-fail
          test-match-name test-match-nth test-match-any test-match-all
          test-read-eval-string test-apply test-with-runner
          test-runner? test-runner-current test-runner-get
          test-runner-create test-runner-null test-runner-simple
          test-runner-factory test-runner-reset
          test-runner-test-name test-runner-group-path
          test-runner-group-stack
          test-runner-pass-count test-runner-fail-count
          test-runner-xpass-count test-runner-xfail-count
          test-runner-skip-count
          test-runner-aux-value test-runner-aux-value!
          test-runner-on-test-begin test-runner-on-test-begin!
          test-runner-on-test-end test-runner-on-test-end!
          test-runner-on-group-begin test-runner-on-group-begin!
          test-runner-on-group-end test-runner-on-group-end!
          test-runner-on-final test-runner-on-final!
          test-runner-on-bad-count test-runner-on-bad-count!
          test-runner-on-bad-end-name test-runner-on-bad-end-name!
          test-on-test-begin-simple test-on-test-end-simple
          test-on-group-begin-simple test-on-group-end-simple
          test-on-final-simple test-on-bad-count-simple
          test-on-bad-end-name-simple
          test-result-ref test-result-set! test-result-remove
          test-result-clear test-result-alist test-result-kind
          test-passed?)
  (begin
    ;; ---- Runners ----

    ;; skip-list and fail-list hold specifiers (procedures taking a
    ;; runner); run-list is #t, or the specifiers of a test-apply. The
    ;; group stack holds #(name expected-count tests-so-far skip-list
    ;; fail-list), the innermost group first. auto is true for a runner
    ;; test-begin made, which test-end forgets again at the end.
    (define-record-type %runner
      (%make-runner pass fail xpass xfail skip skip-list fail-list run-list
                    groups alist aux auto on-group-begin on-test-begin
                    on-test-end on-group-end on-final on-bad-count
                    on-bad-end-name)
      test-runner?
      (pass %pass %set-pass!) (fail %fail %set-fail!)
      (xpass %xpass %set-xpass!) (xfail %xfail %set-xfail!)
      (skip %skip %set-skip!)
      (skip-list %skip-list %set-skip-list!)
      (fail-list %fail-list %set-fail-list!)
      (run-list %run-list %set-run-list!)
      (groups %groups %set-groups!)
      (alist %alist %set-alist!)
      (aux %aux %set-aux!)
      (auto %auto %set-auto!)
      (on-group-begin test-runner-on-group-begin test-runner-on-group-begin!)
      (on-test-begin test-runner-on-test-begin test-runner-on-test-begin!)
      (on-test-end test-runner-on-test-end test-runner-on-test-end!)
      (on-group-end test-runner-on-group-end test-runner-on-group-end!)
      (on-final test-runner-on-final test-runner-on-final!)
      (on-bad-count test-runner-on-bad-count test-runner-on-bad-count!)
      (on-bad-end-name test-runner-on-bad-end-name
                       test-runner-on-bad-end-name!))

    (define (test-runner-pass-count r) (%pass r))
    (define (test-runner-fail-count r) (%fail r))
    (define (test-runner-xpass-count r) (%xpass r))
    (define (test-runner-xfail-count r) (%xfail r))
    (define (test-runner-skip-count r) (%skip r))
    (define (test-runner-aux-value r) (%aux r))
    (define (test-runner-aux-value! r v) (%set-aux! r v))
    (define (test-runner-group-stack r)
      (map (lambda (g) (vector-ref g 0)) (%groups r)))
    (define (test-runner-group-path r)
      (reverse (test-runner-group-stack r)))

    (define (nothing . args) #f)

    ;; test-runner-create calls the factory; test-runner-current and
    ;; test-runner-factory are get-or-set procedures.
    (define current #f)

    (define (test-runner-current . set)
      (if (pair? set) (set! current (car set)) current))

    (define (test-runner-null)
      (%make-runner 0 0 0 0 0 '() '() #t '() '() #f #f
                    nothing nothing nothing nothing nothing
                    nothing nothing))

    (define (test-runner-simple)
      (let ((r (test-runner-null)))
        (test-runner-on-test-end! r test-on-test-end-simple)
        (test-runner-on-final! r test-on-final-simple)
        (test-runner-on-bad-count! r test-on-bad-count-simple)
        (test-runner-on-bad-end-name! r test-on-bad-end-name-simple)
        r))

    ;; The runner tests get by default: simple, but a suite that failed
    ;; ends the run with exit code 1.
    (define (default-runner)
      (let ((r (test-runner-simple)))
        (test-runner-on-final!
         r
         (lambda (r)
           (test-on-final-simple r)
           (if (> (%fail r) 0) (exit 1))))
        r))

    (define factory default-runner)

    (define (test-runner-factory . set)
      (if (pair? set) (set! factory (car set)) factory))

    (define (test-runner-create) ((test-runner-factory)))

    (define (test-runner-get)
      (or current (error "test-runner-get: no active test runner")))

    ;; The runner a test uses: the current one, made if there is none.
    (define (runner!)
      (or current
          (let ((r (test-runner-create)))
            (set! current r)
            r)))

    (define (test-runner-reset r)
      (%set-pass! r 0) (%set-fail! r 0) (%set-xpass! r 0)
      (%set-xfail! r 0) (%set-skip! r 0)
      (%set-skip-list! r '()) (%set-fail-list! r '())
      (%set-run-list! r #t) (%set-alist! r '()))

    (define (total r)
      (+ (%pass r) (%fail r) (%xpass r) (%xfail r) (%skip r)))

    ;; ---- Results ----

    (define (test-result-alist r) (%alist r))

    (define (test-result-ref r name . default)
      (let ((p (assq name (%alist r))))
        (cond (p (cdr p))
              ((pair? default) (car default))
              (else #f))))

    (define (test-result-set! r name value)
      (let ((p (assq name (%alist r))))
        (if p
            (set-cdr! p value)
            (%set-alist! r (cons (cons name value) (%alist r))))))

    (define (test-result-remove r name)
      (%set-alist! r (let loop ((l (%alist r)))
                       (cond ((null? l) '())
                             ((eq? (caar l) name) (cdr l))
                             (else (cons (car l) (loop (cdr l))))))))

    (define (test-result-clear r) (%set-alist! r '()))

    (define (test-result-kind . r)
      (test-result-ref (if (pair? r) (car r) (test-runner-get)) 'result-kind))

    (define (test-passed? . r)
      (and (memq (apply test-result-kind r) '(pass xpass)) #t))

    (define (test-runner-test-name r) (test-result-ref r 'test-name ""))

    ;; ---- Specifiers ----

    ;; A specifier is a procedure of a runner, or a test name.
    (define (specifier s)
      (cond ((procedure? s) s)
            ((string? s) (test-match-name s))
            (else (error "test-skip: not a specifier:" s))))

    (define (test-match-name name)
      (lambda (r) (equal? name (test-runner-test-name r))))

    ;; Matches the nth test it is asked about, and the count - 1 after it.
    (define (test-match-nth n . count)
      (let ((i 0) (count (if (pair? count) (car count) 1)))
        (lambda (r)
          (set! i (+ i 1))
          (and (>= i n) (< i (+ n count))))))

    (define (test-match-any . specs)
      (let ((specs (map specifier specs)))
        (lambda (r) (any-matches? specs r))))

    (define (test-match-all . specs)
      (let ((specs (map specifier specs)))
        (lambda (r)
          (let loop ((l specs) (all #t))
            (if (null? l) all (loop (cdr l) (and ((car l) r) all)))))))

    ;; Asks every specifier, so a counting one sees every test.
    (define (any-matches? specs r)
      (let loop ((l specs) (any #f))
        (if (null? l) any (loop (cdr l) (or ((car l) r) any)))))

    (define (test-skip spec)
      (let ((r (runner!)))
        (%set-skip-list! r (cons (specifier spec) (%skip-list r)))))

    (define (test-expect-fail spec)
      (let ((r (runner!)))
        (%set-fail-list! r (cons (specifier spec) (%fail-list r)))))

    ;; ---- Groups ----

    (define (test-begin suite-name . count)
      (let ((r current))
        (unless r
          (set! r (test-runner-create))
          (%set-auto! r #t)
          (set! current r))
        (%set-groups! r (cons (vector suite-name
                                      (and (pair? count) (car count))
                                      (total r)
                                      (%skip-list r) (%fail-list r))
                              (%groups r)))
        (test-result-clear r)
        ((test-runner-on-group-begin r) r suite-name
         (and (pair? count) (car count)))))

    (define (test-end . name)
      (let* ((r (test-runner-get)) (groups (%groups r)))
        (when (null? groups)
          (error "test-end: no test-begin to end"))
        (let* ((g (car groups)) (begun (vector-ref g 0)))
          (when (and (pair? name) (not (equal? (car name) begun)))
            ((test-runner-on-bad-end-name r) r begun (car name)))
          (let ((expected (vector-ref g 1))
                (actual (- (total r) (vector-ref g 2))))
            (when (and expected (not (= expected actual)))
              ((test-runner-on-bad-count r) r actual expected)))
          ((test-runner-on-group-end r) r)
          (%set-skip-list! r (vector-ref g 3))
          (%set-fail-list! r (vector-ref g 4))
          (%set-groups! r (cdr groups))
          (when (null? (cdr groups))
            (let ((auto (%auto r)))
              (when auto (set! current #f))
              ((test-runner-on-final r) r))))))

    (define-syntax test-group
      (syntax-rules ()
        ((_ name form ...)
         (let ((n name))
           (dynamic-wind
            (lambda () (test-begin n))
            (lambda () form ...)
            (lambda () (test-end n)))))))

    ;; The last form is the cleanup, which runs however the group ends.
    (define-syntax test-group-with-cleanup
      (syntax-rules ()
        ((_ name form ... cleanup)
         (test-group name
           (dynamic-wind
            (lambda () #f)
            (lambda () #f form ...)
            (lambda () cleanup))))))

    (define (call-with-runner runner thunk)
      (let ((saved current))
        (dynamic-wind
         (lambda () (set! current runner))
         thunk
         (lambda () (set! current saved)))))

    (define-syntax test-with-runner
      (syntax-rules ()
        ((_ runner form ...)
         (call-with-runner runner (lambda () #f form ...)))))

    ;; (test-apply [runner] specifier ... thunk): runs thunk, in which
    ;; only the tests some specifier matches run; the others are skipped.
    (define (test-apply first . rest)
      (let* ((given (test-runner? first))
             (r (if given first (runner!)))
             (args (if given rest (cons first rest)))
             (thunk (let last ((l args))
                      (if (null? (cdr l)) (car l) (last (cdr l)))))
             (specs (let init ((l args))
                      (if (null? (cdr l))
                          '()
                          (cons (specifier (car l)) (init (cdr l))))))
             (saved-run (%run-list r))
             (saved-current current))
        (dynamic-wind
         (lambda ()
           (set! current r)
           (%set-run-list! r (if (null? specs) #t specs)))
         thunk
         (lambda ()
           (%set-run-list! r saved-run)
           (set! current saved-current)))))

    ;; ---- Running one test ----

    ;; Runs one test: body takes the runner, records what it saw with
    ;; test-result-set!, and returns whether the test passed. Nothing
    ;; in body runs for a skipped test.
    (define (run-test name form body)
      (let ((r (runner!)))
        (test-result-clear r)
        (test-result-set! r 'test-name (if (string? name) name ""))
        (test-result-set! r 'source-form form)
        (let ((run (%run-list r)))
          (cond ((or (any-matches? (%skip-list r) r)
                     (and (pair? run) (not (any-matches? run r))))
                 (test-result-set! r 'result-kind 'skip))
                ((any-matches? (%fail-list r) r)
                 (test-result-set! r 'result-kind 'xfail))))
        ((test-runner-on-test-begin r) r)
        (let ((kind (test-result-ref r 'result-kind)))
          (if (eq? kind 'skip)
              (%set-skip! r (+ (%skip r) 1))
              (let ((passed (guard (e (#t (test-result-set! r 'actual-error e)
                                          #f))
                              (body r))))
                (cond ((and passed (eq? kind 'xfail))
                       (test-result-set! r 'result-kind 'xpass)
                       (%set-xpass! r (+ (%xpass r) 1)))
                      (passed
                       (test-result-set! r 'result-kind 'pass)
                       (%set-pass! r (+ (%pass r) 1)))
                      ((eq? kind 'xfail)
                       (%set-xfail! r (+ (%xfail r) 1)))
                      (else
                       (test-result-set! r 'result-kind 'fail)
                       (%set-fail! r (+ (%fail r) 1)))))))
        ((test-runner-on-test-end r) r)))

    ;; A test of expected against actual, said to pass by same?.
    (define (compare-body expected actual same?)
      (lambda (r)
        (let ((e (expected)))
          (test-result-set! r 'expected-value e)
          (let ((a (actual)))
            (test-result-set! r 'actual-value a)
            (same? e a)))))

    (define-syntax test-assert
      (syntax-rules ()
        ((_ name expr)
         (run-test name 'expr
                   (compare-body (lambda () #t) (lambda () expr)
                                 (lambda (e a) (if a #t #f)))))
        ((_ expr) (test-assert #f expr))))

    (define-syntax define-comparison
      (syntax-rules ()
        ((_ test same?)
         (define-syntax test
           (syntax-rules ()
             ((_ name expected expr)
              (run-test name 'expr
                        (compare-body (lambda () expected) (lambda () expr)
                                      same?)))
             ((_ expected expr) (test #f expected expr)))))))

    (define-comparison test-equal equal?)
    (define-comparison test-eqv eqv?)
    (define-comparison test-eq eq?)

    (define-syntax test-approximate
      (syntax-rules ()
        ((_ name expected expr error)
         (run-test name 'expr
                   (compare-body (lambda () expected) (lambda () expr)
                                 (let ((tolerance error))
                                   (lambda (e a)
                                     (and (>= a (- e tolerance))
                                          (<= a (+ e tolerance))))))))
        ((_ expected expr error) (test-approximate #f expected expr error))))

    (define (error-accepted? type e)
      (cond ((eq? type #t) #t)
            ((procedure? type) (if (type e) #t #f))
            (else (equal? type e))))

    (define (error-body type thunk)
      (lambda (r)
        (test-result-set! r 'expected-error type)
        (guard (e ((error-accepted? type e)
                   (test-result-set! r 'actual-error e)
                   #t)
                  (#t (test-result-set! r 'actual-error e)
                      #f))
          (test-result-set! r 'actual-value (thunk))
          #f)))

    ;; (test-error [name] [type] expr). With two operands, a string is the
    ;; name.
    (define (error-test-two a form thunk)
      (if (string? a)
          (run-test a form (error-body #t thunk))
          (run-test #f form (error-body a thunk))))

    (define-syntax test-error
      (syntax-rules ()
        ((_ name type expr)
         (run-test name 'expr (error-body type (lambda () expr))))
        ((_ a expr) (error-test-two a 'expr (lambda () expr)))
        ((_ expr) (run-test #f 'expr (error-body #t (lambda () expr))))))

    (define (test-read-eval-string string)
      (let* ((port (open-input-string string))
             (form (read port)))
        (if (eof-object? (peek-char port))
            (eval form (interaction-environment))
            (error "test-read-eval-string: more than one datum:" string))))

    ;; ---- The simple runner ----

    (define (show-error e)
      (if (error-object? e)
          (begin (display (error-object-message e))
                 (for-each (lambda (i) (display " ") (write i))
                           (error-object-irritants e)))
          (write e)))

    (define (test-on-test-begin-simple r) #f)
    (define (test-on-group-begin-simple r name count) #f)
    (define (test-on-group-end-simple r) #f)

    ;; Prints a failure: what it was, and what it expected and got.
    (define (test-on-test-end-simple r)
      (let ((kind (test-result-ref r 'result-kind)))
        (when (memq kind '(fail xpass))
          (display (if (eq? kind 'fail) "FAIL" "XPASS"))
          (display ": ")
          (for-each (lambda (g) (display g) (display "/"))
                    (test-runner-group-path r))
          (let ((name (test-runner-test-name r)))
            (unless (equal? name "") (display name) (display " ")))
          (write (test-result-ref r 'source-form))
          (newline)
          (let ((e (assq 'expected-value (test-result-alist r)))
                (ee (assq 'expected-error (test-result-alist r)))
                (a (assq 'actual-value (test-result-alist r)))
                (ae (assq 'actual-error (test-result-alist r))))
            (when (and e (not ae))
              (display "  expected: ") (write (cdr e)) (newline))
            (when (and ee (not (eq? (cdr ee) #t)))
              (display "  expected error: ") (write (cdr ee)) (newline))
            (when ae
              (display "  raised: ") (show-error (cdr ae)) (newline))
            (when (and a (not ae))
              (display "  actual: ") (write (cdr a)) (newline))))))

    (define (test-on-final-simple r)
      (define (line label n)
        (unless (= n 0)
          (display label) (display n) (newline)))
      (display "# of expected passes      ") (display (%pass r)) (newline)
      (line "# of expected failures    " (%xfail r))
      (line "# of unexpected passes    " (%xpass r))
      (line "# of unexpected failures  " (%fail r))
      (line "# of skipped tests        " (%skip r)))

    (define (test-on-bad-count-simple r actual expected)
      (display "*** Total number of tests was ") (display actual)
      (display " but should be ") (display expected) (display ". ***")
      (newline))

    (define (test-on-bad-end-name-simple r begun ended)
      (display "*** test-end ") (write ended)
      (display " does not match test-begin ") (write begun) (newline))))
