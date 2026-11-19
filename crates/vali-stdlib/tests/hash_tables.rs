//! Hash tables under resource limits (ADR 0023): a table that grows past the
//! memory limit raises the typed out-of-memory error instead of aborting,
//! and one that stays under it behaves the same.

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Source, compile_program};
use vali_runtime::{
    Cx, DeferredHeadered, DeferredSideCounts, Error, HeapConfig, NoCollection, PreciseHeadered,
    PreciseSideCounts, Strategy,
};

struct NoHost;

impl Loader for NoHost {
    fn load(
        &self,
        _name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok(None)
    }
}

fn run<S: Strategy>(source: &str, max_bytes: Option<usize>) -> Result<String, Error> {
    vali_stdlib::install();
    let program =
        compile_program(source, &Identity::new("main"), &NoHost).unwrap_or_else(|e| panic!("{e}"));
    let mut cx = Cx::<S>::new(&HeapConfig {
        max_bytes,
        ..Default::default()
    });
    cx.install_primitives().unwrap();
    let id = vali_compiler::load(&mut cx, program).unwrap();
    let result = cx.run(id)?;
    Ok(cx.write_string(&result).unwrap())
}

const FILL: &str = r#"
(import (scheme base) (srfi 128) (srfi 125))
(define t (make-hash-table (make-equal-comparator)))
(define (fill i)
  (hash-table-set! t (list i (number->string i)) i)
  (fill (+ i 1)))
(fill 0)"#;

const SMALL: &str = r#"
(import (scheme base) (srfi 128) (srfi 125))
(define t (make-hash-table (make-default-comparator)))
(do ((i 0 (+ i 1))) ((= i 1000)) (hash-table-set! t (list i "k") i))
(list (hash-table-size t) (hash-table-ref/default t (list 500 "k") #f))"#;

fn check<S: Strategy>() {
    let limit = vali_runtime::PAGE_BYTES * 64;
    assert_eq!(run::<S>(FILL, Some(limit)).unwrap_err(), Error::OutOfMemory);
    assert_eq!(run::<S>(SMALL, Some(limit)).unwrap(), "(1000 500)");
}

#[test]
fn a_table_that_outgrows_the_memory_limit_raises_out_of_memory() {
    check::<NoCollection>();
    check::<DeferredHeadered>();
    check::<DeferredSideCounts>();
    check::<PreciseHeadered>();
    check::<PreciseSideCounts>();
}
