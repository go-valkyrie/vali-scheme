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
#[ignore = "the derived forms return as macros with the prelude"]
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
