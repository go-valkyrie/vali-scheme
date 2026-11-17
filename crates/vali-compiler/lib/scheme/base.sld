;;; (scheme base): R7RS's base library, as much of it as Vali has so far.
;;;
;;; The core forms come from (vali core) and the procedures implemented in
;;; Rust from (vali primitive); the derived forms are macros defined here.
;;; Vali also exports syntax-case and its companions from here
;;; (ADR 0007 #10).

(define-library (scheme base)
  ;; The primitives R7RS extends with optional arguments are wrapped below;
  ;; a Rust procedure cannot call back into Scheme to compare.
  (import (vali core)
          (rename (vali primitive)
                  (member %member) (assoc %assoc)
                  (vector->list %vector->list) (vector-fill! %vector-fill!)
                  (string->list %string->list)))
  (export
   ;; Core forms.
   quote quote-syntax if begin lambda let letrec letrec* set! define
   define-syntax let-syntax letrec-syntax syntax-rules syntax-case syntax
   ;; Derived forms, below.
   and or when unless let* cond case do with-syntax quasiquote
   define-record-type let-values let*-values define-values syntax-error
   ;; Auxiliary syntax, bound so that literals match by binding.
   else => _ ... unquote unquote-splicing
   ;; Procedures.
   + * - = < <= > >= quotient remainder modulo abs min max
   number? integer? exact? inexact? zero? positive? negative? odd? even?
   cons car cdr set-car! set-cdr! list length reverse append list-tail
   list-ref memq memv member assq assv assoc null? pair? list?
   caar cadr cdar cddr map for-each list-copy make-list list-set!
   not eq? eqv? equal? boolean? boolean=? procedure? symbol? symbol=? square
   symbol->string string->symbol
   vector make-vector vector-ref vector-set! vector-length vector?
   vector->list list->vector vector-fill!
   vector-map vector-for-each vector-copy vector-copy! vector-append
   vector->string string->vector
   string? string-length string-ref string=? string<? string-append
   substring string->list list->string string-copy string-map string-for-each
   string>? string<=? string>=?
   char? char->integer integer->char char=? char<? char>? char<=? char>=?
   newline write-char write-string
   apply error (rename %raise raise) raise-continuable with-exception-handler
   guard error-object? error-object-message error-object-irritants
   values call-with-values
   ;; The syntax procedures, for transformers.
   syntax->datum datum->syntax identifier? bound-identifier=?
   free-identifier=? generate-temporaries syntax-violation)
  (begin
    (define-syntax and
      (syntax-rules ()
        ((_) #t)
        ((_ test) test)
        ((_ test1 test2 ...) (if test1 (and test2 ...) #f))))

    (define-syntax or
      (syntax-rules ()
        ((_) #f)
        ((_ test) test)
        ((_ test1 test2 ...)
         (let ((x test1))
           (if x x (or test2 ...))))))

    (define-syntax when
      (syntax-rules ()
        ((_ test result1 result2 ...)
         (if test (begin result1 result2 ...)))))

    (define-syntax unless
      (syntax-rules ()
        ((_ test result1 result2 ...)
         (if test (if #f #f) (begin result1 result2 ...)))))

    (define-syntax let*
      (syntax-rules ()
        ((_ () body1 body2 ...)
         (let () body1 body2 ...))
        ((_ ((name1 val1) (name2 val2) ...) body1 body2 ...)
         (let ((name1 val1))
           (let* ((name2 val2) ...) body1 body2 ...)))))

    (define-syntax cond
      (syntax-rules (else =>)
        ((_ (else result1 result2 ...))
         (begin result1 result2 ...))
        ((_ (test => result))
         (let ((temp test))
           (if temp (result temp))))
        ((_ (test => result) clause1 clause2 ...)
         (let ((temp test))
           (if temp
               (result temp)
               (cond clause1 clause2 ...))))
        ((_ (test)) test)
        ((_ (test) clause1 clause2 ...)
         (let ((temp test))
           (if temp temp (cond clause1 clause2 ...))))
        ((_ (test result1 result2 ...))
         (if test (begin result1 result2 ...)))
        ((_ (test result1 result2 ...) clause1 clause2 ...)
         (if test
             (begin result1 result2 ...)
             (cond clause1 clause2 ...)))))

    (define-syntax case
      (syntax-rules (else =>)
        ((_ (key ...) clauses ...)
         (let ((atom-key (key ...)))
           (case atom-key clauses ...)))
        ((_ key (else => result))
         (result key))
        ((_ key (else result1 result2 ...))
         (begin result1 result2 ...))
        ((_ key ((atoms ...) => result))
         (if (memv key '(atoms ...))
             (result key)))
        ((_ key ((atoms ...) => result) clause clauses ...)
         (if (memv key '(atoms ...))
             (result key)
             (case key clause clauses ...)))
        ((_ key ((atoms ...) result1 result2 ...))
         (if (memv key '(atoms ...))
             (begin result1 result2 ...)))
        ((_ key ((atoms ...) result1 result2 ...) clause clauses ...)
         (if (memv key '(atoms ...))
             (begin result1 result2 ...)
             (case key clause clauses ...)))))

    (define-syntax do
      (syntax-rules ()
        ((_ ((var init step ...) ...) (test expr ...) command ...)
         (let loop ((var init) ...)
           (if test
               (begin (if #f #f) expr ...)
               (begin
                 command ...
                 (loop (do "step" var step ...) ...)))))
        ((_ "step" x) x)
        ((_ "step" x y) y)))

    ;;; From R6RS's (rnrs syntax-case): with-syntax binds pattern variables to
    ;;; what each input matches, for a transformer's templates.

    ;; quasiquote (R7RS 4.2.8). qq carries the nesting depth as a list,
    ;; one element per enclosing quasiquote, so only the outermost level's
    ;; unquotes are evaluated.
    (define-syntax quasiquote
      (syntax-rules ()
        ((_ template) (qq template ()))))

    (define-syntax qq
      (syntax-rules (unquote unquote-splicing quasiquote)
        ((_ (unquote x) ()) x)
        ((_ (unquote x) (d . ds)) (list 'unquote (qq x ds)))
        ((_ ((unquote-splicing x) . rest) ())
         (append x (qq rest ())))
        ((_ ((unquote-splicing x) . rest) (d . ds))
         (cons (list 'unquote-splicing (qq x ds)) (qq rest (d . ds))))
        ((_ (quasiquote x) ds) (list 'quasiquote (qq x (#t . ds))))
        ((_ (a . b) ds) (cons (qq a ds) (qq b ds)))
        ((_ #(x ...) ds) (list->vector (qq (x ...) ds)))
        ((_ x ds) 'x)))

    ;; define-record-type (R7RS 5.5). Each field's index is looked up once,
    ;; when the type is defined, so accessors don't search.
    (define-syntax define-record-type
      (syntax-rules ()
        ((_ type (constructor field ...) predicate (fname . accessors) ...)
         (begin
           (define type (%make-record-type 'type '(fname ...)))
           (define constructor
             (let ((indices (%record-constructor-indices type '(field ...))))
               (lambda (field ...) (%make-record type indices field ...))))
           (define (predicate obj) (%record? obj type))
           (record-field type fname . accessors) ...))))

    (define-syntax record-field
      (syntax-rules ()
        ((_ type fname accessor)
         (define accessor
           (let ((i (%record-field-index type 'fname)))
             (lambda (obj) (%record-ref obj type i)))))
        ((_ type fname accessor modifier)
         (begin
           (record-field type fname accessor)
           (define modifier
             (let ((i (%record-field-index type 'fname)))
               (lambda (obj value) (%record-set! obj type i value))))))))

    ;; Exceptions (R7RS 6.11), over the VM's one escape, (%catch thunk
    ;; handler): a raise inside the thunk unwinds to it and calls the
    ;; handler with what was raised. %handlers is the stack of handlers
    ;; raise-continuable calls; each guard pushes one that raises again, so
    ;; it reaches the guard's own %catch.
    (define %handlers '())

    (define (with-exception-handler handler thunk)
      (let ((outer %handlers))
        (set! %handlers (cons handler outer))
        (let ((result
               (%catch thunk
                       (lambda (condition)
                         (set! %handlers outer)
                         (handler condition)
                         (error "an exception handler returned from a non-continuable exception"
                                condition)))))
          (set! %handlers outer)
          result)))

    (define (raise-continuable obj)
      (if (null? %handlers)
          (%raise obj)
          (let ((outer %handlers))
            (set! %handlers (cdr outer))
            (let ((result ((car outer) obj)))
              (set! %handlers outer)
              result))))

    ;; guard's work is a procedure here, since only this library can assign
    ;; %handlers; the macro wraps the body and the clauses in lambdas.
    (define (%guard body clauses)
      (let ((outer %handlers))
        (set! %handlers (cons %raise outer))
        (let ((result
               (%catch body
                       (lambda (condition)
                         (set! %handlers outer)
                         (clauses condition)))))
          (set! %handlers outer)
          result)))

    (define-syntax guard
      (syntax-rules ()
        ((_ (var clause ...) body1 body2 ...)
         (%guard (lambda () (let () body1 body2 ...))
                 (lambda (condition)
                   (let ((var condition))
                     (guard-clauses condition clause ...)))))))

    (define-syntax guard-clauses
      (syntax-rules (else =>)
        ((_ c) (%raise c))
        ((_ c (else e1 e2 ...)) (begin e1 e2 ...))
        ((_ c (test => receiver) clause ...)
         (let ((t test)) (if t (receiver t) (guard-clauses c clause ...))))
        ((_ c (test) clause ...)
         (let ((t test)) (if t t (guard-clauses c clause ...))))
        ((_ c (test e1 e2 ...) clause ...)
         (if test (begin e1 e2 ...) (guard-clauses c clause ...)))))

    ;; Multiple values are an object for now; call-with-values spreads it.
    (define (call-with-values producer consumer)
      (apply consumer (%values->list (producer))))

    ;; let-values, let*-values and define-values (R7RS 4.2.2, 5.3.3), as
    ;; R7RS's own definitions have them.
    (define-syntax let-values
      (syntax-rules ()
        ((_ (binding ...) body0 body1 ...)
         (let-values "bind" (binding ...) () (let () body0 body1 ...)))
        ((_ "bind" () tmps body)
         (let tmps body))
        ((_ "bind" ((b0 e0) binding ...) tmps body)
         (let-values "mktmp" b0 e0 () (binding ...) tmps body))
        ((_ "mktmp" () e0 args bindings tmps body)
         (call-with-values
           (lambda () e0)
           (lambda args (let-values "bind" bindings tmps body))))
        ((_ "mktmp" (a . b) e0 (arg ...) bindings (tmp ...) body)
         (let-values "mktmp" b e0 (arg ... x) bindings (tmp ... (a x)) body))
        ((_ "mktmp" a e0 (arg ...) bindings (tmp ...) body)
         (call-with-values
           (lambda () e0)
           (lambda (arg ... . x)
             (let-values "bind" bindings (tmp ... (a x)) body))))))

    (define-syntax let*-values
      (syntax-rules ()
        ((_ () body0 body1 ...)
         (let () body0 body1 ...))
        ((_ (binding0 binding1 ...) body0 body1 ...)
         (let-values (binding0)
           (let*-values (binding1 ...) body0 body1 ...)))))

    (define-syntax define-values
      (syntax-rules ()
        ((_ () expr)
         (define dummy (call-with-values (lambda () expr) (lambda args #f))))
        ((_ (var) expr)
         (define var expr))
        ((_ (var0 var1 ... varn) expr)
         (begin
           (define var0 (call-with-values (lambda () expr) list))
           (define var1 (let ((v (cadr var0))) (set-cdr! var0 (cddr var0)) v))
           ...
           (define varn (let ((v (cadr var0))) (set! var0 (car var0)) v))))
        ((_ (var0 var1 ... . varn) expr)
         (begin
           (define var0 (call-with-values (lambda () expr) list))
           (define var1 (let ((v (cadr var0))) (set-cdr! var0 (cddr var0)) v))
           ...
           (define varn (let ((v (cdr var0))) (set! var0 (car var0)) v))))
        ((_ var expr)
         (define var (call-with-values (lambda () expr) list)))))

    ;; syntax-error (R7RS 4.3.3): an error when the form is expanded.
    (define-syntax syntax-error
      (lambda (form)
        (syntax-case form ()
          ((_ message arg ...)
           (syntax-violation #f (syntax->datum (syntax message)) form)))))

    ;; Auxiliary syntax (R7RS 4.3.2): keywords that only mean something
    ;; inside other forms. Each is bound, so a macro's literal matches it
    ;; only where it means this binding.
    (define-syntax else (lambda (form) (syntax-violation #f "misplaced auxiliary syntax" form)))
    (define-syntax => (lambda (form) (syntax-violation #f "misplaced auxiliary syntax" form)))
    (define-syntax _ (lambda (form) (syntax-violation #f "misplaced auxiliary syntax" form)))
    (define-syntax ... (lambda (form) (syntax-violation #f "misplaced auxiliary syntax" form)))
    (define-syntax unquote (lambda (form) (syntax-violation #f "misplaced auxiliary syntax" form)))
    (define-syntax unquote-splicing (lambda (form) (syntax-violation #f "misplaced auxiliary syntax" form)))

    ;; Lists (R7RS 6.4).
    (define (caar x) (car (car x)))
    (define (cadr x) (car (cdr x)))
    (define (cdar x) (cdr (car x)))
    (define (cddr x) (cdr (cdr x)))

    ;; The cars and the cdrs of `lists`, or #f once one of them has run out.
    (define (%cars lists)
      (let loop ((lists lists) (cars '()))
        (cond ((null? lists) (reverse cars))
              ((pair? (car lists)) (loop (cdr lists) (cons (car (car lists)) cars)))
              (else #f))))

    (define (%cdrs lists)
      (let loop ((lists lists) (cdrs '()))
        (if (null? lists)
            (reverse cdrs)
            (loop (cdr lists) (cons (cdr (car lists)) cdrs)))))

    ;; map and for-each stop at the shortest list. map builds its result in
    ;; reverse, so a long list never deepens the stack.
    (define (map f list1 . lists)
      (if (null? lists)
          (let loop ((xs list1) (acc '()))
            (if (pair? xs)
                (loop (cdr xs) (cons (f (car xs)) acc))
                (reverse acc)))
          (let loop ((lists (cons list1 lists)) (acc '()))
            (let ((cars (%cars lists)))
              (if cars
                  (loop (%cdrs lists) (cons (apply f cars) acc))
                  (reverse acc))))))

    (define (for-each f list1 . lists)
      (if (null? lists)
          (let loop ((xs list1))
            (when (pair? xs)
              (f (car xs))
              (loop (cdr xs))))
          (let loop ((lists (cons list1 lists)))
            (let ((cars (%cars lists)))
              (when cars
                (apply f cars)
                (loop (%cdrs lists)))))))

    (define (list-copy x)
      (let loop ((x x) (acc '()))
        (if (pair? x)
            (loop (cdr x) (cons (car x) acc))
            (let rebuild ((acc acc) (tail x))
              (if (null? acc)
                  tail
                  (rebuild (cdr acc) (cons (car acc) tail)))))))

    (define (make-list k . fill)
      (let ((fill (if (pair? fill) (car fill) #f)))
        (let loop ((k k) (acc '()))
          (if (<= k 0) acc (loop (- k 1) (cons fill acc))))))

    (define (list-set! list k obj)
      (set-car! (list-tail list k) obj))

    (define (member x list . compare)
      (if (null? compare)
          (%member x list)
          (let ((same? (car compare)))
            (let loop ((l list))
              (cond ((null? l) #f)
                    ((same? x (car l)) l)
                    (else (loop (cdr l))))))))

    (define (assoc x alist . compare)
      (if (null? compare)
          (%assoc x alist)
          (let ((same? (car compare)))
            (let loop ((l alist))
              (cond ((null? l) #f)
                    ((same? x (car (car l))) (car l))
                    (else (loop (cdr l))))))))

    ;; Booleans and numbers.
    (define (boolean=? a b . rest)
      (let loop ((a a) (bs (cons b rest)))
        (cond ((not (boolean? a)) (error "boolean=?: not a boolean" a))
              ((null? bs) #t)
              ((not (boolean? (car bs))) (error "boolean=?: not a boolean" (car bs)))
              ((eq? a (car bs)) (loop (car bs) (cdr bs)))
              (else #f))))

    (define (square x) (* x x))

    ;; Optional start and end indices into an object of length `len`,
    ;; checked, as (start . end).
    (define (%range who range len)
      (let* ((start (if (pair? range) (car range) 0))
             (end (if (and (pair? range) (pair? (cdr range))) (car (cdr range)) len)))
        (if (and (integer? start) (exact? start) (integer? end) (exact? end)
                 (<= 0 start end len))
            (cons start end)
            (error (string-append (symbol->string who) ": bad range") start end))))

    ;; Vectors (R7RS 6.8).
    (define (vector->list v . range)
      (if (null? range)
          (%vector->list v)
          (let* ((r (%range 'vector->list range (vector-length v)))
                 (start (car r)))
            (let loop ((i (cdr r)) (acc '()))
              (if (= i start)
                  acc
                  (loop (- i 1) (cons (vector-ref v (- i 1)) acc)))))))

    (define (vector-fill! v fill . range)
      (if (null? range)
          (%vector-fill! v fill)
          (let* ((r (%range 'vector-fill! range (vector-length v)))
                 (end (cdr r)))
            (let loop ((i (car r)))
              (when (< i end)
                (vector-set! v i fill)
                (loop (+ i 1)))))))

    (define (vector-copy v . range)
      (let* ((r (%range 'vector-copy range (vector-length v)))
             (start (car r))
             (copy (make-vector (- (cdr r) start))))
        (vector-copy! copy 0 v start (cdr r))
        copy))

    ;; Overlapping ranges of one vector are copied in the safe direction.
    (define (vector-copy! to at from . range)
      (let* ((r (%range 'vector-copy! range (vector-length from)))
             (start (car r))
             (end (cdr r)))
        (%range 'vector-copy! (list at (+ at (- end start))) (vector-length to))
        (if (<= at start)
            (let loop ((i start) (j at))
              (when (< i end)
                (vector-set! to j (vector-ref from i))
                (loop (+ i 1) (+ j 1))))
            (let loop ((i end) (j (+ at (- end start))))
              (when (> i start)
                (vector-set! to (- j 1) (vector-ref from (- i 1)))
                (loop (- i 1) (- j 1)))))))

    (define (vector-append . vs)
      (list->vector (apply append (map vector->list vs))))

    (define (vector-map f v . vs)
      (list->vector (apply map f (vector->list v) (map vector->list vs))))

    (define (vector-for-each f v . vs)
      (apply for-each f (vector->list v) (map vector->list vs)))

    (define (vector->string v . range)
      (list->string (apply vector->list v range)))

    (define (string->vector s . range)
      (list->vector (apply string->list s range)))

    ;; Strings (R7RS 6.7); more come with mutable strings.
    (define (string->list s . range)
      (if (null? range)
          (%string->list s)
          (let ((r (%range 'string->list range (string-length s))))
            (%string->list (substring s (car r) (cdr r))))))

    (define (string-copy s . range)
      (let ((r (%range 'string-copy range (string-length s))))
        (substring s (car r) (cdr r))))

    (define (string-map f s . ss)
      (list->string (apply map f (string->list s) (map string->list ss))))

    (define (string-for-each f s . ss)
      (apply for-each f (string->list s) (map string->list ss)))

    ;; Whether `ok?` holds of each neighbouring pair in x and xs.
    (define (%chain ok? x xs)
      (or (null? xs)
          (and (ok? x (car xs)) (%chain ok? (car xs) (cdr xs)))))

    (define (string>? s1 s2 . ss)
      (%chain (lambda (a b) (string<? b a)) s1 (cons s2 ss)))
    (define (string<=? s1 s2 . ss)
      (%chain (lambda (a b) (not (string<? b a))) s1 (cons s2 ss)))
    (define (string>=? s1 s2 . ss)
      (%chain (lambda (a b) (not (string<? a b))) s1 (cons s2 ss)))

    ;; Characters (R7RS 6.6).
    (define (char>? c1 c2 . cs)
      (apply > (char->integer c1) (char->integer c2) (map char->integer cs)))
    (define (char<=? c1 c2 . cs)
      (apply <= (char->integer c1) (char->integer c2) (map char->integer cs)))
    (define (char>=? c1 c2 . cs)
      (apply >= (char->integer c1) (char->integer c2) (map char->integer cs)))

    (define-syntax with-syntax
      (syntax-rules ()
        ((_ () e1 e2 ...)
         (let () e1 e2 ...))
        ((_ ((out in)) e1 e2 ...)
         (syntax-case in () (out (let () e1 e2 ...))))
        ((_ ((out in) ...) e1 e2 ...)
         (syntax-case (list in ...) () ((out ...) (let () e1 e2 ...))))))))
