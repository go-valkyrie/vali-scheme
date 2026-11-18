//! Lowering to the IR (ADR 0017), as the IR's printer shows it.

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

/// The printed IR of the program `source`'s body.
fn ir(source: &str) -> String {
    let program =
        vali_compiler::compile_internal_program(source, &Identity::new("test"), &NoLibraries)
            .unwrap();
    program.ir.expect("every module is lowered").to_string()
}

/// Asserts that the IR of `source` has the functions `expected`.
#[track_caller]
fn check(source: &str, expected: &str) {
    let ir = ir(source);
    assert!(ir.contains(expected), "{source}\n{ir}");
}

#[test]
fn a_named_let_is_a_closure_tied_to_itself() {
    // Contification makes it a loop (vali-opt's tests).
    check(
        "(define (sum l) (let loop ((l l) (acc 0)) (if (null? l) acc (loop (cdr l) (+ acc (car l))))))",
        "fun f1 sum (v0) []:
  let v1 = closure f2 [c0]
  let v2 = set-free v1 0 v1
  tail v1(v0 c1) @0:1:16

fun f2 loop (v1 v2) [v0]:
  let v3 = null? v1
  if v3:
    return v2
  else:
    let v4 = cdr v1 @0:1:66
    let v5 = car v1 @0:1:81
    let v6 = + v2 v5 @0:1:74
    tail v0(v4 v6) @0:1:60
",
    );
}

#[test]
fn values_of_ifs_go_to_join_points() {
    check(
        "(define (f x) (+ 1 (if (< x 0) (- x) x)))",
        "fun f1 f (v0) []:
  let v1 = < v0 c1 @0:1:23
  join j0(v2):
    let v4 = + c0 v2 @0:1:14
    return v4
  if v1:
    let v3 = - c1 v0 @0:1:31
    jump j0(v3)
  else:
    jump j0(v0)
",
    );
}

#[test]
fn letrec_closures_are_tied_with_set_free() {
    check(
        "(define (even? n) (letrec ((e? (lambda (n) (if (= n 0) #t (o? (- n 1))))) (o? (lambda (n) (if (= n 0) #f (e? (- n 1)))))) (e? n)))",
        "fun f1 even? (v0) []:
  let v1 = closure f2 [c0]
  let v2 = closure f3 [v1]
  let v3 = set-free v1 0 v2
  tail v1(v0) @0:1:122

fun f2 e? (v1) [v0]:
  let v2 = = v1 c1 @0:1:47
  if v2:
    return c2
  else:
    let v3 = - v1 c3 @0:1:62
    tail v0(v3) @0:1:58
",
    );
}
