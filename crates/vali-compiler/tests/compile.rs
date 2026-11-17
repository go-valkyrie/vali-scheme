//! Compiling Scheme and running it on the tier-0 VM.

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Source, compile};
use vali_runtime::{
    CountingHeap, Cx, DeferredHeadered, DeferredSideCounts, Error, HeapConfig, NoCollection,
    PreciseHeadered, PreciseSideCounts, Strategy,
};

/// Only the standard libraries.
struct NoLibraries;

impl Loader for NoLibraries {
    fn load(
        &self,
        _name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok(None)
    }
}

/// Compiles and runs `source` under strategy `S`, returning its value as
/// `write` prints it. A collection runs before it is printed, so whatever
/// only the result's handle keeps, cycles included, must survive it.
fn run_under<S: Strategy>(source: &str) -> Result<String, Error> {
    let program = vali_compiler::compile_program(source, &Identity::new("test"), &NoLibraries)
        .unwrap_or_else(|e| panic!("{source}\n{e}"));
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    cx.install_primitives()?;
    let id = vali_compiler::load(&mut cx, program)?;
    let result = cx.run(id)?;
    cx.collect_garbage();
    cx.write_string(&result)
}

/// Compiles and runs `source` under every strategy, which must agree: a
/// strategy never changes what a program means (ADR 0003 #3).
fn run(source: &str) -> Result<String, Error> {
    let result = run_under::<NoCollection>(source);
    let describe = |r: &Result<String, Error>| match r {
        Ok(s) => s.clone(),
        Err(e) => format!("error: {e}"),
    };
    for (name, other) in [
        ("deferred", run_under::<DeferredHeadered>(source)),
        ("deferred-side", run_under::<DeferredSideCounts>(source)),
        ("precise", run_under::<PreciseHeadered>(source)),
        ("precise-side", run_under::<PreciseSideCounts>(source)),
    ] {
        assert_eq!(
            describe(&other),
            describe(&result),
            "{source}\nunder {name}"
        );
    }
    result
}

fn check(cases: &[(&str, &str)]) {
    for (source, expected) in cases {
        match run(source) {
            Ok(result) => assert_eq!(&result, expected, "{source}"),
            Err(e) => panic!("{source}\n{e}"),
        }
    }
}

#[test]
fn constants_and_arithmetic() {
    check(&[
        ("42", "42"),
        ("(+ 1 2)", "3"),
        ("(+)", "0"),
        ("(* 2 3 4)", "24"),
        ("(- 10 1 2)", "7"),
        ("(- 5)", "-5"),
        ("(+ 1152921504606846975 1)", "1152921504606846976"),
        (
            "123456789012345678901234567890",
            "123456789012345678901234567890",
        ),
        ("(* 1.5 2)", "3.0"),
        (
            "(list (quotient 17 -5) (remainder 17 -5) (modulo 17 -5))",
            "(-3 2 -3)",
        ),
        // The one fixnum quotient that is not a fixnum.
        ("(quotient -1152921504606846976 -1)", "1152921504606846976"),
        ("'(a b . c)", "(a b . c)"),
        ("\"str\"", "\"str\""),
        ("#(1 \"two\" #\\3)", "#(1 \"two\" #\\3)"),
        ("#u8(1 2)", "#u8(1 2)"),
        ("(< 1 2)", "#t"),
        ("(>= 1 2)", "#f"),
        ("(car (cdr '(1 2 3)))", "2"),
        ("(vector-ref (vector 1 2 3) 1)", "2"),
        ("(let ((x (quote a))) (vector x 1 x (+ 1 1)))", "#(a 1 a 2)"),
        (
            "(let ((v (vector 1))) (set! v (vector v v)) v)",
            "#(#(1) #(1))",
        ),
        ("(vector 1 2 3 4 5 6 7 8 9)", "#(1 2 3 4 5 6 7 8 9)"),
    ]);
}

#[test]
fn definitions_and_recursion() {
    check(&[
        (
            "(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))) (fib 20)",
            "6765",
        ),
        (
            "(define (sum-to n) (if (= n 0) 0 (+ n (sum-to (- n 1))))) (sum-to 100000)",
            "5000050000",
        ),
        ("(define x 10) (set! x (+ x 1)) x", "11"),
        ("(define (f . args) args) (f 1 2 3)", "(1 2 3)"),
        ("((lambda (a . rest) rest) 1 2 3)", "(2 3)"),
    ]);
}

#[test]
fn closures_and_boxes() {
    check(&[
        (
            "(define (make-adder n) (lambda (x) (+ x n))) ((make-adder 10) 5)",
            "15",
        ),
        (
            "(define (make-counter)
               (let ((n 0)) (lambda () (set! n (+ n 1)) n)))
             (define c (make-counter))
             (c) (c) (c)",
            "3",
        ),
        (
            // An assigned variable captured by two closures: they share it.
            "(define (pair-of-closures)
               (let ((n 0))
                 (cons (lambda () (set! n (+ n 1))) (lambda () n))))
             (define p (pair-of-closures))
             ((car p)) ((car p))
             ((cdr p))",
            "2",
        ),
        // A local assigned but not captured stays in its slot.
        ("(let ((x 1)) (set! x (+ x 41)) x)", "42"),
    ]);
}

#[test]
fn loops() {
    check(&[
        (
            "(let loop ((i 0) (acc 0)) (if (= i 1000000) acc (loop (+ i 1) (+ acc i))))",
            "499999500000",
        ),
        // Entered from a position that is not a tail call.
        (
            "(+ 1 (let loop ((i 0)) (if (< i 10) (loop (+ i 1)) i)))",
            "11",
        ),
        // Closures over the loop's variable see each iteration's value.
        (
            "(let loop ((i 0) (fs '()))
               (if (= i 3)
                   (list ((car fs)) ((car (cdr fs))))
                   (loop (+ i 1) (cons (lambda () i) fs))))",
            "(2 1)",
        ),
        // Loops inside loops.
        (
            "(let outer ((i 0) (total 0))
               (if (= i 10)
                   total
                   (outer (+ i 1)
                          (let inner ((j 0) (t total))
                            (if (= j i) t (inner (+ j 1) (+ t 1)))))))",
            "45",
        ),
    ]);
}

#[test]
fn a_loop_needs_no_closure() {
    // The named let compiles into the body: one function, no closure.
    let module = compile("(let loop ((i 0)) (if (< i 10) (loop (+ i 1)) i))", "test").unwrap();
    assert_eq!(module.functions.len(), 1);
}

