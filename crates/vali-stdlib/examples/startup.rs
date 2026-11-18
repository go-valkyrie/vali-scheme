//! Times compiling and loading small programs with the prebuilt standard
//! libraries: `cargo run --release -p vali-stdlib --example startup`.
//! Setting `VALI_NO_PREBUILT` compiles them from source instead, to compare.

use std::time::{Duration, Instant};

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Source, compile_program};
use vali_runtime::{Cx, DeferredHeadered, HeapConfig};

/// Serves nothing: the programs use only Vali's own libraries.
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

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

fn main() {
    if std::env::var_os("VALI_NO_PREBUILT").is_none() {
        vali_stdlib::install();
    }
    let programs = [
        ("(scheme base)", "(import (scheme base)) (+ 1 2)"),
        (
            "r7rs-small",
            "(import (scheme base) (scheme char) (scheme complex) (scheme cxr) \
             (scheme inexact) (scheme lazy) (scheme read) (scheme write) \
             (scheme case-lambda) (scheme eval)) (+ 1 2)",
        ),
    ];
    let runs = 51;
    for (name, source) in programs {
        let mut compile = Vec::new();
        let mut load = Vec::new();
        for _ in 0..runs {
            let start = Instant::now();
            let program = compile_program(source, &Identity::new("main"), &NoLibraries)
                .unwrap_or_else(|e| panic!("{e}"));
            let compiled = Instant::now();
            let mut cx = Cx::<DeferredHeadered>::new(&HeapConfig::default());
            cx.install_primitives().expect("installing the primitives");
            let id = vali_compiler::load(&mut cx, program).expect("loading");
            cx.run(id).expect("running");
            compile.push(compiled - start);
            load.push(compiled.elapsed());
        }
        println!(
            "{name:16} first {:>10.3?}   compile {:>10.3?}   load+run {:>10.3?}",
            compile[0] + load[0],
            median(compile),
            median(load)
        );
    }
    let mut repl = Vec::new();
    for _ in 0..runs {
        let start = Instant::now();
        vali_compiler::Repl::<DeferredHeadered>::new(&HeapConfig::default(), None)
            .expect("starting the REPL");
        repl.push(start.elapsed());
    }
    let first = repl[0];
    println!(
        "{:16} first {:>10.3?}   start {:>10.3?}",
        "REPL",
        first,
        median(repl)
    );
}
