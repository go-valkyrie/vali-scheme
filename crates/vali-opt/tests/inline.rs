//! Inlining, as the IR of compiled programs shows it.

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

#[test]
fn small_procedures_are_inlined() {
    // `sq`'s body replaces the call, keeping its position.
    let sq = ir("(define (sq x) (* x x)) (define (f y) (+ (sq y) 1))");
    assert!(
        sq.contains("fun f2 f (v0) []:\n  let v5 = * v0 v0 @0:1:15/i0\n  let v3 = + v5 c1 @0:1:38\n  return v3\n  inlined i0 sq @0:1:41\n"),
        "{sq}"
    );
    // In tail position, its returns are the caller's.
    let tail = ir("(define (sq x) (* x x)) (define (f y) (sq y))");
    assert!(
        tail.contains("fun f2 f (v0) []:\n  let v3 = * v0 v0 @0:1:15/i0\n  return v3\n"),
        "{tail}"
    );
    // Its constants fold with the caller's arguments.
    let folded = ir("(define (sq x) (* x x)) (define (f) (sq 3))");
    assert!(folded.contains("fun f2 f () []:\n  return c"), "{folded}");
}

#[test]
fn some_procedures_are_not_inlined() {
    // Recursive: it would never end.
    let fib = ir("(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))) (fib 30)");
    assert!(fib.contains("call direct f1"), "{fib}");
    // Too big.
    let big =
        ir("(define (big x) (+ x x x x x x x x x x x x x x x x x x x x)) (define (f y) (big y))");
    assert!(big.contains("tail direct f1"), "{big}");
    // Assigned, so not known.
    let assigned = ir("(define (sq x) (* x x)) (define (f y) (sq y)) (set! sq car)");
    assert!(assigned.contains("tail v"), "{assigned}");
}

#[test]
fn inlined_procedures_may_call_others() {
    // Inlined into each other, down to the leaves, which stay calls when
    // they are recursive.
    let nested = ir("(define (loop n) (if (= n 0) 0 (loop (- n 1))))
         (define (g x) (loop x))
         (define (h x) (+ 1 (g x)))");
    assert!(
        nested.contains(
            "fun f3 h (v0) []:\n  let v6 = call direct f1(v0) @0:2:23/i0\n  let v3 = + c1 v6 @0:3:23\n  return v3\n  inlined i0 g @0:3:28\n"
        ),
        "{nested}"
    );
}
