//! Times compiling a program of n chained definitions, for n doubling: how
//! compile time grows. Run with
//! `cargo run --release -p vali-compiler --example scaling`.

use std::time::Instant;

use vali_compiler::{Identity, LibraryName, LoadError, Loader, Source};

struct NoLibraries;

impl Loader for NoLibraries {
    fn load(&self, _: &LibraryName, _: Option<&Identity>) -> Result<Option<Source>, LoadError> {
        Ok(None)
    }
}

fn program(n: usize) -> String {
    let mut text = String::from("(define (f0 x) x)\n");
    for i in 1..=n {
        text.push_str(&format!(
            "(define (f{i} x) (let ((y (+ x 1))) (f{} y)))\n",
            i - 1
        ));
    }
    text
}

fn main() {
    // With a size, compile that once, for a profiler.
    if let Some(n) = std::env::args().nth(1) {
        let source = program(n.parse().expect("a size"));
        vali_compiler::compile_program(&source, &Identity::new("scaling"), &NoLibraries)
            .expect("compiles");
        return;
    }
    let time = |n| {
        let source = program(n);
        (0..3)
            .map(|_| {
                let start = Instant::now();
                vali_compiler::compile_program(&source, &Identity::new("scaling"), &NoLibraries)
                    .expect("compiles");
                start.elapsed()
            })
            .min()
            .expect("three runs")
    };
    let base = time(0);
    println!("n=0: {base:?}");
    let mut last = None;
    for n in [500, 1000, 2000, 4000, 8000, 16000] {
        let t = time(n).saturating_sub(base);
        let growth = last.map(|l: std::time::Duration| t.as_secs_f64() / l.as_secs_f64());
        println!(
            "n={n}: {t:?} above n=0{}",
            growth.map_or(String::new(), |g| format!(", {g:.2}x the last"))
        );
        last = Some(t);
    }
}
