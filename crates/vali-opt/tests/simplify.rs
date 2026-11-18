//! Simplification, as the IR of compiled programs shows it.

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Source};

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

/// The printed, optimised IR of `source`'s body.
fn ir(source: &str) -> String {
    let mut ir =
        vali_compiler::compile_internal_program(source, &Identity::new("test"), &NoLibraries)
            .unwrap_or_else(|e| panic!("{source}\n{e}"))
            .ir
            .expect("every module is lowered");
    vali_opt::optimise(&mut ir);
    ir.to_string()
}

/// Asserts that the optimised IR of `source` has `expected`.
#[track_caller]
fn check(source: &str, expected: &str) {
    let ir = ir(source);
    assert!(ir.contains(expected), "{source}\n{ir}");
}

#[test]
fn constants_fold() {
    // (+ 1 (* 2 3)) is 7, and the constants it no longer needs are gone.
    let ir = ir("(define (f) (+ 1 (* 2 3)))");
    assert!(ir.contains("c0 = #<unspecified>\nc1 = 7\ng0"), "{ir}");
    assert!(ir.contains("fun f1 f () []:\n  return c1\n"), "{ir}");
    check(
        "(define (q) (car (cdr '(1 2 3))))",
        "fun f1 q () []:\n  return c1\n",
    );
}

#[test]
fn ifs_on_constants_keep_one_arm() {
    check(
        "(define (g x) (if (null? '()) x 0))",
        "fun f1 g (v0) []:\n  return v0\n",
    );
    // The join point the `if`'s value went to is just its handler now.
    check(
        "(define (k x) (+ 1 (if (< 1 2) x 0)))",
        "fun f1 k (v0) []:\n  let v3 = + c0 v0 @0:1:14\n  return v3\n",
    );
}

#[test]
fn assigned_variables_need_no_box_without_calls() {
    // i is assigned, but nothing captures it and nothing is called: it is
    // a parameter of the loop and of the join point after it.
    let counted = ir(
        "(define (count n) (define i 0) (let loop () (if (< i n) (begin (set! i (+ i 1)) (loop)))) i)",
    );
    assert!(!counted.contains("box"), "{counted}");
    println!("{counted}");
    // A call in its scope could capture a continuation, which copies the
    // frame: the box stays.
    let called = ir("(define (f g) (define i 0) (g) (set! i 1) i)");
    assert!(called.contains("box"), "{called}");
    // So does a closure that captures it.
    let captured = ir("(define (f) (define i 0) (lambda () (set! i 1) i))");
    assert!(captured.contains("box"), "{captured}");
}

#[test]
fn unused_values_go_unless_they_may_raise() {
    // An unused allocation goes; a copy is replaced by what it copies.
    check(
        "(define (h x) (let ((y x) (z (cons x x))) y))",
        "fun f1 h (v0) []:\n  return v0\n",
    );
    // car may raise, so it stays though its value is unused.
    check(
        "(define (e x) (car x) (vector-length '#(1 2 3)))",
        "fun f1 e (v0) []:\n  let v1 = car v0 @0:1:14\n  return c1\n",
    );
}

#[test]
fn named_lets_become_loops() {
    // Entered by a tail call: the loop's returns are the function's, and
    // its closure function is gone.
    let sum = ir(
        "(define (sum l) (let loop ((l l) (acc 0)) (if (null? l) acc (loop (cdr l) (+ acc (car l))))))",
    );
    assert!(
        sum.contains(
            "fun f1 sum (v0) []:
  rec join j0(v4 v5):
    let v6 = null? v4
    if v6:
      return v5
    else:
      let v7 = cdr v4 @0:1:66
      let v8 = car v4 @0:1:81
      let v9 = + v5 v8 @0:1:74
      jump j0(v7 v9)
  jump j0(v0 c1)
"
        ),
        "{sum}"
    );
    assert!(!sum.contains("fun f2"), "{sum}");
    // Entered for its value: its returns jump to what follows.
    check(
        "(define (g x) (let ((n (let lp ((i x)) (if (> i 10) i (lp (* i 2)))))) (vector n x n)))",
        "fun f1 g (v0) []:
  join j1(v3):
    let v4 = make-vector c3 v3 @0:1:71
    let v5 = vector-set! v4 c4 v0 @0:1:71
    return v4
  rec join j0(v7):
    let v8 = < c1 v7 @0:1:43
    if v8:
      jump j1(v7)
    else:
      let v9 = * v7 c2 @0:1:58
      jump j0(v9)
  jump j0(v0)
",
    );
    // A loop that escapes stays a closure.
    let escapes = ir("(define (h) (letrec ((f (lambda () f))) (f)))");
    assert!(escapes.contains("closure"), "{escapes}");
}
