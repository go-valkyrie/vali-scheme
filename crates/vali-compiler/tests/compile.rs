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
        (define (ring) (let ((p (list 1 2 3))) (set-cdr! (cdr (cdr p)) p) p))
        (define (rep f n) (let loop ((i 0)) (if (< i n) (begin (f) (loop (+ i 1))))))
        (define (deep n) (if (= n 0) (list (ring)) (cons (ring) (deep (- n 1)))))
        (define (rest . xs) (cons (ring) xs))
        (define (tail-native x) (list x (ring)))
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
