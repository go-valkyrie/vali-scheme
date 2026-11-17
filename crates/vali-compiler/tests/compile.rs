//! Compiling Scheme and running it on the tier-0 VM.

use vali_compiler::compile;
use vali_runtime::{Cx, Error, HeapConfig, NoCollection};

/// Compiles and runs `source`, returning its value as `write` prints it.
fn run(source: &str) -> Result<String, Error> {
    let module = compile(source, "test").unwrap_or_else(|e| panic!("{source}\n{e}"));
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    cx.install_primitives()?;
    let id = cx.load(module)?;
    let result = cx.run(id)?;
    cx.write_string(&result)
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
    assert!(compile_error("(quote-syntax x)").contains("not supported yet"));
}
