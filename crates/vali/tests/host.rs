//! A host that depends on `vali` alone: it bridges a Rust type with the
//! attribute macros, compiles a program that uses it, and runs it in an
//! isolate.

use std::sync::Arc;

use vali::bridge::NativeLibrary;
use vali::load::{Identity, LibraryName, LoadError, Loader, Source};
use vali::{HeapConfig, Isolate};

#[vali::object]
struct Counter {
    n: i64,
}

#[vali::bridge]
impl Counter {
    #[scheme(name = "make-counter")]
    fn new(start: i64) -> Counter {
        Counter { n: start }
    }

    #[scheme(name = "counter-add!")]
    fn add(&mut self, by: Option<i64>) -> i64 {
        self.n += by.unwrap_or(1);
        self.n
    }
}

/// Serves `(host counters)` as a native library.
struct Counters;

impl Loader for Counters {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok((name.to_string() == "(host counters)").then(|| {
            let library = Counter::bridge(NativeLibrary::new(&["host", "counters"]));
            Source::native(Identity::new("native:host/counters"), Arc::new(library))
        }))
    }
}

#[test]
fn a_host_runs_a_program_that_uses_its_bridges() {
    let program = vali::compile::compile_program(
        "(import (scheme base) (host counters))
         (let ((c (make-counter 40)))
           (counter-add! c)
           (counter-add! c 1))",
        &Identity::new("host"),
        &Counters,
    )
    .unwrap();
    let mut isolate: Isolate = Isolate::new(&HeapConfig::default());
    let result = isolate
        .with(|cx| {
            let id = vali::compile::load(cx, program)?;
            let result = cx.run(id)?;
            cx.write_string(&result)
        })
        .unwrap();
    assert_eq!(result, "42");
}