#[test]
fn mutually_recursive_definitions() {
    check(&[
        (
            "(define (parity n)
               (define (even? n) (if (= n 0) #t (odd? (- n 1))))
               (define (odd? n) (if (= n 0) #f (even? (- n 1))))
               (even? n))
             (list (parity 100) (parity 7))",
            "(#t #f)",
        ),
        (
            // A local recursive procedure that is not a loop: its calls to
            // itself are not tail calls, so it is a closure that refers to
            // itself.
            "(define (count-down n)
               (define (down k) (if (= k 0) '() (cons k (down (- k 1)))))
               (down n))
             (count-down 3)",
            "(3 2 1)",
        ),
    ]);
}

#[test]
fn derived_forms() {
    check(&[
        ("(and)", "#t"),
        ("(and 1 2 3)", "3"),
        ("(and 1 #f 3)", "#f"),
        ("(or)", "#f"),
        ("(or #f 2 3)", "2"),
        (
            "(cond (#f 1) ((+ 1 1) => (lambda (x) (* x 10))) (else 3))",
            "20",
        ),
        ("(cond ((assq 'b '((a 1) (b 2))) => cdr) (else 'no))", "(2)"),
        ("(cond (#f 1) (else 2 3))", "3"),
        ("(cond ((memq 'x '(a x b))))", "(x b)"),
        ("(when (< 1 2) 'yes)", "yes"),
        ("(unless (< 1 2) 'yes)", "#<unspecified>"),
        ("(let* ((a 1) (b (+ a 1))) (list a b))", "(1 2)"),
        (
            "(letrec ((f (lambda (n) (if (= n 0) 1 (* n (f (- n 1))))))) (f 10))",
            "3628800",
        ),
        ("(if (not (null? '())) 'a 'b)", "b"),
    ]);
}

#[test]
fn core_forms_can_be_shadowed() {
    check(&[
        // A local binding shadows a core form's keyword.
        ("(let ((if list)) (if 1 2 3))", "(1 2 3)"),
        ("((lambda (quote) (quote 5)) -)", "-5"),
        // So does a top-level definition, for the rest of the program.
        ("(define (begin . xs) xs) (begin 1 2)", "(1 2)"),
        // A body's definitions shadow the parameters around them.
        ("((lambda (x) (define x 2) x) 1)", "2"),
        // Definitions spliced from a begin are still definitions.
        (
            "(define (f) (begin (define a 1) (define b 2)) (+ a b)) (f)",
            "3",
        ),
        ("(begin (define x 1) (define y 2)) (+ x y)", "3"),
        // A top-level redefinition assigns the same variable.
        ("(define x 1) (define (get) x) (define x 2) (get)", "2"),
    ]);
}

#[test]
fn redefining_a_standard_name_stops_inlining_it() {
    check(&[
        ("(define (car x) 'mine) (car '(1 2))", "mine"),
        ("(define (+ a b) (* a b)) (+ 3 4)", "12"),
    ]);
}

#[test]
fn errors() {
    let compile_error = |source: &str| compile(source, "test").unwrap_err().message;
    assert!(compile_error("(if)").starts_with("if:"));
    assert!(compile_error("(lambda)").starts_with("lambda:"));
    assert!(compile_error("(let ((x)) x)").starts_with("let:"));
    assert!(compile_error("(+ 1 (define x 2))").contains("define is only allowed"));
    assert!(compile_error("()").contains("not an expression"));
    assert!(compile_error("(1 . 2)").contains("dotted list"));
    assert!(compile_error("(set! nowhere 1)").contains("nowhere is not defined"));
    assert!(compile_error("((lambda () 1 (define x 2) x))").contains("must come before"));
    assert!(compile_error("((lambda (x x) x) 1 2)").contains("x is bound twice"));
    assert!(compile_error("(lambda (x) (define y 1) (define y 2) y)").contains("y is bound twice"));
    assert!(compile_error("(+ if 1)").contains("syntactic keyword"));

    let Err(Error::Scheme(e)) = run("(undefined-procedure 1)") else {
        panic!("expected an unbound variable");
    };
    assert_eq!(e.message, "unbound variable: undefined-procedure");
    let Err(Error::Scheme(e)) = run("(define (f x) (car x)) (f 5)") else {
        panic!("expected a type error");
    };
    assert_eq!(e.message, "car: expected a pair, got 5");
    assert_eq!(e.backtrace.first().map(String::as_str), Some("f"));
}

#[test]
fn fib_compiles_to_tight_bytecode() {
    let module = compile(
        "(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))",
        "test",
    )
    .unwrap();
    let listing = vali_bytecode::disassemble(&module.functions[1]);
    // The comparison and both subtractions are single instructions, each
    // call's result stays in the slot where the call put it, and the sum
    // reuses the first one's slot.
    assert_eq!(
        listing,
        "\
fib: arity 1, 11 slots, 0 free
  0000  if-lt-imm r1, 2, +1  ; -> 0003
  0002  return r1
  0003  get-variable r5, v0
  0004  sub-imm r6, r1, 1
  0005  call r2, 1
  0006  get-variable r9, v0
  0007  sub-imm r10, r1, 2
  0008  call r6, 1
  0009  add r5, r5, r9
  0010  return r5
"
    );
}

#[test]
fn more_derived_forms() {
    check(&[
        (
            "(case (* 2 3) ((2 3 5 7) 'prime) ((1 4 6 8 9) 'composite))",
            "composite",
        ),
        (
            "(case (car '(c d)) ((a e i o u) 'vowel) ((w y) 'semivowel) (else => (lambda (x) x)))",
            "c",
        ),
        ("(case 5 ((5) => (lambda (x) (* x x))) (else 0))", "25"),
        (
            "(do ((vec (make-vector 5)) (i 0 (+ i 1))) ((= i 5) vec) (vector-set! vec i i))",
            "#(0 1 2 3 4)",
        ),
        (
            "(let ((x '(1 3 5 7 9))) (do ((x x (cdr x)) (sum 0 (+ sum (car x)))) ((null? x) sum)))",
            "25",
        ),
    ]);
}

#[test]
fn macros_are_hygienic() {
    check(&[
        // The temporary that `or` introduces doesn't capture the user's x.
        ("(let ((x 1)) (or #f x))", "1"),
        // What a macro introduces means what it meant where the macro was
        // defined, whatever the use binds: here, the core `if`.
        ("(let ((if list)) (when #t 5))", "5"),
        // R7RS 4.3.2's example: the user's `t` and the macro's are distinct.
        (
            "(letrec-syntax
               ((my-or (syntax-rules ()
                         ((_) #f)
                         ((_ e) e)
                         ((_ e r ...) (let ((t e)) (if t t (my-or r ...)))))))
               (let ((x #f) (y 7) (temp 8) (let odd?) (if even?))
                 (my-or x (let temp) (if y) y)))",
            "7",
        ),
        // A literal matches only an identifier bound the same way: the
        // user's local `else` is a variable, not cond's keyword.
        ("(let ((else #f)) (cond (else 1)))", "#<unspecified>"),
        // But a macro's own `else` is still cond's.
        (
            "(define-syntax choose (syntax-rules () ((_ c a b) (cond (c a) (else b)))))
             (let ((else #f)) (choose #f 1 2))",
            "2",
        ),
    ]);
}

#[test]
fn macros_can_define_names_their_uses_supply() {
    check(&[
        (
            "(define-syntax def-one (syntax-rules () ((_ name) (define name 1))))
             (def-one x)
             x",
            "1",
        ),
        (
            "(define-syntax def-one (syntax-rules () ((_ name) (define name 1))))
             (define (f) (def-one y) (+ y 1))
             (f)",
            "2",
        ),
        // A definition from a macro in a body is seen by the rest of it.
        (
            "(define-syntax def-pair
               (syntax-rules () ((_ a b) (begin (define a 1) (define b 2)))))
             (define (f) (def-pair p q) (list p q))
             (f)",
            "(1 2)",
        ),
    ]);
    // A name the macro made up stays the macro's.
    let Err(Error::Scheme(e)) = run(
        "(define-syntax def-hidden (syntax-rules () ((_) (define hidden 1))))
         (def-hidden)
         hidden",
    ) else {
        panic!("expected hidden to be unbound");
    };
    assert_eq!(e.message, "unbound variable: hidden");
}

#[test]
fn patterns_and_templates() {
    check(&[
        // A variable with less depth than its ellipses is repeated.
        (
            "(define-syntax tag (syntax-rules () ((_ t x ...) '((t x) ...))))
             (tag k 1 2)",
            "((k 1) (k 2))",
        ),
        // Two ellipses in a row flatten two levels.
        (
            "(define-syntax flat (syntax-rules () ((_ (a ...) ...) '(a ... ...))))
             (flat (1 2) () (3))",
            "(1 2 3)",
        ),
        // Nested ellipses, each consuming its own level.
        (
            "(define-syntax swap (syntax-rules () ((_ (a b ...) ...) '((b ... a) ...))))
             (swap (1 2 3) (4) (5 6))",
            "((2 3 1) (4) (6 5))",
        ),
        // Patterns after the ellipsis, and a dotted tail.
        (
            "(define-syntax ends (syntax-rules () ((_ a ... z) '(z a ...))))
             (ends 1 2 3)",
            "(3 1 2)",
        ),
        (
            "(define-syntax rest (syntax-rules () ((_ a . r) 'r)))
             (list (rest 1 2 3) (rest 1))",
            "((2 3) ())",
        ),
        // Vectors.
        (
            "(define-syntax v (syntax-rules () ((_ #(a b ...)) '#(b ... a))))
             (v #(1 2 3))",
            "#(2 3 1)",
        ),
        // Datums in patterns match equal datums.
        (
            "(define-syntax lit (syntax-rules () ((_ 1) 'one) ((_ \"s\") 'string) ((_ x) 'other)))
             (list (lit 1) (lit \"s\") (lit 2))",
            "(one string other)",
        ),
        // A custom ellipsis, and the escape for a literal one.
        (
            "(define-syntax ell (syntax-rules dots () ((_ x dots) '(x dots ...))))
             (ell 1 2)",
            "(1 2 ...)",
        ),
        (
            "(define-syntax esc (syntax-rules () ((_ x) '(x (... ...)))))
             (esc 1)",
            "(1 ...)",
        ),
        // let-syntax.
        (
            "(let-syntax ((double (syntax-rules () ((_ x) (* x 2))))) (double 21))",
            "42",
        ),
        // A macro that expands into a lambda names it after its definition.
        (
            "(define-syntax fn (syntax-rules () ((_ args body) (lambda args body))))
             (define add (fn (a b) (+ a b)))
             (add 1 2)",
            "3",
        ),
    ]);
}

#[test]
fn macro_errors() {
    let compile_error = |source: &str| compile(source, "test").unwrap_err().message;
    assert!(
        compile_error("(define-syntax m (syntax-rules () ((_ x) x))) (m)")
            .contains("m: no pattern matches")
    );
    assert!(compile_error("(define-syntax m (syntax-rules () ((_) (m)))) (m)").contains("forever"));
    assert!(
        compile_error("(define-syntax m (syntax-rules () ((_) (m)))) (+ 1 (m))")
            .contains("forever")
    );
    assert!(
        compile_error("(define-syntax m (syntax-rules () ((_ x ...) x))) (m 1)")
            .contains("needs 1 ellipses")
    );
    assert!(
        compile_error("(define-syntax m (syntax-rules () ((_ ... x) x)))")
            .contains("must follow a pattern")
    );
    assert!(compile_error("(define-syntax m 5)").contains("m: the transformer is not a procedure"));
    assert!(compile_error("(+ 1 when)").contains("syntactic keyword"));
}

#[test]
fn deeply_nested_code_compiles() {
    // Each clause is a cond use inside the previous one's expansion, far
    // deeper than a test thread's stack holds without growing it.
    let clauses: String = (0..1000).map(|i| format!("((= x {i}) {i}) ")).collect();
    let source = format!("(define x 999) (cond {clauses})");
    check(&[(&source, "999")]);
}

#[test]
fn transformers_written_in_scheme() {
    check(&[
        // The simplest: a procedure from syntax to syntax.
        (
            "(define-syntax five (lambda (stx) (quote-syntax 5))) (five)",
            "5",
        ),
        // What quote-syntax quotes means what it meant where it was
        // written, whatever the use binds.
        (
            "(define-syntax ten (lambda (stx) (list (quote-syntax +) 4 6)))
             (let ((+ -)) (ten))",
            "10",
        ),
        // syntax->datum sees the use.
        (
            "(define-syntax show (lambda (stx) (list (quote-syntax quote) (syntax->datum stx))))
             (show 1 (2))",
            "(show 1 (2))",
        ),
        // datum->syntax with the use as context captures on purpose.
        (
            "(define-syntax def-it (lambda (stx) (datum->syntax stx '(define it 42))))
             (def-it)
             it",
            "42",
        ),
        // Transformers can use macros, helpers and closures at phase 1.
        (
            "(define-syntax count-args
               (lambda (stx)
                 (let loop ((d (cdr (syntax->datum stx))) (n 0))
                   (cond ((null? d) (datum->syntax stx n))
                         (else (loop (cdr d) (+ n 1)))))))
             (count-args a b c)",
            "3",
        ),
        // Identifier predicates.
        (
            "(define-syntax ids
               (lambda (stx)
                 (if (and (identifier? (quote-syntax x))
                          (not (identifier? (quote-syntax (x))))
                          (bound-identifier=? (quote-syntax x) (quote-syntax x))
                          (free-identifier=? (quote-syntax car) (quote-syntax car))
                          (not (free-identifier=? (quote-syntax car) (quote-syntax cdr))))
                     (quote-syntax 'ok)
                     (quote-syntax 'bad))))
             (ids)",
            "ok",
        ),
        // A macro defined inside a body.
        (
            "(define (f) (define-syntax two (lambda (stx) (quote-syntax 2))) (* 3 (two))) (f)",
            "6",
        ),
    ]);
}

#[test]
fn transformer_errors() {
    let compile_error = |source: &str| compile(source, "test").unwrap_err().message;
    assert!(
        compile_error("(let ((x 1)) (define-syntax m (lambda (stx) x)) (m))")
            .contains("used out of context")
    );
    assert!(
        compile_error("(define y 1) (define-syntax m (lambda (stx) y)) (m)")
            .contains("does not exist yet")
    );
    assert!(
        compile_error("(define-syntax bad (lambda (stx) (car 5))) (bad)")
            .contains("bad: car: expected a pair, got 5")
    );
    assert!(compile_error("(define-syntax bad (lambda (stx) car)) (bad)").contains("not syntax"));
    assert!(compile_error("(define-syntax bad 5) (bad)").contains("bad:"));
}

#[test]
fn syntax_case() {
    check(&[
        // The regression: a template's tmp doesn't capture the user's.
        (
            "(define-syntax swap!
               (lambda (stx)
                 (syntax-case stx ()
                   ((_ a b) #'(let ((tmp a)) (set! a b) (set! b tmp))))))
             (define tmp 1)
             (define other 2)
             (swap! tmp other)
             (list tmp other)",
            "(2 1)",
        ),
        // The regression: _ in a syntax-case inside a helper.
        (
            "(define-syntax second-of
               (lambda (stx)
                 (define (second form)
                   (syntax-case form ()
                     ((_ b . _) #'b)))
                 (syntax-case stx ()
                   ((_ elements) (second #'elements)))))
             (list (second-of (1 2 3)) (second-of (a 'b)))",
            "(2 b)",
        ),
        // The regression: symbols from syntax->datum are eq?.
        (
            "(define-syntax one-of-x-y?
               (lambda (stx)
                 (syntax-case stx ()
                   ((_ id) (if (memq (syntax->datum #'id) '(x y)) #'#t #'#f)))))
             (list (one-of-x-y? x) (one-of-x-y? y) (one-of-x-y? z))",
            "(#t #t #f)",
        ),
        // Ellipses, and datum->syntax for a computed result.
        (
            "(define-syntax count-args
               (lambda (stx)
                 (syntax-case stx ()
                   ((_ arg ...) (datum->syntax stx (length (syntax->datum #'(arg ...))))))))
             (list (count-args a b c) (count-args))",
            "(3 0)",
        ),
        // Fenders, falling through to the next clause.
        (
            "(define-syntax kind
               (lambda (stx)
                 (syntax-case stx ()
                   ((_ x) (identifier? #'x) #''identifier)
                   ((_ x) #''other))))
             (list (kind a) (kind 1))",
            "(identifier other)",
        ),
        // Literals.
        (
            "(define-syntax arrow?
               (lambda (stx)
                 (syntax-case stx (=>)
                   ((_ =>) #'#t)
                   ((_ x) #'#f))))
             (list (arrow? =>) (arrow? x))",
            "(#t #f)",
        ),
        // with-syntax and generate-temporaries.
        (
            "(define-syntax let-list
               (lambda (stx)
                 (syntax-case stx ()
                   ((_ e ...)
                    (with-syntax (((t ...) (generate-temporaries #'(e ...))))
                      #'(let ((t e) ...) (list t ...)))))))
             (let-list 1 (+ 1 1) 3)",
            "(1 2 3)",
        ),
        // A template in a lambda inside the clause still sees its variables.
        (
            "(define-syntax later
               (lambda (stx)
                 (syntax-case stx ()
                   ((_ a) (let ((f (lambda () #'a))) (f))))))
             (later 5)",
            "5",
        ),
        // A recursive macro, as R7RS defines or.
        (
            "(define-syntax my-or
               (lambda (stx)
                 (syntax-case stx ()
                   ((_) #'#f)
                   ((_ e) #'e)
                   ((_ e r ...) #'(let ((t e)) (if t t (my-or r ...)))))))
             (let ((t 5)) (my-or #f t))",
            "5",
        ),
        // Vectors and nested ellipses in patterns and templates.
        (
            "(define-syntax table
               (lambda (stx)
                 (syntax-case stx ()
                   ((_ #((k v ...) ...)) #''((v ... k) ...)))))
             (table #((a 1 2) (b 3)))",
            "((1 2 a) (3 b))",
        ),
    ]);
}

#[test]
fn syntax_case_errors() {
    let compile_error = |source: &str| compile(source, "test").unwrap_err().message;
    assert!(
        compile_error("(define-syntax m (lambda (stx) (syntax-case stx () ((_ x) #'x)))) (m)")
            .contains("invalid syntax: (m)")
    );
    assert!(
        compile_error("(define-syntax m (lambda (stx) (syntax-case stx () ((_ x) x)))) (m 1)")
            .contains("x: a pattern variable can only be used in a template")
    );
    assert!(
        compile_error(
            "(define-syntax m
               (lambda (stx) (syntax-case stx () ((_ x) (syntax-violation 'm \"bad thing\" stx #'x)))))
             (m 1)"
        )
        .contains("m: bad thing: (m 1), in 1")
    );
    assert!(
        compile_error(
            "(define-syntax m (lambda (stx) (syntax-case stx () ((_ x ...) #'x)))) (m 1)"
        )
        .contains("needs 1 ellipses")
    );
}

#[test]
fn a_program_definition_does_not_change_what_macros_mean() {
    // case's memv is the standard one, whatever the program defines.
    check(&[(
        "(define (memv . args) #f)
         (case 2 ((1 2) 'yes) (else 'no))",
        "yes",
    )]);
}

#[test]
fn quasiquote() {
    check(&[
        ("`(1 ,(+ 1 1) ,@(list 3 4) 5)", "(1 2 3 4 5)"),
        ("(let ((x 'a)) `(x ,x . ,x))", "(x a . a)"),
        ("`#(1 ,(* 2 1) ,@(list 3))", "#(1 2 3)"),
        // R7RS 4.2.8: the inner unquote, at the outermost level, is
        // evaluated; the one around it stays.
        (
            "`(1 `(2 ,(3 ,(+ 1 3))))",
            "(1 (quasiquote (2 (unquote (3 4)))))",
        ),
        ("`,(+ 2 3)", "5"),
        ("`()", "()"),
        (
            "(let ((name 'a)) `(list ,name ',name))",
            "(list a (quote a))",
        ),
    ]);
}

#[test]
fn records() {
    let point = "(define-record-type point (make-point x y) point?
                   (x point-x set-point-x!)
                   (y point-y))";
    check(&[
        (
            &format!("{point} (let ((p (make-point 1 2))) (set-point-x! p 10) (list (point-x p) (point-y p)))"),
            "(10 2)",
        ),
        (
            &format!("{point} (list (point? (make-point 1 2)) (point? 5) (point? (vector 1 2)))"),
            "(#t #f #f)",
        ),
        (&format!("{point} (make-point 1 2)"), "#<point>"),
        (&format!("{point} point"), "#<record-type point>"),
        // A constructor taking some fields, in another order.
        (
            "(define-record-type node (make-node value) node? (value node-value) (next node-next set-node-next!))
             (let ((n (make-node 5))) (set-node-next! n 'end) (list (node-value n) (node-next n)))",
            "(5 end)",
        ),
        // Records of different types are told apart.
        (
            &format!("{point} (define-record-type other (make-other x) other? (x other-x)) (point? (make-other 1))"),
            "#f",
        ),
    ]);
    let Err(Error::Scheme(e)) = run(&format!("{point} (point-x 5)")) else {
        panic!("expected a type error");
    };
    assert!(
        e.message.contains("expected a point record, got 5"),
        "{}",
        e.message
    );
}

#[test]
fn a_dotted_template_with_a_list_tail_is_a_proper_list() {
    // (f . args) with args matching a list builds a call, not a dotted
    // list: the tail continues the list.
    check(&[(
        "(define-syntax call (syntax-rules () ((_ f . args) (f . args))))
         (call + 1 2 3)",
        "6",
    )]);
}

#[test]
fn exceptions() {
    check(&[
        // guard catches what raise raises, and errors from primitives.
        (
            "(guard (e (#t (list 'caught e))) (raise 'oops))",
            "(caught oops)",
        ),
        (
            "(guard (e ((symbol? e) 'symbol) ((string? e) 'string)) (raise \"x\"))",
            "string",
        ),
        (
            "(guard (e ((error-object? e) (error-object-message e))) (car 5))",
            "\"car: expected a pair, got 5\"",
        ),
        (
            "(guard (e (#t (list (error-object-message e) (error-object-irritants e))))
               (error \"bad thing\" 1 'two))",
            "(\"bad thing\" (1 two))",
        ),
        // => and else clauses.
        (
            "(guard (e ((assq 'a e) => cdr) (else 'other)) (raise '((a . 42))))",
            "42",
        ),
        ("(guard (e (#f 'no) (else 'yes)) (raise 1))", "yes"),
        // A clause that doesn't match re-raises, to the next guard out.
        (
            "(guard (e (#t (list 'outer e))) (guard (e ((string? e) 'inner)) (raise 'x)))",
            "(outer x)",
        ),
        // Nothing raised: the body's value, and the stack is as it was.
        ("(guard (e (#t 'no)) (+ 1 2))", "3"),
        (
            "(define (deep n) (if (= n 0) (raise 'bottom) (+ 1 (deep (- n 1)))))
             (guard (e (#t e)) (deep 100000))",
            "bottom",
        ),
        // Guards keep working after a raise and inside loops.
        (
            "(let loop ((i 0) (caught 0))
               (if (= i 100)
                   caught
                   (loop (+ i 1) (+ caught (guard (e (#t 1)) (if (odd? i) (raise i) 0))))))",
            "50",
        ),
        // raise-continuable returns the handler's value.
        (
            "(with-exception-handler
               (lambda (c) (* c 10))
               (lambda () (+ 1 (raise-continuable 4))))",
            "41",
        ),
        // A handler that returns from raise is an error, which a guard
        // around it catches.
        (
            "(guard (e ((error-object? e) 'secondary))
               (with-exception-handler (lambda (c) 'ignored) (lambda () (raise 'x))))",
            "secondary",
        ),
        // Multiple values.
        (
            "(call-with-values (lambda () (values 1 2 3)) list)",
            "(1 2 3)",
        ),
        (
            "(call-with-values (lambda () 5) (lambda (x) (* x 2)))",
            "10",
        ),
        ("(call-with-values (lambda () (values)) list)", "()"),
    ]);
    // An uncaught raise reaches the host with what was raised.
    let Err(Error::Scheme(e)) = run("(raise 'oops)") else {
        panic!("expected an error");
    };
    assert_eq!(e.message, "uncaught exception: oops");
    assert!(e.raised.is_some());
    let Err(Error::Scheme(e)) = run("(error \"bad thing:\" 'x 42)") else {
        panic!("expected an error");
    };
    assert_eq!(e.message, "bad thing: x 42");
}

#[test]
fn syntax_objects_at_run_time() {
    check(&[
        ("(syntax->datum #'(a (b) . c))", "(a (b) . c)"),
        // The regression: symbols from syntax->datum are eq? to literals.
        ("(eq? (syntax->datum #'x) 'x)", "#t"),
        (
            "(list (identifier? #'x) (identifier? #'(x)) (identifier? 5))",
            "(#t #f #f)",
        ),
        ("#'(a b)", "#<syntax (a b)>"),
        // syntax-case at run time.
        ("(syntax-case #'(1 2) () ((a b) (syntax->datum #'b)))", "2"),
        (
            "(syntax->datum (syntax-case #'(f x y) () ((_ arg ...) #'(arg ... done))))",
            "(x y done)",
        ),
        // Bindings resolve as where the syntax was written.
        (
            "(list (free-identifier=? #'car #'car) (free-identifier=? #'car #'cdr))",
            "(#t #f)",
        ),
        (
            "(syntax->datum (datum->syntax #'here '(made up)))",
            "(made up)",
        ),
    ]);
}

#[test]
fn the_numeric_tower() {
    check(&[
        // Literals are constants of every kind.
        (
            "(list 1/2 -3/4 1+2i 1.5-0.5i +i 6/4)",
            "(1/2 -3/4 1+2i 1.5-0.5i 0+1i 3/2)",
        ),
        ("(list (/ 1 3) (/ 6 3) (/ 1 2.0) (/ 2))", "(1/3 2 0.5 1/2)"),
        ("(+ 1/2 1/3 1/6)", "1"),
        ("(* 2/3 3/2)", "1"),
        ("(- 1/2 0.5)", "0.0"),
        ("(* +i +i)", "-1"),
        (
            "(list (+ 1+2i 1-2i) (* 1+2i 3-i) (/ 5+5i 3-i))",
            "(2 5+5i 1+2i)",
        ),
        (
            "(list (< 1/3 0.5 2/3) (= 1/2 0.5) (= 1+2i 1.0+2.0i) (> 9007199254740993 9007199254740992.0))",
            "(#t #t #t #t)",
        ),
        (
            "(list (eqv? 1/2 1/2) (eqv? 1/2 0.5) (eqv? 1+2i 1+2i) (equal? '(1/2) (list (/ 1 2))))",
            "(#t #f #t #t)",
        ),
        ("(memv 1/2 (list 1 (/ 2 4) 3))", "(1/2 3)"),
        (
            "(list (exact 0.5) (exact 2.0) (inexact 1/4) (inexact 1+2i) (exact 0.5+1.5i))",
            "(1/2 2 0.25 1.0+2.0i 1/2+3/2i)",
        ),
        (
            "(list (numerator 6/4) (denominator 6/4) (denominator 0.5) (numerator 5))",
            "(3 2 2.0 5)",
        ),
        (
            "(list (floor -7/2) (ceiling -7/2) (truncate -7/2) (round 7/2) (round 5/2) (round 2.5) (floor 2.5))",
            "(-4 -3 -3 4 2 2.0 2.0)",
        ),
        (
            "(list (quotient -7 2) (remainder -7 2) (modulo -7 2) (modulo 7 -2) (modulo -7.0 2))",
            "(-3 -1 1 -1 1.0)",
        ),
        (
            "(list (floor-quotient -7 2) (floor-remainder -7 2) (truncate-quotient -7 2) (truncate-remainder -7 2))",
            "(-4 1 -3 -1)",
        ),
        (
            "(call-with-values (lambda () (floor/ -7 2)) list)",
            "(-4 1)",
        ),
        (
            "(let-values (((q r) (truncate/ -7 2))) (list q r))",
            "(-3 -1)",
        ),
        (
            "(let-values (((s r) (exact-integer-sqrt 17))) (list s r))",
            "(4 1)",
        ),
        (
            "(list (gcd 32 -36) (gcd) (lcm 32 -36) (lcm) (gcd 4.0 6))",
            "(4 0 288 1 2.0)",
        ),
        (
            "(list (expt 2 100) (expt 2/3 3) (expt 2 -2) (expt 2.0 3) (expt 0 0) (expt +i 2))",
            "(1267650600228229401496703205376 8/27 1/4 8.0 1 -1)",
        ),
        (
            "(list (abs -1/2) (abs -5) (min 1/2 1/3) (max 1 2.0) (min 1 2))",
            "(1/2 5 1/3 2.0 1)",
        ),
        (
            "(list (number? 1/2) (complex? 1+i) (real? 1+i) (real? 1/2) (rational? 1/2) (rational? 1.5) (rational? +inf.0))",
            "(#t #t #f #t #t #t #f)",
        ),
        (
            "(list (integer? 2.0) (integer? 1/2) (exact-integer? 2) (exact-integer? 2.0) (exact? 1/2) (inexact? 1.0+i))",
            "(#t #f #t #f #t #t)",
        ),
        (
            "(list (zero? 0/5) (positive? 1/2) (negative? -1/2) (odd? 7) (even? 1267650600228229401496703205376) (even? 4.0))",
            "(#t #t #t #t #t #t)",
        ),
        (
            "(list (number->string 1/3) (number->string 255 16) (number->string 1+2i) (string->number \"1/3\") (string->number \"#xff\") (string->number \"2+3i\") (string->number \"x\"))",
            "(\"1/3\" \"ff\" \"1+2i\" 1/3 255 2+3i #f)",
        ),
        (
            "(list (rationalize 1/3 1/100) (rationalize 3/10 1/10) (rationalize -3/10 1/10) (rationalize 0.3 1/10))",
            "(1/3 1/3 -1/3 0.3333333333333333)",
        ),
        // Large values stay exact.
        ("(* 1/3 (expt 10 30))", "1000000000000000000000000000000/3"),
    ]);
    let error = |source: &str| run(source).unwrap_err().to_string();
    assert!(error("(/ 1 0)").contains("/: division by zero"));
    assert!(error("(< 1 +i)").contains("<: expected a real number"));
    assert!(error("(floor/ 1/2 1)").contains("floor/: expected an integer, got 1/2"));
    assert!(error("(exact +inf.0)").contains("exact: no exact value"));
    assert!(error("(exact-integer-sqrt -1)").contains("expected an exact non-negative integer"));
    assert!(error("(+ 1 'a)").contains("+: expected a number, got a"));
}

#[test]
fn inexact_and_complex_libraries() {
    check(&[
        (
            "(import (scheme base) (scheme inexact))
             (list (sqrt 16) (sqrt -4) (sqrt 9/4) (sqrt 2.25) (exp 0) (log 1) (sin 0) (cos 0) (atan 0 1))",
            "(4 0+2i 3/2 1.5 1 0 0 1 0)",
        ),
        (
            "(import (scheme base) (scheme inexact))
             (list (log 8 2) (atan 1 0) (nan? +nan.0) (infinite? -inf.0) (finite? 1/2) (finite? +nan.0))",
            "(3.0 1.5707963267948966 #t #t #t #f)",
        ),
        (
            "(import (scheme base) (scheme complex))
             (list (make-rectangular 1 2) (make-rectangular 1.5 0) (make-polar 2 0) (real-part 3-4i)
                   (imag-part 3-4i) (imag-part 2.5) (magnitude 3-4i) (magnitude -5/2) (angle 1) (angle -1))",
            "(1+2i 1.5 2 3 -4 0 5 5/2 0 3.141592653589793)",
        ),
    ]);
}

#[test]
fn mutable_strings() {
    check(&[
        (
            "(list (make-string 3 #\\x) (string #\\a #\\b) (string) (string-length (make-string 2)))",
            "(\"xxx\" \"ab\" \"\" 2)",
        ),
        (
            "(let ((s (make-string 3 #\\a))) (string-set! s 1 #\\b) s)",
            "\"aba\"",
        ),
        // A character that takes more bytes replaces the string's bytes.
        (
            "(let ((s (string-copy \"abc\"))) (string-set! s 1 #\\λ) (list s (string-length s) (string-ref s 2)))",
            "(\"aλc\" 3 #\\c)",
        ),
        (
            "(let ((s (string-copy \"aλc\"))) (string-set! s 1 #\\b) (list s (string->utf8 s)))",
            "(\"abc\" #u8(97 98 99))",
        ),
        (
            "(let ((s (string-copy \"日本語\"))) (string-set! s 2 #\\x) s)",
            "\"日本x\"",
        ),
        (
            "(let ((s (make-string 5 #\\-))) (string-fill! s #\\* 1 3) s)",
            "\"-**--\"",
        ),
        (
            "(let ((s (string-copy \"hello\"))) (string-copy! s 1 \"XYZ\" 1) s)",
            "\"hYZlo\"",
        ),
        (
            "(let ((s (string-copy \"abcde\"))) (string-copy! s 1 s 0 3) s)",
            "\"aabce\"",
        ),
        (
            "(list (string->utf8 \"λx\") (utf8->string #u8(206 187 120)) (string->utf8 \"abc\" 1 2) (utf8->string #u8(97 98 99) 1))",
            "(#u8(206 187 120) \"λx\" #u8(98) \"bc\")",
        ),
    ]);
    let error = |source: &str| run(source).unwrap_err().to_string();
    assert!(
        error("(string-set! \"literal\" 0 #\\x)")
            .contains("string-set!: the string \"literal\" is immutable")
    );
    assert!(error("(string-set! (symbol->string 'abc) 0 #\\x)").contains("is immutable"));
    assert!(
        error("(string-set! (make-string 2) 2 #\\x)")
            .contains("index 2 is out of range for length 2")
    );
    assert!(error("(utf8->string (bytevector 255))").contains("invalid UTF-8"));
    assert!(error("(string-length 'abc)").contains("string-length: expected a string, got abc"));
}

#[test]
fn characters_and_case() {
    check(&[
        (
            "(import (scheme base) (scheme char))
             (list (char-ci=? #\\a #\\A #\\a) (char-ci<? #\\a #\\B) (char-foldcase #\\Σ) (char-upcase #\\ß)
                   (char-upper-case? #\\A) (char-lower-case? #\\A) (digit-value #\\7) (digit-value #\\٣)
                   (digit-value #\\x) (char-numeric? #\\٣) (char-numeric? #\\Ⅻ))",
            "(#t #t #\\σ #\\ß #t #f 7 3 #f #t #f)",
        ),
        (
            "(import (scheme base) (scheme char))
             (list (string-ci=? \"Straße\" \"STRASSE\") (string-ci<? \"abc\" \"ABD\") (string-foldcase \"ΣΑΣ\")
                   (string-upcase \"straße\") (string-downcase \"ΣΑΣ\"))",
            "(#t #t \"σασ\" \"STRASSE\" \"σας\")",
        ),
    ]);
}

#[test]
fn bytevectors() {
    check(&[
        (
            "(list (bytevector 1 2 3) (make-bytevector 2 7) (bytevector-length (make-bytevector 4)) (bytevector? #u8()) (bytevector? \"x\"))",
            "(#u8(1 2 3) #u8(7 7) 4 #t #f)",
        ),
        (
            "(let ((b (bytevector 1 2 3))) (bytevector-u8-set! b 0 255) (list b (bytevector-u8-ref b 0)))",
            "(#u8(255 2 3) 255)",
        ),
        (
            "(list (bytevector-copy #u8(1 2 3 4) 1 3) (bytevector-append #u8(1) #u8() #u8(2 3)))",
            "(#u8(2 3) #u8(1 2 3))",
        ),
        (
            "(let ((b (bytevector 1 2 3 4 5))) (bytevector-copy! b 1 b 0 3) b)",
            "#u8(1 1 2 3 5)",
        ),
        ("(equal? (bytevector 1 2) #u8(1 2))", "#t"),
    ]);
    let error = |source: &str| run(source).unwrap_err().to_string();
    assert!(
        error("(bytevector-u8-set! #u8(1 2) 0 3)").contains("the bytevector #u8(1 2) is immutable")
    );
    assert!(error("(bytevector 256)").contains("bytevector: expected a byte, got 256"));
    assert!(error("(bytevector-u8-ref (bytevector 1) 1)").contains("out of range"));
}

#[test]
fn parameters() {
    check(&[
        (
            "(define p (make-parameter 1)) (list (p) (parameterize ((p 2)) (p)) (p))",
            "(1 2 1)",
        ),
        (
            "(define q (make-parameter 10 (lambda (x) (* x 2))))
             (list (q) (parameterize ((q 3)) (q)) (q))",
            "(20 6 20)",
        ),
        // A raise out of the body restores the old value.
        (
            "(define p (make-parameter 1))
             (list (guard (e (#t (list e (p)))) (parameterize ((p 2)) (raise 'x))) (p))",
            "((x 1) 1)",
        ),
        (
            "(define a (make-parameter 'a)) (define b (make-parameter 'b))
             (parameterize ((a 1) (b 2)) (parameterize ((a 3)) (list (a) (b))))",
            "(3 2)",
        ),
    ]);
}

#[test]
fn string_and_bytevector_ports() {
    check(&[
        (
            "(let ((p (open-input-string \"héllo\\nworld\")))
               (list (read-char p) (peek-char p) (read-line p) (read-string 3 p)
                     (read-line p) (eof-object? (read-char p)) (eof-object? (read-line p))))",
            "(#\\h #\\é \"éllo\" \"wor\" \"ld\" #t #t)",
        ),
        (
            "(let ((p (open-output-string)))
               (write-char #\\a p) (write-string \"xbcd\" p 1) (newline p)
               (equal? (get-output-string p) (string #\\a #\\b #\\c #\\d #\\newline)))",
            "#t",
        ),
        (
            "(let ((p (open-output-string)))
               (parameterize ((current-output-port p)) (write-string \"inside\"))
               (get-output-string p))",
            "\"inside\"",
        ),
        (
            "(let ((p (open-input-bytevector #u8(1 2 3))))
               (list (read-u8 p) (peek-u8 p) (read-bytevector 5 p) (eof-object? (read-u8 p))))",
            "(1 2 #u8(2 3) #t)",
        ),
        (
            "(let ((p (open-output-bytevector)))
               (write-u8 7 p) (write-bytevector #u8(1 2 3) p 1) (get-output-bytevector p))",
            "#u8(7 2 3)",
        ),
        (
            "(let ((bv (make-bytevector 4 0)) (p (open-input-bytevector #u8(9 8))))
               (list (read-bytevector! bv p 1) bv))",
            "(2 #u8(0 9 8 0))",
        ),
        (
            "(let ((p (open-input-string \"\")))
               (list (port? p) (input-port? p) (output-port? p) (textual-port? p) (binary-port? p)
                     (port? 5) (input-port? 5) (input-port-open? p)
                     (begin (close-port p) (input-port-open? p))))",
            "(#t #t #f #t #f #f #f #t #f)",
        ),
        (
            "(let ((p (open-input-string \"abc\"))) (list (call-with-port p read-char) (input-port-open? p)))",
            "(#\\a #f)",
        ),
        (
            "(list (open-input-string \"\") (open-output-bytevector))",
            "(#<textual input port> #<binary output port>)",
        ),
        (
            "(list (eof-object? (eof-object)) (eof-object? 'eof))",
            "(#t #f)",
        ),
    ]);
    let error = |source: &str| run(source).unwrap_err().to_string();
    assert!(
        error("(read-char (open-output-string))")
            .contains("read-char: expected an open textual input port")
    );
    assert!(error("(read-char 5)").contains("read-char: argument 1: expected port, got 5"));
    assert!(
        error("(let ((p (open-output-string))) (close-port p) (write-char #\\a p))")
            .contains("expected an open textual output port")
    );
}

#[test]
fn reading_and_writing() {
    check(&[
        (
            "(import (scheme base) (scheme read))
             (let ((p (open-input-string \"(a . b) 42 \\\"s\\\" #(1)\")))
               (list (read p) (read p) (read p) (read p) (eof-object? (read p))))",
            "((a . b) 42 \"s\" #(1) #t)",
        ),
        (
            "(import (scheme base) (scheme write))
             (let ((p (open-output-string)))
               (write '(a \"b\" #\\c) p) (display '(a \"b\" #\\c) p)
               (equal? (get-output-string p)
                       (string-append \"(a \" (string #\\\" #\\b #\\\") \" #\\\\c)(a b c)\")))",
            "#t",
        ),
        (
            "(import (scheme base) (scheme write))
             (let ((x (list 1 2)) (shared (open-output-string)) (plain (open-output-string)))
               (write-shared (list x x) shared) (write (list x x) plain) (write-simple x plain)
               (list (get-output-string shared) (get-output-string plain)))",
            "(\"(#0=(1 2) #0#)\" \"((1 2) (1 2))(1 2)\")",
        ),
        (
            "(import (scheme base) (scheme write))
             (let ((x (list 1 2)) (p (open-output-string)))
               (set-cdr! (cdr x) x) (write x p) (get-output-string p))",
            "\"#0=(1 2 . #0#)\"",
        ),
    ]);
    let error = |source: &str| run(source).unwrap_err().to_string();
    assert!(
        error("(import (scheme base) (scheme file)) (open-input-file \"x\")")
            .contains("open-input-file: needs the file capability")
    );
    assert!(
        error("(import (scheme base) (scheme process-context)) (command-line)")
            .contains("command-line: needs the process-context capability")
    );
}

#[test]
fn keywords() {
    let prelude = "(import (scheme base) (scheme read) (srfi 88))";
    let cases = [
        ("foo:", "foo:"),
        (
            "(list (keyword? foo:) (keyword? 'foo) (keyword? \"foo:\"))",
            "(#t #f #f)",
        ),
        // Interned, so eq?, and self-evaluating inside quoted data too.
        ("(eq? foo: (string->keyword \"foo\"))", "#t"),
        ("(eq? foo: (car '(foo: bar:)))", "#t"),
        ("(keyword->string bar:)", "\"bar\""),
        ("(read (open-input-string \"baz:\"))", "baz:"),
        ("(eq? baz: (read (open-input-string \"baz:\")))", "#t"),
        ("(equal? '(a: #(b:)) (list a: (vector b:)))", "#t"),
        // A keyword in syntax-rules' output stays a keyword.
        (
            "(define-syntax k (syntax-rules () ((_) 'key:))) (keyword? (k))",
            "#t",
        ),
    ];
    for (source, expected) in cases {
        let source = format!("{prelude}{source}");
        match run(&source) {
            Ok(result) => assert_eq!(result, expected, "{source}"),
            Err(e) => panic!("{source}\n{e}"),
        }
    }
}

/// Checks many expressions after one prelude, compiling once per strategy:
/// each expression becomes a thunk the host calls, so each is checked on
/// its own and every strategy must agree, as `run` does, at a fraction of
/// the compiling.
fn check_each(prelude: &str, cases: &[(&str, &str)]) {
    fn under<S: Strategy>(source: &str, count: usize) -> Vec<Result<String, String>> {
        let program = vali_compiler::compile_program(source, &Identity::new("test"), &NoLibraries)
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let mut cx = Cx::<S>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let id = vali_compiler::load(&mut cx, program).unwrap();
        cx.run(id).unwrap();
        (0..count)
            .map(|i| {
                let thunk = cx.variable(id, &format!("%case{i}")).unwrap();
                let result = cx.call(&thunk, &[]).map_err(|e| e.to_string())?;
                cx.collect_garbage();
                cx.write_string(&result).map_err(|e| e.to_string())
            })
            .collect()
    }
    let mut source = prelude.to_owned();
    for (i, (expr, _)) in cases.iter().enumerate() {
        source.push_str(&format!("\n(define (%case{i}) {expr})"));
    }
    let results = under::<NoCollection>(&source, cases.len());
    for (name, other) in [
        ("deferred", under::<DeferredHeadered>(&source, cases.len())),
        (
            "deferred-side",
            under::<DeferredSideCounts>(&source, cases.len()),
        ),
        ("precise", under::<PreciseHeadered>(&source, cases.len())),
        (
            "precise-side",
            under::<PreciseSideCounts>(&source, cases.len()),
        ),
    ] {
        for ((expr, _), (a, b)) in cases.iter().zip(results.iter().zip(&other)) {
            assert_eq!(b, a, "{expr}\nunder {name}");
        }
    }
    for ((expr, expected), result) in cases.iter().zip(results) {
        match result {
            Ok(result) => assert_eq!(&result, expected, "{expr}"),
            Err(e) => panic!("{expr}\n{e}"),
        }
    }
}

/// SRFI 1, mostly the SRFI's own examples, beside (scheme base).
#[test]
fn list_library() {
    let prelude = "(import (scheme base) (srfi 1))";
    let cases = [
        // Constructors.
        ("(xcons '(b c) 'a)", "(a b c)"),
        ("(list (cons* 1 2 3 4) (cons* 1))", "((1 2 3 . 4) 1)"),
        ("(list-tabulate 4 (lambda (i) (* i i)))", "(0 1 4 9)"),
        (
            "(list (iota 5) (iota 5 0 -1) (iota 3 1/2))",
            "((0 1 2 3 4) (0 -1 -2 -3 -4) (1/2 3/2 5/2))",
        ),
        (
            "(let ((c (circular-list 'z 'q))) (list (take c 5) (circular-list? c) (length+ c)))",
            "((z q z q z) #t #f)",
        ),
        // Predicates.
        (
            "(list (proper-list? '(1 2)) (proper-list? '(1 . 2)) (dotted-list? '(1 . 2))
                   (dotted-list? 3) (circular-list? '(1 2)) (not-pair? 5) (null-list? '()))",
            "(#t #f #t #t #f #t #t)",
        ),
        (
            "(list (list= eq?) (list= eq? '(a)) (list= = '(1 2) '(1 2) '(1 2)) (list= = '(1 2) '(1 3)))",
            "(#t #t #t #f)",
        ),
        // Selectors.
        (
            "(list (first '(a b c)) (third '(a b c)) (tenth (iota 10)))",
            "(a c 9)",
        ),
        (
            "(call-with-values (lambda () (car+cdr '(1 . 2))) list)",
            "(1 2)",
        ),
        (
            "(list (take '(a b c d e) 2) (drop '(a b c d e) 2)
                   (take-right '(a b c d e) 2) (drop-right '(a b c d e) 2))",
            "((a b) (c d e) (d e) (a b c))",
        ),
        (
            "(list (take-right '(1 2 3 . d) 2) (take-right '(1 2 3 . d) 0))",
            "((2 3 . d) d)",
        ),
        (
            "(call-with-values (lambda () (split-at '(a b c d e f g h) 3)) list)",
            "((a b c) (d e f g h))",
        ),
        ("(list (last '(1 2 3)) (last-pair '(a b c)))", "(3 (c))"),
        // Miscellaneous.
        (
            "(list (length+ '(1 2 3)) (concatenate '((1) (2 3) () (4))) (append-reverse '(3 2 1) '(4 5)))",
            "(3 (1 2 3 4) (1 2 3 4 5))",
        ),
        (
            "(zip '(one two three) '(1 2 3) '(odd even odd even))",
            "((one 1 odd) (two 2 even) (three 3 odd))",
        ),
        (
            "(call-with-values (lambda () (unzip2 '((1 one) (2 two) (3 three)))) list)",
            "((1 2 3) (one two three))",
        ),
        (
            "(list (count even? '(3 1 4 1 5 9 2 5 6))
                   (count < '(1 2 4 8) '(2 4 6 8 10 12 14 16))
                   (count < '(3 1 4 1) (circular-list 1 10)))",
            "(3 3 2)",
        ),
        // Folding, unfolding and mapping.
        (
            "(list (fold + 0 '(1 2 3)) (fold cons '() '(a b c)) (fold cons* '() '(a b c) '(1 2 3 4 5)))",
            "(6 (c b a) (c 3 b 2 a 1))",
        ),
        (
            "(list (fold-right cons '() '(a b c)) (fold-right cons* '() '(a b c) '(1 2 3 4 5)))",
            "((a b c) (a 1 b 2 c 3))",
        ),
        (
            "(list (pair-fold cons '() '(a b c)) (pair-fold-right cons '() '(a b c)))",
            "(((c) (b c) (a b c)) ((a b c) (b c) (c)))",
        ),
        (
            "(list (reduce + 0 '(1 2 3)) (reduce max 0 '()) (reduce-right append '() '((1 2) (3) (4 5))))",
            "(6 0 (1 2 3 4 5))",
        ),
        (
            "(list (unfold (lambda (x) (> x 5)) (lambda (x) (* x x)) (lambda (x) (+ x 1)) 1)
                   (unfold null-list? car cdr '(1 2 3) (lambda (x) 'end))
                   (unfold-right zero? (lambda (x) (* x x)) (lambda (x) (- x 1)) 5))",
            "((1 4 9 16 25) (1 2 3 . end) (1 4 9 16 25))",
        ),
        (
            "(append-map (lambda (x) (list x (- x))) '(1 3 8))",
            "(1 -1 3 -3 8 -8)",
        ),
        (
            "(let ((acc '())) (pair-for-each (lambda (p) (set! acc (cons (length p) acc))) '(a b c)) acc)",
            "(1 2 3)",
        ),
        (
            "(filter-map (lambda (x) (and (number? x) (* x x))) '(a 1 b 3 c 7))",
            "(1 9 49)",
        ),
        // Filtering.
        ("(filter even? '(0 7 8 8 43 -4))", "(0 8 8 -4)"),
        (
            "(call-with-values (lambda () (partition symbol? '(one 2 3 four five 6))) list)",
            "((one four five) (2 3 6))",
        ),
        ("(remove even? '(0 7 8 8 43 -4))", "(7 43)"),
        // Searching.
        (
            "(list (find even? '(3 1 4 1 5 9)) (find-tail even? '(3 1 37 -8 -5 0 0)) (find even? '(1 3)))",
            "(4 (-8 -5 0 0) #f)",
        ),
        (
            "(list (any integer? '(a 3 b 2.7)) (any < '(3 1 4 1 5) '(2 7 1 8 2)) (any even? '())
                   (every integer? '()) (every (lambda (x) (and (> x 0) x)) '(1 2 3)))",
            "(#t #t #f #t 3)",
        ),
        (
            "(list (list-index even? '(3 1 4 1 5 9))
                   (list-index < '(3 1 4 1 5 9 2 5 6) '(2 7 1 8 2))
                   (list-index = '(3 1 4 1 5 9 2 5 6) '(2 7 1 8 2)))",
            "(2 1 #f)",
        ),
        (
            "(list (take-while even? '(2 18 3 10 22 9)) (drop-while even? '(2 18 3 10 22 9)))",
            "((2 18) (3 10 22 9))",
        ),
        (
            "(list (call-with-values (lambda () (span even? '(2 18 3 10 22 9))) list)
                   (call-with-values (lambda () (break even? '(3 1 4 1 5 9 2 6))) list))",
            "(((2 18) (3 10 22 9)) ((3 1) (4 1 5 9 2 6)))",
        ),
        // Deleting.
        (
            "(list (delete 3 '(1 3 2 3)) (delete 5 '(0 3 7 8 9) <) (delete-duplicates '(a b a c a b c z)))",
            "((1 2) (0 3) (a b c z))",
        ),
        (
            "(delete-duplicates '((a . 3) (b . 7) (a . 9) (c . 1)) (lambda (x y) (eq? (car x) (car y))))",
            "((a . 3) (b . 7) (c . 1))",
        ),
        // Association lists.
        (
            "(list (alist-cons 'a 1 '((b . 2)))
                   (alist-delete 'a '((a . 1) (b . 2) (a . 3)))
                   (let* ((a (list (cons 1 2))) (b (alist-copy a)))
                     (list (equal? a b) (eq? (car a) (car b)))))",
            "(((a . 1) (b . 2)) ((b . 2)) (#t #f))",
        ),
        // Sets as lists.
        (
            "(list (lset<= eq? '(a) '(a b a) '(a b c c)) (lset= eq? '(b e a) '(a e b) '(e e b a))
                   (lset-adjoin eq? '(a b c d c e) 'a 'e 'i 'o 'u))",
            "(#t #t (u o i a b c d c e))",
        ),
        (
            "(lset-union eq? '(a b c d e) '(a e i o u))",
            "(u o i a b c d e)",
        ),
        (
            "(list (lset-intersection eq? '(a b c d e) '(a e i o u))
                   (lset-difference eq? '(a b c d e) '(a e i o u))
                   (lset= eq? (lset-xor eq? '(a b c d e) '(a e i o u)) '(d c b i o u)))",
            "((a e) (b c d) #t)",
        ),
        (
            "(call-with-values (lambda () (lset-diff+intersection eq? '(a b c d e) '(a e i o u))) list)",
            "((b c d) (a e))",
        ),
        // Beside (scheme base): the same map and member, which stop at the
        // shortest list, a circular one included.
        (
            "(list (map + '(1 2 3) '(10 20)) (member 2.0 '(1 2 3) =) (map + '(3 1 4 1) (circular-list 1 0)))",
            "((11 22) (2 3) (4 1 5 1))",
        ),
        // Long lists deepen no stack.
        (
            "(list (length (fold-right cons '() (iota 100000))) (fold + 0 (iota 100000)))",
            "(100000 4999950000)",
        ),
    ];
    check_each(prelude, &cases);
}

#[test]
fn sorting() {
    let prelude = "(import (scheme base) (srfi 132))";
    let cases = [
        ("(list-sort < '(3 1 2))", "(1 2 3)"),
        ("(list-sort < '())", "()"),
        // Stable: equal keys keep their order.
        (
            "(list-stable-sort (lambda (a b) (< (car a) (car b)))
                               '((2 . a) (1 . b) (2 . c) (1 . d)))",
            "((1 . b) (1 . d) (2 . a) (2 . c))",
        ),
        (
            "(list-sort < (let loop ((i 0) (acc '())) (if (= i 1000) acc (loop (+ i 1) (cons (modulo (* i 7919) 1000) acc)))))",
            "",
        ),
        (
            "(list (list-sorted? < '(1 2 2 3)) (list-sorted? < '(2 1)))",
            "(#t #f)",
        ),
        ("(vector-sort < #(5 3 9 1))", "#(1 3 5 9)"),
        ("(vector-sort < #(5 3 9 1) 1 3)", "#(3 9)"),
        (
            "(let ((v (vector 5 3 9 1 0))) (vector-sort! v < 1 4) v)",
            "#(5 1 3 9 0)",
        ),
        ("(list-merge < '(1 4 6) '(2 3 7))", "(1 2 3 4 6 7)"),
        ("(vector-merge < #(1 4) #(2 3))", "#(1 2 3 4)"),
        (
            "(list-delete-neighbor-dups = '(1 1 2 3 3 3 1))",
            "(1 2 3 1)",
        ),
        (
            "(let ((v (vector 1 1 2 2 3))) (list (vector-delete-neighbor-dups! = v) v))",
            "(3 #(1 2 3 2 3))",
        ),
        ("(vector-find-median < #(5 1 3) 0)", "3"),
        ("(vector-find-median < #(4 1 3 2) 0)", "5/2"),
        ("(vector-select! < (vector 5 1 3 9) 2)", "5"),
    ];
    for (source, expected) in cases {
        let source = format!("{prelude}{source}");
        match run(&source) {
            Ok(result) if expected.is_empty() => {
                let expected = (0..1000)
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                assert_eq!(result, format!("({expected})"), "{source}");
            }
            Ok(result) => assert_eq!(result, expected, "{source}"),
            Err(e) => panic!("{source}\n{e}"),
        }
    }
}

#[test]
fn dynamic_wind_and_call_cc() {
    check(&[
        (
            "(let ((log '()))
               (define (note x) (set! log (cons x log)))
               (dynamic-wind (lambda () (note 'before))
                             (lambda () (note 'during))
                             (lambda () (note 'after)))
               (reverse log))",
            "(before during after)",
        ),
        // Escaping runs the after thunk.
        (
            "(let ((log '()))
               (define (note x) (set! log (cons x log)))
               (call/cc (lambda (k)
                 (dynamic-wind (lambda () (note 'in))
                               (lambda () (k 'escaped))
                               (lambda () (note 'out)))))
               (reverse log))",
            "(in out)",
        ),
        // Re-entering runs the before thunk again (R7RS 6.10).
        (
            "(let ((path '()) (c #f))
               (let ((add (lambda (s) (set! path (cons s path)))))
                 (dynamic-wind (lambda () (add 'connect))
                               (lambda () (add (call/cc (lambda (c0) (set! c c0) 'talk1))))
                               (lambda () (add 'disconnect)))
                 (if (< (length path) 4) (c 'talk2) (reverse path))))",
            "(connect talk1 disconnect connect talk2 disconnect)",
        ),
        // From one extent to another: out of one, into the other.
        (
            "(let ((log '()) (k #f))
               (define (note x) (set! log (cons x log)))
               (dynamic-wind (lambda () (note 'a-in))
                             (lambda () (call/cc (lambda (c) (set! k c))))
                             (lambda () (note 'a-out)))
               (if (< (length log) 4)
                   (dynamic-wind (lambda () (note 'b-in))
                                 (lambda () (k 'again))
                                 (lambda () (note 'b-out)))
                   (reverse log)))",
            "(a-in a-out b-in b-out a-in a-out)",
        ),
        (
            "(call-with-values (lambda () (call/cc (lambda (k) (k 1 2)))) list)",
            "(1 2)",
        ),
        (
            "(+ 1 (call-with-current-continuation (lambda (k) (k 2) 10)))",
            "3",
        ),
        // A continuation restores the parameterization and the handlers.
        (
            "(define p (make-parameter 'outer))
             (define k #f)
             (define n 0)
             (define seen
               (parameterize ((p 'inner))
                 (call/cc (lambda (c) (set! k c) (p)))))
             (set! n (+ n 1))
             (if (= n 1) (k (p)) (list seen (p) n))",
            "(outer outer 2)",
        ),
    ]);
}

#[test]
fn handlers_run_where_the_raise_happens() {
    check(&[
        // A handler sees the raise's parameterization.
        (
            "(let ((p (make-parameter 'outer)))
               (call/cc (lambda (k)
                 (with-exception-handler
                   (lambda (e) (k (list e (p))))
                   (lambda () (parameterize ((p 'inner)) (raise 'oops)))))))",
            "(oops inner)",
        ),
        // So does an error from a Rust procedure.
        (
            "(let ((p (make-parameter 'outer)))
               (call/cc (lambda (k)
                 (with-exception-handler
                   (lambda (e) (k (list (error-object? e) (error-object-message e) (p))))
                   (lambda () (parameterize ((p 'inner)) (+ 1 (car 5))))))))",
            "(#t \"car: expected a pair, got 5\" inner)",
        ),
        // A handler's value is raise-continuable's.
        (
            "(with-exception-handler (lambda (e) 42) (lambda () (+ (raise-continuable 'c) 1)))",
            "43",
        ),
        // A handler runs with the outer handlers installed.
        (
            "(with-exception-handler
               (lambda (e) (list 'outer e))
               (lambda ()
                 (with-exception-handler
                   (lambda (e) (raise-continuable (list 'inner e)))
                   (lambda () (raise-continuable 'x)))))",
            "(outer (inner x))",
        ),
        // Returning from a handler for raise raises again, outside it.
        (
            "(guard (e ((error-object? e) (error-object-message e)))
               (with-exception-handler (lambda (e) 'ignored) (lambda () (raise 'boom))))",
            "\"an exception handler returned from a non-continuable exception\"",
        ),
        // guard unwinds first, running after thunks.
        (
            "(let ((log '()))
               (define (note x) (set! log (cons x log)))
               (guard (e (#t (note (list 'caught e))))
                 (dynamic-wind (lambda () (note 'in))
                               (lambda () (raise 'x))
                               (lambda () (note 'out))))
               (reverse log))",
            "(in out (caught x))",
        ),
        // With no clause for it, guard re-enters the raise and calls
        // raise-continuable there: the outer handler's value comes back.
        (
            "(let ((log '()))
               (define (note x) (set! log (cons x log)))
               (let ((v (with-exception-handler
                          (lambda (e) 10)
                          (lambda ()
                            (guard (e ((string? e) 'no))
                              (dynamic-wind (lambda () (note 'in))
                                            (lambda () (+ 100 (raise-continuable 'sym)))
                                            (lambda () (note 'out))))))))
                 (list v (reverse log))))",
            "(110 (in out in out))",
        ),
        // Nested guards: the outer gets what the inner declines.
        (
            "(guard (e ((symbol? e) (list 'outer e)))
               (guard (e ((string? e) 'inner))
                 (raise 'x)))",
            "(outer x)",
        ),
        (
            "(guard (e ((error-object? e) (error-object-message e))) (vector-ref (vector 1) 3))",
            "\"vector-ref: index 3 is out of range for a vector of length 1\"",
        ),
        (
            "(guard (e ((string? e) 'string) (else (list 'else e))) (raise 5))",
            "(else 5)",
        ),
    ]);
}

#[test]
fn parameters_are_dynamic() {
    check(&[(
        "(define p (make-parameter 1 (lambda (x) (* x 10))))
         (list (p)
               (parameterize ((p 2)) (p))
               (p)
               (call/cc (lambda (k) (parameterize ((p 3)) (k (p)))))
               (p)
               (guard (e (#t (p))) (parameterize ((p 4)) (raise 'x))))",
        "(10 20 10 30 10 10)",
    )]);
}

/// Every host call starts in an empty dynamic environment, however the last
/// one ended.
#[test]
fn host_calls_start_afresh() {
    fn under<S: Strategy>() {
        let source = "
            (define p (make-parameter 'outer))
            (define log '())
            (define (bad) (parameterize ((p 'inner)) (car 5)))
            (define (unwound)
              (dynamic-wind (lambda () #f) (lambda () (car 5))
                            (lambda () (set! log (cons 'after log)))))
            (define (handled)
              (with-exception-handler (lambda (e) 'handled) (lambda () (car 5))))
            (define (state) (list (p) log))";
        let program =
            vali_compiler::compile_program(source, &Identity::new("test"), &NoLibraries).unwrap();
        let mut cx = Cx::<S>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let id = vali_compiler::load(&mut cx, program).unwrap();
        cx.run(id).unwrap();
        let call = |cx: &mut Cx<S>, name: &str| {
            let f = cx.variable(id, name).unwrap();
            cx.call(&f, &[])
        };
        assert!(call(&mut cx, "bad").is_err());
        assert!(call(&mut cx, "unwound").is_err());
        // An error with no handler unwinds straight to the host, running
        // no after thunks, as before.
        let error = call(&mut cx, "handled").unwrap_err().to_string();
        assert!(error.contains("an exception handler returned"), "{error}");
        let state = call(&mut cx, "state").unwrap();
        assert_eq!(cx.write_string(&state).unwrap(), "(outer ())");
    }
    under::<NoCollection>();
    under::<DeferredHeadered>();
    under::<PreciseSideCounts>();
}

#[test]
fn continuations() {
    let prelude = "(import (scheme base) (only (vali primitive) %call/cc %continue %catch))";
    let cases = [
        // Returning normally, through the sealed caller.
        "(+ 1 (%call/cc (lambda (k) 41)))",
        // Escaping.
        "(define (find-first pred xs)
           (%call/cc (lambda (k) (for-each (lambda (x) (if (pred x) (%continue k x))) xs) #f)))
         (find-first even? '(1 3 4 5))",
        // In tail position, the continuation is the caller's.
        "(define (f) (%call/cc (lambda (k) (%continue k 5) 6))) (list (f) (f))",
        // Re-entered, many times.
        "(define saved #f)
         (define n 0)
         (let ((v (%call/cc (lambda (k) (set! saved k) 0))))
           (set! n (+ n 1))
           (if (< v 3) (%continue saved (+ v 1)) (list v n)))",
        // A local assigned after the capture keeps its value when the
        // continuation is re-entered: it is one location.
        "(let ((n 0))
           (let ((k (%call/cc (lambda (k) k))))
             (set! n (+ n 1))
             (if (< n 3) (%continue k k) n)))",
        // Captured deep enough to chain segments, then re-entered.
        "(define r '())
         (define k2 #f)
         (define (deep n) (if (= n 0) (%call/cc (lambda (k) (set! k2 k) 0)) (+ 1 (deep (- n 1)))))
         (let ((x (deep 20000)))
           (set! r (cons x r))
           (if (< (length r) 3) (%continue k2 (length r)) r))",
        // Captured inside a capture's receiver.
        "(define outer #f)
         (define count 0)
         (let ((v (%call/cc (lambda (k1) (+ 10 (%call/cc (lambda (k2) (set! outer k1) (%continue k2 1))))))))
           (set! count (+ count 1))
           (if (= count 1) (%continue outer 100) (list v count)))",
        // A catch whose thunk was re-entered is waiting again.
        "(define k3 #f)
         (define count 0)
         (define result
           (%catch (lambda ()
                     (let ((v (%call/cc (lambda (k) (set! k3 k) 0))))
                       (if (> v 0) (raise v) v)))
                   (lambda (c) (list 'caught c))))
         (set! count (+ count 1))
         (if (= count 1) (%continue k3 7) (list result count))",
        // Raising inside a receiver reaches a catch whose frame is sealed.
        "(list (%catch (lambda () (+ 1 (%call/cc (lambda (k) (raise 'up))))) (lambda (c) (list 'caught c))))",
        // A generator: two loops passing control back and forth.
        "(define (make-gen xs)
           (define return #f)
           (define resume #f)
           (define (start)
             (for-each (lambda (x) (%call/cc (lambda (k) (set! resume k) (%continue return x)))) xs)
             (%continue return 'done))
           (lambda ()
             (%call/cc (lambda (r)
               (set! return r)
               (if resume (%continue resume #f) (start))))))
         (define g (make-gen '(a b c)))
         (let loop ((acc '()))
           (let ((x (g)))
             (if (eq? x 'done) (reverse acc) (loop (cons x acc)))))",
    ];
    let expected = [
        "42",
        "4",
        "(5 5)",
        "(3 4)",
        "3",
        "(20002 20001 20000)",
        "(100 2)",
        "((caught 7) 2)",
        "((caught up))",
        "(a b c)",
    ];
    for (source, expected) in cases.iter().zip(expected) {
        let source = format!("{prelude}{source}");
        match run(&source) {
            Ok(result) => assert_eq!(result, expected, "{source}"),
            Err(e) => panic!("{source}\n{e}"),
        }
    }
    let errors = [
        (
            "(%call/cc car)",
            "%call/cc: expected a Scheme procedure of one argument",
        ),
        (
            "(%call/cc (lambda () 1))",
            "%call/cc: expected a Scheme procedure of one argument",
        ),
        ("(%continue 1 2)", "%continue: expected a continuation"),
    ];
    for (source, message) in errors {
        let source = format!("{prelude}{source}");
        match run(&source) {
            Ok(result) => panic!("{source}: expected an error, got {result}"),
            Err(e) => assert!(e.to_string().contains(message), "{source}: {e}"),
        }
    }
}

/// Serves `(lib a)` and lets everything read the files it holds, except
/// `secret.scm`.
struct Files;

impl Files {
    const FILES: &[(&str, &str)] = &[
        ("defs.scm", "(define included 42)"),
        (
            "nested.scm",
            "(include \"defs.scm\") (define nested (+ included 1))",
        ),
        ("CI.scm", "(DEFINE Shout 'LOUD)"),
        ("a-exports.scm", "(export extra)"),
        (
            "a-body.scm",
            "(define value 1) (define both 2) (define extra 3)",
        ),
        ("secret.scm", "(define secret 'leaked)"),
    ];
}

impl Loader for Files {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok((*name == LibraryName::new(["lib", "a"])).then(|| {
            Source::scheme(
                Identity::new("test:lib/a"),
                "(define-library (lib a)
                   (import (scheme base))
                   (export value both)
                   (cond-expand
                     ((library (lib missing)) (export missing))
                     ((and vali (library (scheme base)))
                      (include-library-declarations \"a-exports.scm\")))
                   (include \"a-body.scm\"))",
            )
        }))
    }

    fn include(&self, path: &str, _from: &Identity) -> Result<Option<String>, LoadError> {
        Ok(Self::FILES
            .iter()
            .find(|(p, _)| *p == path && path != "secret.scm")
            .map(|(_, text)| (*text).to_owned()))
    }
}

fn run_with_files(source: &str) -> Result<String, String> {
    let program = vali_compiler::compile_program(source, &Identity::new("test"), &Files)
        .map_err(|e| e.message)?;
    let mut cx = Cx::<PreciseHeadered>::new(&HeapConfig::default());
    cx.install_primitives().map_err(|e| e.to_string())?;
    let id = vali_compiler::load(&mut cx, program).map_err(|e| e.to_string())?;
    let result = cx.run(id).map_err(|e| e.to_string())?;
    cx.write_string(&result).map_err(|e| e.to_string())
}

#[test]
fn cond_expand_and_features() {
    check(&[
        ("(cond-expand (r7rs 'yes) (else 'no))", "yes"),
        (
            "(cond-expand ((not vali) 1) ((and r7rs (library (scheme base))) 2))",
            "2",
        ),
        (
            "(cond-expand ((library (no such library)) 1) (else 2))",
            "2",
        ),
        (
            "(cond-expand ((or full-unicode no-such-feature) 'or))",
            "or",
        ),
        // In a body, the chosen clause's definitions are the body's.
        ("(let () (cond-expand (vali (define x 5))) (+ x 1))", "6"),
        ("(if (memq 'r7rs (features)) #t #f)", "#t"),
    ]);
}

#[test]
fn including_files() {
    let cases = [
        ("(include \"defs.scm\") included", "42"),
        // A file can include another.
        ("(include \"nested.scm\") nested", "43"),
        ("(include-ci \"CI.scm\") shout", "loud"),
        // In a body, and as an expression.
        ("(let () (include \"defs.scm\") (* included 2))", "84"),
        // Declarations: cond-expand, include-library-declarations, include.
        (
            "(import (scheme base) (lib a)) (list value both extra)",
            "(1 2 3)",
        ),
    ];
    for (source, expected) in cases {
        match run_with_files(source) {
            Ok(result) => assert_eq!(result, expected, "{source}"),
            Err(e) => panic!("{source}\n{e}"),
        }
    }
    // The host decides what may be read; by default, nothing.
    let error = run_with_files("(include \"secret.scm\") secret").unwrap_err();
    assert!(error.contains("test may not read secret.scm"), "{error}");
    let error = vali_compiler::compile_program(
        "(include \"defs.scm\") included",
        &Identity::new("test"),
        &NoLibraries,
    )
    .unwrap_err()
    .message;
    assert!(error.contains("may not read defs.scm"), "{error}");
}

/// Serves its own (scheme time), as a host granting the clock would, and a
/// (scheme base) that must be ignored.
struct Clock;

impl Loader for Clock {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok(if *name == LibraryName::new(["scheme", "time"]) {
            Some(Source::scheme(
                Identity::new("host:time"),
                "(define-library (scheme time)
                   (import (scheme base))
                   (export current-second current-jiffy jiffies-per-second)
                   (begin
                     (define (current-second) 1.5)
                     (define (current-jiffy) 7)
                     (define (jiffies-per-second) 1000)))",
            ))
        } else if *name == LibraryName::new(["scheme", "base"]) {
            Some(Source::scheme(
                Identity::new("host:base"),
                "(define-library (scheme base) (export) (begin))",
            ))
        } else {
            None
        })
    }
}

/// Runs `source` with `loader` under every strategy, which must agree.
fn run_loaded(source: &str, loader: &impl Loader) -> Result<String, Error> {
    fn under<S: Strategy>(source: &str, loader: &impl Loader) -> Result<String, Error> {
        let program = vali_compiler::compile_program(source, &Identity::new("test"), loader)
            .unwrap_or_else(|e| panic!("{source}\n{e}"));
        let mut cx = Cx::<S>::new(&HeapConfig::default());
        cx.install_primitives()?;
        let id = vali_compiler::load(&mut cx, program)?;
        let result = cx.run(id)?;
        cx.write_string(&result)
    }
    let result = under::<NoCollection>(source, loader);
    assert_eq!(
        under::<DeferredHeadered>(source, loader),
        result,
        "{source}"
    );
    assert_eq!(
        under::<PreciseSideCounts>(source, loader),
        result,
        "{source}"
    );
    result
}

#[test]
fn exiting() {
    let prelude = "(import (scheme base) (scheme process-context))";
    let cases = [
        ("(exit)", 0),
        ("(exit #t)", 0),
        ("(exit #f)", 1),
        ("(exit 7)", 7),
        ("(exit 'other)", 1),
        // No handler or guard stops it.
        ("(guard (e (#t 'caught)) (exit 4))", 4),
        (
            "(with-exception-handler (lambda (e) 'handled) (lambda () (exit 5)))",
            5,
        ),
        ("(emergency-exit 6)", 6),
    ];
    for (source, code) in cases {
        let source = format!("{prelude}{source}");
        assert_eq!(
            run_loaded(&source, &NoLibraries),
            Err(Error::Exit(code)),
            "{source}"
        );
    }
    // exit runs the after thunks, innermost first; emergency-exit does not.
    let log = |exit: &str| {
        let source = format!(
            "{prelude}
             (define log '())
             (define (note x) (set! log (cons x log)))
             (define (run)
               (dynamic-wind (lambda () #f)
                             (lambda ()
                               (dynamic-wind (lambda () #f)
                                             (lambda () ({exit} 0))
                                             (lambda () (note 'inner))))
                             (lambda () (note 'outer))))"
        );
        let program =
            vali_compiler::compile_program(&source, &Identity::new("test"), &NoLibraries).unwrap();
        let mut cx = Cx::<PreciseHeadered>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let id = vali_compiler::load(&mut cx, program).unwrap();
        cx.run(id).unwrap();
        let run = cx.variable(id, "run").unwrap();
        assert_eq!(cx.call(&run, &[]).err(), Some(Error::Exit(0)));
        let log = cx.variable(id, "log").unwrap();
        cx.write_string(&log).unwrap()
    };
    assert_eq!(log("exit"), "(outer inner)");
    assert_eq!(log("emergency-exit"), "()");
}

#[test]
fn libraries_that_reach_outside() {
    // Vali's own versions have no capability.
    let error = |source: &str| match run_loaded(source, &NoLibraries) {
        Ok(result) => panic!("{source}: expected an error, got {result}"),
        Err(e) => e.to_string(),
    };
    let message = error("(import (scheme base) (scheme process-context)) (command-line)");
    assert!(
        message.contains("command-line: needs the process-context capability"),
        "{message}"
    );
    let message = error("(import (scheme base) (scheme time)) (current-second)");
    assert!(
        message.contains("current-second: needs the clock capability"),
        "{message}"
    );
    assert_eq!(
        run_loaded(
            "(import (scheme base) (scheme time)) (jiffies-per-second)",
            &NoLibraries
        ),
        Ok("1000000".to_owned())
    );
    // A host serves its own, which wins; it cannot replace (scheme base).
    assert_eq!(
        run_loaded(
            "(import (scheme base) (scheme time)) (list (current-second) (current-jiffy) (+ 1 2))",
            &Clock
        ),
        Ok("(1.5 7 3)".to_owned())
    );
}

#[test]
fn console_ports() {
    let program = vali_compiler::compile_program(
        "(import (scheme base) (scheme write))
         (display \"out \") (write \"q\")
         (write-string \"err\" (current-error-port))
         (list (read-char) (read-line))",
        &Identity::new("test"),
        &NoLibraries,
    )
    .unwrap();
    let mut cx = Cx::<DeferredHeadered>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    cx.give_input("ab\ncd").unwrap();
    let id = vali_compiler::load(&mut cx, program).unwrap();
    let result = cx.run(id).unwrap();
    assert_eq!(cx.write_string(&result).unwrap(), "(#\\a \"b\")");
    assert_eq!(cx.take_output(), "out \"q\"");
    assert_eq!(cx.take_output(), "");
    assert_eq!(cx.take_error_output(), "err");
}

#[test]
fn lists_from_r7rs() {
    check(&[
        (
            "(list (caar '((1) 2)) (cadr '(1 2)) (cdar '((1 . 3))) (cddr '(1 2 3)))",
            "(1 2 3 (3))",
        ),
        ("(map + '(1 2 3) '(10 20 30 40))", "(11 22 33)"),
        ("(map (lambda (x) (* x x)) '(1 2 3))", "(1 4 9)"),
        ("(map car '())", "()"),
        (
            "(let ((acc '())) (for-each (lambda (x y) (set! acc (cons (+ x y) acc))) '(1 2) '(3 4 5)) acc)",
            "(6 4)",
        ),
        // A long list never deepens the stack.
        (
            "(length (map (lambda (x) x) (make-list 200000 0)))",
            "200000",
        ),
        (
            "(let* ((a (list 1 2 3)) (b (list-copy a))) (set-car! b 9) (list a b))",
            "((1 2 3) (9 2 3))",
        ),
        ("(list-copy '(1 2 . 3))", "(1 2 . 3)"),
        ("(list-copy 5)", "5"),
        (
            "(list (make-list 2 'x) (length (make-list 3)))",
            "((x x) 3)",
        ),
        ("(let ((l (list 1 2 3))) (list-set! l 1 'b) l)", "(1 b 3)"),
        ("(member 2.0 '(1 2 3) =)", "(2 3)"),
        ("(member '(a) '(b (a) c))", "((a) c)"),
        ("(assoc 2.0 '((1 one) (2 two)) =)", "(2 two)"),
        ("(assoc 5 '((1 one)) =)", "#f"),
        (
            "(list (boolean=? #t #t #t) (boolean=? #f #t) (square 7))",
            "(#t #f 49)",
        ),
    ]);
}

#[test]
fn vectors_strings_and_chars_from_r7rs() {
    check(&[
        ("(vector->list #(1 2 3 4) 1)", "(2 3 4)"),
        ("(vector->list #(1 2 3 4) 1 3)", "(2 3)"),
        (
            "(let ((v (vector 1 2 3 4))) (vector-fill! v 'x 1 3) v)",
            "#(1 x x 4)",
        ),
        ("(vector-copy #(1 2 3) 1)", "#(2 3)"),
        (
            "(let ((v (vector 1 2 3 4 5))) (vector-copy! v 1 v 0 3) v)",
            "#(1 1 2 3 5)",
        ),
        (
            "(let ((v (vector 1 2 3 4 5))) (vector-copy! v 0 v 2) v)",
            "#(3 4 5 4 5)",
        ),
        ("(vector-append #(1) #() #(2 3))", "#(1 2 3)"),
        ("(vector-map + #(1 2) #(10 20 30))", "#(11 22)"),
        (
            "(let ((n 0)) (vector-for-each (lambda (x) (set! n (+ n x))) #(1 2 3)) n)",
            "6",
        ),
        (
            "(list (vector->string #(#\\a #\\b)) (string->vector \"abc\" 1))",
            "(\"ab\" #(#\\b #\\c))",
        ),
        (
            "(list (string->list \"hello\" 1 3) (string-copy \"hello\" 2))",
            "((#\\e #\\l) \"llo\")",
        ),
        ("(string-map char-upcase \"abc\")", "\"ABC\""),
        (
            "(let ((acc '())) (string-for-each (lambda (a b) (set! acc (cons (list a b) acc))) \"ab\" \"xyz\") acc)",
            "((#\\b #\\y) (#\\a #\\x))",
        ),
        (
            "(list (string>? \"b\" \"a\") (string<=? \"a\" \"a\" \"b\") (string>=? \"a\" \"b\"))",
            "(#t #t #f)",
        ),
        (
            "(list (char>? #\\b #\\a) (char<=? #\\a #\\a #\\b) (char>=? #\\a #\\b))",
            "(#t #t #f)",
        ),
    ]);
    let error = |source: &str| run(source).unwrap_err().to_string();
    assert!(error("(vector->list #(1 2) 1 5)").contains("vector->list: bad range"));
}

#[test]
fn multiple_values_from_r7rs() {
    check(&[
        (
            "(let-values (((a b) (values 1 2)) ((c) (values 3))) (list a b c))",
            "(1 2 3)",
        ),
        (
            "(let-values (((a . rest) (values 1 2 3)) (all (values 4 5))) (list a rest all))",
            "(1 (2 3) (4 5))",
        ),
        (
            "(let ((a 1)) (let-values (((a b) (values 2 a))) (list a b)))",
            "(2 1)",
        ),
        (
            "(let ((a 1)) (let*-values (((a b) (values 2 a)) ((c) (values a))) (list a b c)))",
            "(2 1 2)",
        ),
        ("(define-values (x y) (values 1 2)) (list x y)", "(1 2)"),
        (
            "(define-values (x . y) (values 1 2 3)) (list x y)",
            "(1 (2 3))",
        ),
        ("(define-values all (values 1 2)) all", "(1 2)"),
        (
            "(define (f) (define-values (a b c) (values 1 2 3)) (list c b a)) (f)",
            "(3 2 1)",
        ),
    ]);
}

#[test]
fn auxiliary_syntax_is_bound() {
    check(&[
        // A local binding of else is not cond's else.
        (
            "(let ((else #f)) (cond (else 'matched) (#t 'not-else)))",
            "not-else",
        ),
        ("(cond (#f 1) (else 2))", "2"),
        ("(case 3 ((1) 'one) (else => (lambda (x) (* x 2))))", "6"),
    ]);
    let compile_error = |source: &str| compile(source, "test").unwrap_err().message;
    assert!(compile_error("(else)").contains("misplaced auxiliary syntax"));
    assert!(
        compile_error("(define-syntax m (syntax-rules () ((_) (syntax-error \"no way\" 1)))) (m)")
            .contains("no way")
    );
}

#[test]
fn case_lambda_and_promises() {
    check(&[
        (
            "(import (scheme base) (scheme case-lambda))
             (define f (case-lambda ((x) (list 'one x)) ((x y) (list 'two x y)) ((x . r) (list 'many x r))))
             (list (f 1) (f 1 2) (f 1 2 3))",
            "((one 1) (two 1 2) (many 1 (2 3)))",
        ),
        (
            "(import (scheme base) (scheme lazy))
             (define n 0)
             (define p (delay (begin (set! n (+ n 1)) n)))
             (list (force p) (force p) (promise? p) (force (make-promise 5)) (force 7))",
            "(1 1 #t 5 7)",
        ),
        // A long chain of delay-forces runs in constant space.
        (
            "(import (scheme base) (scheme lazy))
             (define (loop n) (delay-force (if (= n 0) (delay 'done) (loop (- n 1)))))
             (force (loop 100000))",
            "done",
        ),
    ]);
    let error = |source: &str| run(source).unwrap_err().to_string();
    assert!(
        error("(import (scheme base) (scheme case-lambda)) ((case-lambda ((x) x)))")
            .contains("no clause accepts")
    );
}

/// Builds and sums a 20,000-element structure ten times: about 2 MiB each
/// time, so epochs run mid-program, at calls and loops' backward jumps.
const CHURN: &str = "
    (define (build n)
      (let loop ((i 0) (acc '()))
        (if (= i n) acc (loop (+ i 1) (cons (vector i (list i i)) acc)))))
    (define (sum xs)
      (let loop ((xs xs) (t 0))
        (if (null? xs) t (loop (cdr xs) (+ t (vector-ref (car xs) 0))))))
    (let loop ((k 0) (total 0))
      (if (= k 10) total (loop (+ k 1) (+ total (sum (build 20000))))))";

#[test]
fn epochs_run_mid_program() {
    check(&[(CHURN, "1999900000")]);
}

/// The heap a program leaves behind under strategy `S`.
fn bytes_after<S: Strategy>(source: &str) -> usize {
    let program =
        vali_compiler::compile_program(source, &Identity::new("test"), &NoLibraries).unwrap();
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    let id = vali_compiler::load(&mut cx, program).unwrap();
    cx.run(id).unwrap();
    cx.bytes_used()
}

/// Makes garbage cycles of every kind, 50,000 times: pairs through
/// `set-cdr!`, a vector and a closure that refer to each other, and
/// procedures that call each other. One cycle lives in a variable and
/// another only in a frame, through cycle collections.
const CYCLES: &str = "
    (define (ring)
      (let ((p (list 1 2 3)))
        (set-cdr! (cdr (cdr p)) p)
        p))
    (define (knot)
      (let ((v (make-vector 1 #f)))
        (vector-set! v 0 (lambda () v))
        v))
    (define (parity)
      (letrec ((ev? (lambda (n) (if (= n 0) #t (od? (- n 1)))))
               (od? (lambda (n) (if (= n 0) #f (ev? (- n 1))))))
        ev?))
    (define keep (ring))
    (define (churn n)
      (let ((held (knot)))
        (let loop ((i 0) (total 0))
          (if (= i n)
              (list total (eq? ((vector-ref held 0)) held))
              (let ((r (ring)) (k (knot)) (e (parity)))
                (loop (+ i 1)
                      (+ total (car r)
                         (if (eq? ((vector-ref k 0)) k) 1 0)
                         (if (e 4) 1 0))))))))
    (list (churn 50000) (car keep) (car (cdr (cdr (cdr keep)))))";

#[test]
fn cycles_are_collected() {
    check(&[(CYCLES, "((150000 #t) 1 1)")]);
    fn live_after<S: Strategy<Heap = CountingHeap>>(source: &str) -> (usize, usize) {
        let program =
            vali_compiler::compile_program(source, &Identity::new("test"), &NoLibraries).unwrap();
        let mut cx = Cx::<S>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let id = vali_compiler::load(&mut cx, program).unwrap();
        cx.run(id).unwrap();
        let before = cx.heap().bytes_live();
        cx.collect_garbage();
        (before, cx.heap().bytes_live())
    }
    for (name, (before, after)) in [
        ("deferred", live_after::<DeferredHeadered>(CYCLES)),
        ("deferred-side", live_after::<DeferredSideCounts>(CYCLES)),
        ("precise", live_after::<PreciseHeadered>(CYCLES)),
        ("precise-side", live_after::<PreciseSideCounts>(CYCLES)),
    ] {
        // Collections mid-program keep the heap near their 4 MiB
        // threshold, and a last one leaves only what the program's
        // variables and the runtime keep: about 2 KiB.
        assert!(
            before < 8 << 20,
            "{name}: {before} bytes live before the last collection"
        );
        assert!(after < 64 << 10, "{name}: {after} bytes live after it");
    }
}

#[test]
fn counting_reclaims_memory() {
    let none = bytes_after::<NoCollection>(CHURN);
    for (name, used) in [
        ("deferred", bytes_after::<DeferredHeadered>(CHURN)),
        ("deferred-side", bytes_after::<DeferredSideCounts>(CHURN)),
        ("precise", bytes_after::<PreciseHeadered>(CHURN)),
        ("precise-side", bytes_after::<PreciseSideCounts>(CHURN)),
    ] {
        // Only one structure lives at a time, plus an epoch's garbage.
        assert!(
            used * 3 < none,
            "{name}: {used} bytes, against {none} without collection"
        );
    }
}

/// Under a counting strategy, every frame path leaves the stack counted
/// out: after a collection, repeating a program's work leaves no more
/// behind, however it called, returned, unwound or chained stack segments.
#[test]
fn frames_leave_nothing_counted() {
    fn live_after<S: Strategy<Heap = CountingHeap>>(source: &str) -> usize {
        let program =
            vali_compiler::compile_program(source, &Identity::new("test"), &NoLibraries).unwrap();
        let mut cx = Cx::<S>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let id = vali_compiler::load(&mut cx, program).unwrap();
        // Some of these fail on purpose; what they leave is what counts.
        drop(cx.run(id));
        cx.collect_garbage();
        cx.heap().bytes_live()
    }
    fn each_strategy(check: impl Fn(&str, fn(&str) -> usize)) {
        check("deferred", live_after::<DeferredHeadered>);
        check("precise", live_after::<PreciseHeadered>);
        check("precise-side", live_after::<PreciseSideCounts>);
    }
    // Constants live as long as their module, so these programs quote
    // nothing that is allocated.
    let prelude = "
        (import (scheme base) (only (vali primitive) %call/cc %continue %catch))
        (define (ring) (let ((p (list 1 2 3))) (set-cdr! (cdr (cdr p)) p) p))
        (define (rep f n) (let loop ((i 0)) (if (< i n) (begin (f) (loop (+ i 1))))))
        (define (deep n) (if (= n 0) (list (ring)) (cons (ring) (deep (- n 1)))))
        (define (rest . xs) (cons (ring) xs))
        (define (tail-native x) (list x (ring)))
        (define (deep-k n)
          (if (= n 0)
              (%call/cc (lambda (k) (list (ring) k)))
              (cons (ring) (deep-k (- n 1)))))
    ";
    let repeated = [
        "(ring)",
        "(list (ring) (ring))",
        // Recursion deep enough to chain stack segments.
        "(length (deep 30000))",
        "(rest (ring) (ring) (ring) (ring))",
        "(apply rest (map (lambda (i) (ring)) (list 1 2 3 4 5 6 7 8 9 10)))",
        "(tail-native (ring))",
        "(guard (e (#t (ring))) (raise (ring)))",
        "(guard (e (#t e)) (car (ring)) (vector-ref (vector (ring)) 5))",
        // Continuations: returned to, escaped through, re-entered, in a
        // cycle with what holds them, and sealing frames deep enough to
        // chain segments, all copied back.
        "(%call/cc (lambda (k) (ring)))",
        "(list (%call/cc (lambda (k) (%continue k (ring)))))",
        "(let ((p (list 1))) (%call/cc (lambda (k) (set-car! p k))) (cons p (ring)))",
        "(let ((n 0)) (let ((k (%call/cc (lambda (k) k)))) (set! n (+ n 1)) (if (< n 3) (%continue k k) (ring))))",
        "(length (deep-k 30000))",
        "(%catch (lambda () (%call/cc (lambda (k) (%continue k (ring))))) (lambda (c) c))",
        "(%catch (lambda () (list (%call/cc (lambda (k) (raise (ring)))))) (lambda (c) c))",
        // The dynamic environment: escaping a dynamic-wind, guard
        // re-entering a raise, a handler returning, parameterize.
        "(call/cc (lambda (k) (dynamic-wind (lambda () #f) (lambda () (k (ring))) (lambda () #f))))",
        "(with-exception-handler (lambda (e) (ring)) (lambda () (guard (e ((string? e) 'no)) (raise-continuable (ring)))))",
        "(guard (e (#t e)) (with-exception-handler (lambda (e) (ring)) (lambda () (car 5))))",
        "(parameterize ((current-output-port (current-output-port))) (ring))",
    ];
    for body in repeated {
        // Each must run, or it would check nothing.
        if let Err(e) = run(&format!("{prelude}(rep (lambda () {body}) 1)")) {
            panic!("{body}: {e}");
        }
        each_strategy(|name, live| {
            let times = |n: usize| live(&format!("{prelude}(rep (lambda () {body}) {n})"));
            assert_eq!(times(4), times(2), "{name}: {body}");
        });
    }
    // Raised to the host: the same program raising a number instead of
    // allocating leaves the same behind.
    let raised = [
        ("(let ((r (ring))) (raise r))", "(let ((r 5)) (raise r))"),
        (
            "(define (fail n) (if (= n 0) (car (ring) 5) (list (ring) (fail (- n 1))))) (fail 100)",
            "(define (fail n) (if (= n 0) (car 5 5) (list 5 (fail (- n 1))))) (fail 100)",
        ),
    ];
    for (program, control) in raised {
        each_strategy(|name, live| {
            assert_eq!(
                live(&format!("{prelude}{program}")),
                live(&format!("{prelude}{control}")),
                "{name}: {program}"
            );
        });
    }
}
