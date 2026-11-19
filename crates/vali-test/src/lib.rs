//! Vali's test harness.
//!
//! Scheme test files carry a [`Header`] of `;;; key: value` comment lines
//! and assert with SRFI 64. [`discover`] finds them, and [`trials`] turns
//! each into a `cargo test` test, run through a [`Backend`]. A test marked
//! `expect: fail` passes while it fails, and fails the build the moment it
//! passes, so a fixed regression can't stay marked as expected to fail.
//!
//! [`Evaluate`] compiles and runs each file; [`ReadOnly`] only checks that
//! each file reads as Scheme.
//!
//! What [`Evaluate`] does depends on the file's kind:
//! - `scheme`: runs it under every memory strategy; it passes if every run
//!   ends. A strategy that frees too early shows up here as a wrong answer
//!   or a crash, which the one strategy that never frees cannot show.
//! - `host`: the same, in the host fixture `host:` names. `in-memory-loader`
//!   also serves `(regression virtual)` from memory; `expansion-reads-scoped`
//!   lets expansion read the test's `data/` directory and nothing else.
//!   `library-path` serves libraries from the test's `lib/` directory, as
//!   `lib/a/b.sld` for `(a b)`, and lets them include files from the
//!   test's own directory. The conformance suites use it for the shims
//!   that stand in for an upstream suite's test library.
//!   A host test has no capabilities unless its `grant:` line asks for
//!   them: `clock` serves `(scheme time)`, and `read` serves `(scheme
//!   file)` for reading files and asking whether they exist, below the
//!   test's own directory and nowhere else (ADR 0009).
//! - `performance`: the file defines `(program n)`, returning a program as
//!   data. Compiling `(program (* 4 n))` must take at most twice as long
//!   as linear growth would, eight times as long as `(program n)` for an
//!   `n` large enough to time, where a quadratic pass would take sixteen.
//! - `benchmark`: run under every memory strategy, timing each run (not
//!   compiling); it passes if every run ends. The times go to
//!   `target/vali-benchmarks/<name>.json`, to track over time. What share of
//!   the time goes where needs a sampling profiler, which comes later. One
//!   marked `expect: fail` benchmarks something not built yet, and is not
//!   run.

pub mod fuzz;
mod header;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use libtest_mimic::{Failed, Trial};
use vali_syntax::{SourceId, TextSize};

pub use header::{Grant, Header, HeaderError, Kind, parse_header};

/// A test file: its name, where it is, and its text.
#[derive(Clone, Debug)]
pub struct TestFile {
    /// The file name without `.scm`, which is also the test's name.
    pub name: String,
    /// The file's path.
    pub path: PathBuf,
    /// The file's contents.
    pub source: String,
}

/// What running a test file produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every assertion held.
    Passed,
    /// An assertion failed, or the code under test raised an error.
    Failed(String),
    /// The backend cannot run this kind of test yet.
    Unsupported(String),
    /// The test file itself is broken, for example it does not read as
    /// Scheme. Always a failure, even for a test expected to fail.
    Invalid(String),
}

/// Runs test files.
pub trait Backend {
    /// Runs `file`, whose header is `header`.
    fn run(&self, file: &TestFile, header: &Header) -> Outcome;
}

/// A backend that only reads each file. Reading errors make the file
/// [`Outcome::Invalid`]; otherwise it is [`Outcome::Unsupported`], since
/// nothing can evaluate it yet.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadOnly;

impl Backend for ReadOnly {
    fn run(&self, file: &TestFile, _header: &Header) -> Outcome {
        let read = vali_syntax::read(&file.source, SourceId(0));
        if read.errors.is_empty() {
            return Outcome::Unsupported("no evaluator yet; the file reads as Scheme".to_owned());
        }
        let errors: Vec<String> = read
            .errors
            .iter()
            .map(|e| {
                let (line, column) = line_column(&file.source, e.range.start());
                format!("{}:{line}:{column}: {}", file.path.display(), e.kind)
            })
            .collect();
        Outcome::Invalid(errors.join("\n"))
    }
}

/// A backend that compiles and runs each `kind: scheme` file in a fresh
/// isolate, with the standard libraries. A file passes if it runs to the
/// end: the runner vali-test installs ends the run with exit code 1, which
/// this reports as an error, when the outermost `test-end` finds a test that
/// failed or unexpectedly passed.
/// Files that don't read are [`Outcome::Invalid`]; other kinds of test are
/// [`Outcome::Unsupported`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Evaluate;

impl Backend for Evaluate {
    fn run(&self, file: &TestFile, header: &Header) -> Outcome {
        if let outcome @ Outcome::Invalid(_) = ReadOnly.run(file, header) {
            return outcome;
        }
        let mut host = TestHost::new(file);
        host.grants = header.grants.clone();
        match (header.kind, header.host.as_deref()) {
            (Kind::Scheme, _) => {}
            (Kind::Host, Some("in-memory-loader")) => host.in_memory = true,
            (Kind::Host, Some("expansion-reads-scoped")) => {
                host.readable = file.path.parent().map(|dir| dir.join("data"));
            }
            (Kind::Host, Some("library-path")) => {
                host.libraries = file.path.parent().map(|dir| dir.join("lib"));
                host.readable = file.path.parent().map(Path::to_path_buf);
            }
            (Kind::Host, Some(other)) => {
                return Outcome::Unsupported(format!("no host fixture named {other}"));
            }
            (Kind::Performance, _) => return performance(file, &host),
            // Running one whose subject isn't built yet would time nothing.
            (Kind::Benchmark, _) if header.expect_fail.is_some() => {
                return Outcome::Unsupported("a benchmark of something not built yet".to_owned());
            }
            (Kind::Benchmark, _) => return benchmark(file, &host),
            (kind, _) => {
                return Outcome::Unsupported(format!("cannot run {kind:?} tests yet"));
            }
        }
        match run_everywhere(file, &host) {
            Ok(()) => Outcome::Passed,
            Err(e) => Outcome::Failed(e),
        }
    }
}

/// The runner every test file gets: the simple runner, which prints what
/// failed and a summary, and ends the run with exit code 1 when a test
/// failed or unexpectedly passed. Installed before the file loads, in the
/// same isolate, so the file's `(srfi 64)` is the one it configured.
const RUNNER: &str = "
(import (scheme base) (scheme process-context) (srfi 64))
(test-runner-factory
  (lambda ()
    (let ((runner (test-runner-simple)))
      (test-runner-on-final! runner
        (lambda (r)
          (test-on-final-simple r)
          (when (or (> (test-runner-fail-count r) 0)
                    (> (test-runner-xpass-count r) 0))
            (exit 1))))
      runner)))";

/// Installs [`RUNNER`] in `cx`.
fn install_runner<S: vali_runtime::Strategy>(
    cx: &mut vali_runtime::Cx<S>,
    host: &TestHost,
) -> Result<(), String> {
    let identity = vali_compiler::Identity::new("vali-test:runner");
    let program = vali_compiler::compile_program(RUNNER, &identity, host)
        .map_err(|e| format!("compiling the test runner: {e}"))?;
    let id =
        vali_compiler::load_with(cx, program, Arc::new(host.clone())).map_err(|e| e.to_string())?;
    cx.run(id).map_err(|e| e.to_string())?;
    Ok(())
}

/// Compiles `file`, then runs it in a fresh isolate under strategy `S`,
/// returning how long running took.
fn timed<S: vali_runtime::Strategy>(file: &TestFile, host: &TestHost) -> Result<Duration, String> {
    let identity = vali_compiler::Identity::new(&file.name);
    let program = vali_compiler::compile_program(&file.source, &identity, host)
        .map_err(|e| format!("compiling: {e}"))?;
    let mut cx = vali_runtime::Cx::<S>::new(&Default::default());
    cx.install_primitives().map_err(|e| e.to_string())?;
    install_runner(&mut cx, host)?;
    let id = vali_compiler::load_with(&mut cx, program, Arc::new(host.clone()))
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    cx.run(id).map_err(|e| format!("under {}: {e}", S::NAME))?;
    Ok(start.elapsed())
}

/// Runs a benchmark: see the module documentation.
fn benchmark(file: &TestFile, host: &TestHost) -> Outcome {
    use vali_runtime::{
        DeferredHeadered, DeferredSideCounts, NoCollection, PreciseHeadered, PreciseSideCounts,
        Strategy,
    };
    let runs = [
        (NoCollection::NAME, timed::<NoCollection>(file, host)),
        (
            DeferredHeadered::NAME,
            timed::<DeferredHeadered>(file, host),
        ),
        (
            DeferredSideCounts::NAME,
            timed::<DeferredSideCounts>(file, host),
        ),
        (PreciseHeadered::NAME, timed::<PreciseHeadered>(file, host)),
        (
            PreciseSideCounts::NAME,
            timed::<PreciseSideCounts>(file, host),
        ),
    ];
    let mut times = Vec::new();
    for (strategy, run) in runs {
        match run {
            Ok(time) => times.push(format!(
                "\"{strategy}\": {:.3}",
                time.as_secs_f64() * 1000.0
            )),
            Err(e) => return Outcome::Failed(e),
        }
    }
    let report = format!(
        "{{\"benchmark\": \"{}\", \"build\": \"{}\", \"milliseconds\": {{{}}}}}\n",
        file.name,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        times.join(", ")
    );
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/vali-benchmarks");
    let written = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(dir.join(format!("{}.json", file.name)), report));
    match written {
        Ok(()) => Outcome::Passed,
        Err(e) => Outcome::Failed(format!("writing the benchmark's times: {e}")),
    }
}

/// Compiles and runs `file` in a fresh isolate, with `host`'s libraries,
/// returning the isolate and the program's module.
fn run<S: vali_runtime::Strategy>(
    file: &TestFile,
    host: &TestHost,
) -> Result<(vali_runtime::Cx<S>, vali_runtime::ModuleId), String> {
    let identity = vali_compiler::Identity::new(&file.name);
    let program = vali_compiler::compile_program(&file.source, &identity, host)
        .map_err(|e| format!("compiling: {e}"))?;
    let mut cx = vali_runtime::Cx::<S>::new(&Default::default());
    cx.install_primitives()
        .map_err(|e| format!("under {}: {e}", S::NAME))?;
    install_runner(&mut cx, host)?;
    let id = vali_compiler::load_with(&mut cx, program, Arc::new(host.clone()))
        .map_err(|e| format!("under {}: {e}", S::NAME))?;
    if let Err(e) = cx.run(id) {
        // A failing suite ends the run with exit code 1 (see RUNNER) after
        // the runner has written what failed, the test's only report.
        let written = cx.take_output() + &cx.take_error_output();
        let written = written.trim_end();
        return Err(if written.is_empty() {
            format!("under {}: {e}", S::NAME)
        } else {
            format!("under {}: {e}\n{written}", S::NAME)
        });
    }
    // A suite that ends without reaching its final `test-end` exits 0 too,
    // so a `library-path` test must say so when it gets there.
    if host.libraries.is_some() && !cx.take_output().contains("summary: ") {
        return Err(format!(
            "under {}: the run ended without the suite's summary line",
            S::NAME
        ));
    }
    Ok((cx, id))
}

/// Compiles and runs `file` under every memory strategy, each in a fresh
/// isolate, stopping at the first that fails.
fn run_everywhere(file: &TestFile, host: &TestHost) -> Result<(), String> {
    use vali_runtime::{
        DeferredHeadered, DeferredSideCounts, NoCollection, PreciseHeadered, PreciseSideCounts,
    };
    run::<NoCollection>(file, host)?;
    run::<DeferredHeadered>(file, host)?;
    run::<DeferredSideCounts>(file, host)?;
    run::<PreciseHeadered>(file, host)?;
    run::<PreciseSideCounts>(file, host)?;
    Ok(())
}

/// How much longer compiling four times as much may take, at most: twice
/// what linear growth gives, where a quadratic pass gives sixteen.
const MAX_GROWTH: f64 = 8.0;

/// Below this, compile times are too short to compare.
const MIN_TIME: Duration = Duration::from_millis(30);

/// Runs a performance test: see the module documentation.
fn performance(file: &TestFile, host: &TestHost) -> Outcome {
    let (mut cx, id) = match run::<vali_runtime::NoCollection>(file, host) {
        Ok(ran) => ran,
        Err(e) => return Outcome::Failed(e),
    };
    let Some(program) = cx.variable(id, "program") else {
        return Outcome::Invalid(format!(
            "{}: a performance test defines (program n)",
            file.path.display()
        ));
    };
    // The best of three compiles of `(program n)`.
    let mut time = |n: u32| -> Result<Duration, String> {
        let n = vali_runtime::Handle::fixnum(i64::from(n)).expect("small");
        let source = cx
            .call(&program, &[n])
            .and_then(|datum| cx.write_string(&datum))
            .map_err(|e| format!("(program n): {e}"))?;
        let identity = vali_compiler::Identity::new(&format!("{}:generated", file.name));
        let mut best = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            vali_compiler::compile_program(&source, &identity, host)
                .map_err(|e| format!("compiling (program n): {e}"))?;
            best = best.min(start.elapsed());
        }
        Ok(best)
    };
    match growth(&mut time) {
        Ok(()) => Outcome::Passed,
        Err(e) => Outcome::Failed(e),
    }
}

/// Decides a performance test, given `time`, which compiles `(program n)`.
/// Only the time above `(program 0)`'s counts: most of that is compiling
/// the libraries the program imports, the same whatever `n` is. `n` grows
/// until its share is long enough to time, then `(program (* 4 n))`'s
/// share may be at most `MAX_GROWTH` times as long.
fn growth(mut time: impl FnMut(u32) -> Result<Duration, String>) -> Result<(), String> {
    let base = time(0)?;
    let mut n = 25;
    let mut small = time(n)?.saturating_sub(base);
    while small < MIN_TIME {
        if n >= 1 << 16 {
            return Err(format!(
                "(program {n}) takes no longer to compile than (program 0): nothing to compare"
            ));
        }
        n *= 2;
        small = time(n)?.saturating_sub(base);
    }
    let large = time(4 * n)?.saturating_sub(base);
    let growth = large.as_secs_f64() / small.as_secs_f64();
    if growth <= MAX_GROWTH {
        Ok(())
    } else {
        Err(format!(
            "compiling (program {}) took {large:?} more than (program 0), {growth:.1} times \
             (program {n})'s {small:?}; linear growth gives about 4",
            4 * n
        ))
    }
}

/// The host tests run in: besides the standard libraries, it grants the
/// process-context capability, by serving its own (scheme process-context)
/// over (vali-test process), a native library reading the process (ADR
/// 0009 #6). The command line is `vali-test` and the test's name.
#[derive(Clone)]
struct TestHost {
    process: Arc<vali_runtime::NativeLibrary>,
    /// Whether it serves `(regression virtual)`, from memory.
    in_memory: bool,
    /// The test's own directory, which its includes are relative to.
    dir: PathBuf,
    /// The directory expansion may read, if any.
    readable: Option<PathBuf>,
    /// A directory of `.sld` files to serve libraries from, if any.
    libraries: Option<PathBuf>,
    /// The capabilities the test asked for; none by default.
    grants: Vec<Grant>,
    /// `(vali-test clock)`, behind `(scheme time)`.
    clock: Arc<vali_runtime::NativeLibrary>,
    /// `(vali-test file)`, behind `(scheme file)`.
    files: Arc<vali_runtime::NativeLibrary>,
}

/// The test host's (scheme time), when it grants the clock.
const SCHEME_TIME: &str = "
(define-library (scheme time)
  (import (scheme base) (vali-test clock))
  (export current-second current-jiffy jiffies-per-second)
  (begin
    (define (jiffies-per-second) 1000000)))";

/// The test host's (scheme file), when it grants reading. Writing and
/// deleting are never granted: those procedures raise file errors.
const SCHEME_FILE: &str = "
(define-library (scheme file)
  (import (scheme base) (vali-test file))
  (export call-with-input-file call-with-output-file delete-file file-exists?
          open-binary-input-file open-binary-output-file open-input-file
          open-output-file with-input-from-file with-output-to-file)
  (begin
    (define (call-with-port-closing port proc)
      (call-with-values (lambda () (proc port))
        (lambda results (close-port port) (apply values results))))
    (define (call-with-input-file name proc)
      (call-with-port-closing (open-input-file name) proc))
    (define (call-with-output-file name proc)
      (call-with-port-closing (open-output-file name) proc))
    (define (with-input-from-file name thunk)
      (call-with-port-closing (open-input-file name)
        (lambda (port) (parameterize ((current-input-port port)) (thunk)))))
    (define (with-output-to-file name thunk)
      (call-with-port-closing (open-output-file name)
        (lambda (port) (parameterize ((current-output-port port)) (thunk)))))))";

/// A file error, for `op` on `name`.
fn file_error(op: &str, name: &str, why: impl std::fmt::Display) -> vali_runtime::Error {
    vali_runtime::Error::file_error(format!("{op}: {name}: {why}"))
}

/// `name` as the test's file capability reaches it: relative to `dir` (or
/// absolute), resolved, and refused unless it is `dir` or below it. `..`
/// is refused outright, and links are followed before the check.
fn scoped(dir: &Path, op: &str, name: &str) -> Result<PathBuf, vali_runtime::Error> {
    let outside = || file_error(op, name, "outside the test's directory");
    let root = dir.canonicalize().map_err(|e| file_error(op, name, e))?;
    let path = Path::new(name);
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(outside());
    }
    let joined = root.join(path);
    // A file that does not exist is checked by where it would be.
    let resolved = match joined.canonicalize() {
        Ok(resolved) => resolved,
        Err(_) => match (joined.parent().map(Path::canonicalize), joined.file_name()) {
            (Some(Ok(parent)), Some(file)) => parent.join(file),
            _ => return Err(file_error(op, name, "no such directory")),
        },
    };
    if resolved.starts_with(&root) {
        Ok(resolved)
    } else {
        Err(outside())
    }
}

/// What the `in-memory-loader` fixture serves, with no file behind it.
const VIRTUAL: &str = "
(define-library (regression virtual)
  (import (scheme base))
  (export where-was-i-loaded-from)
  (begin (define (where-was-i-loaded-from) 'served-from-memory)))";

/// The test host's (scheme process-context).
const PROCESS_CONTEXT: &str = "
(define-library (scheme process-context)
  (import (scheme base) (vali exit) (prefix (vali-test process) %))
  (export command-line get-environment-variable get-environment-variables
          exit emergency-exit)
  (begin
    (define (command-line) (%command-line))
    (define (get-environment-variable name) (%get-environment-variable name))
    (define (get-environment-variables)
      (map (lambda (p) (cons (car p) (cadr p))) (%get-environment-variables)))))";

impl TestHost {
    fn new(file: &TestFile) -> TestHost {
        let command_line = vec!["vali-test".to_owned(), file.name.clone()];
        let process = vali_runtime::NativeLibrary::new(&["vali-test", "process"])
            .function("command-line", move || command_line.clone())
            .function("get-environment-variable", |name: String| {
                std::env::var(name).ok()
            })
            .function("get-environment-variables", || {
                std::env::vars()
                    .map(|(name, value)| vec![name, value])
                    .collect::<Vec<_>>()
            });
        let dir = file
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let clock = vali_runtime::NativeLibrary::new(&["vali-test", "clock"])
            .function("current-second", || -> f64 {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64())
            })
            .function("current-jiffy", || -> i64 {
                static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
                START
                    .get_or_init(Instant::now)
                    .elapsed()
                    .as_micros()
                    .try_into()
                    .unwrap_or(i64::MAX)
            });
        let files = {
            use vali_runtime::Error;
            use vali_runtime::ports::Port;
            let (text_dir, bytes_dir, exists_dir) = (dir.clone(), dir.clone(), dir.clone());
            let no_writing = |op: &'static str| {
                move |name: String| -> Result<Port, Error> {
                    Err(file_error(
                        op,
                        &name,
                        "needs the write capability, which this test was not given",
                    ))
                }
            };
            vali_runtime::NativeLibrary::new(&["vali-test", "file"])
                .function(
                    "open-input-file",
                    move |name: String| -> Result<Port, Error> {
                        let op = "open-input-file";
                        let text = std::fs::read(scoped(&text_dir, op, &name)?)
                            .map_err(|e| file_error(op, &name, e))?;
                        String::from_utf8(text)
                            .map(Port::input_string)
                            .map_err(|_| file_error(op, &name, "not valid UTF-8 text"))
                    },
                )
                .function(
                    "open-binary-input-file",
                    move |name: String| -> Result<Port, Error> {
                        let op = "open-binary-input-file";
                        std::fs::read(scoped(&bytes_dir, op, &name)?)
                            .map(Port::input_bytes)
                            .map_err(|e| file_error(op, &name, e))
                    },
                )
                .function("file-exists?", move |name: String| -> Result<bool, Error> {
                    Ok(scoped(&exists_dir, "file-exists?", &name)?.exists())
                })
                .function("open-output-file", no_writing("open-output-file"))
                .function(
                    "open-binary-output-file",
                    no_writing("open-binary-output-file"),
                )
                .function("delete-file", |name: String| -> Result<(), Error> {
                    Err(file_error(
                        "delete-file",
                        &name,
                        "needs the write capability, which this test was not given",
                    ))
                })
        };
        TestHost {
            process: Arc::new(process),
            in_memory: false,
            dir,
            readable: None,
            libraries: None,
            grants: Vec::new(),
            clock: Arc::new(clock),
            files: Arc::new(files),
        }
    }
}

impl vali_compiler::Loader for TestHost {
    fn load(
        &self,
        name: &vali_compiler::LibraryName,
        _importer: Option<&vali_compiler::Identity>,
    ) -> Result<Option<vali_compiler::Source>, vali_compiler::LoadError> {
        use vali_compiler::{Identity, LibraryName, Source};
        Ok(
            if *name == LibraryName::new(["scheme", "process-context"]) {
                Some(Source::scheme(
                    Identity::new("vali-test:scheme/process-context"),
                    PROCESS_CONTEXT,
                ))
            } else if *name == LibraryName::new(["vali-test", "process"]) {
                Some(Source::native(
                    Identity::new("vali-test:process"),
                    Arc::clone(&self.process),
                ))
            } else if self.grants.contains(&Grant::Clock)
                && *name == LibraryName::new(["scheme", "time"])
            {
                Some(Source::scheme(
                    Identity::new("vali-test:scheme/time"),
                    SCHEME_TIME,
                ))
            } else if self.grants.contains(&Grant::Clock)
                && *name == LibraryName::new(["vali-test", "clock"])
            {
                Some(Source::native(
                    Identity::new("vali-test:clock"),
                    Arc::clone(&self.clock),
                ))
            } else if self.grants.contains(&Grant::Read)
                && *name == LibraryName::new(["scheme", "file"])
            {
                Some(Source::scheme(
                    Identity::new("vali-test:scheme/file"),
                    SCHEME_FILE,
                ))
            } else if self.grants.contains(&Grant::Read)
                && *name == LibraryName::new(["vali-test", "file"])
            {
                Some(Source::native(
                    Identity::new("vali-test:file"),
                    Arc::clone(&self.files),
                ))
            } else if self.in_memory && *name == LibraryName::new(["regression", "virtual"]) {
                Some(Source::scheme(
                    Identity::new("memory:regression/virtual"),
                    VIRTUAL,
                ))
            } else if let Some(libraries) = &self.libraries {
                library_path_source(libraries, name)
            } else {
                None
            },
        )
    }

    /// Files under the readable directory, if there is one, relative to the
    /// test's own directory; nothing else, `..` and links included, since
    /// paths are compared once resolved.
    fn include(
        &self,
        path: &str,
        _from: &vali_compiler::Identity,
    ) -> Result<Option<String>, vali_compiler::LoadError> {
        let Some(readable) = self.readable.as_ref().and_then(|d| d.canonicalize().ok()) else {
            return Ok(None);
        };
        let Ok(resolved) = self.dir.join(path).canonicalize() else {
            return Ok(None);
        };
        if !resolved.starts_with(&readable) {
            return Ok(None);
        }
        std::fs::read_to_string(&resolved)
            .map(Some)
            .map_err(|e| vali_compiler::LoadError {
                message: e.to_string(),
            })
    }
}

/// The source of `name` in the `library-path` fixture's directory, as
/// `a/b.sld` for `(a b)`. Only plain names are served, so a name can't
/// climb out of the directory.
fn library_path_source(
    libraries: &Path,
    name: &vali_compiler::LibraryName,
) -> Option<vali_compiler::Source> {
    let mut path = libraries.to_path_buf();
    for part in &name.0 {
        match part {
            vali_compiler::NamePart::Symbol(s)
                if s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') =>
            {
                path.push(s);
            }
            _ => return None,
        }
    }
    path.set_extension("sld");
    let text = std::fs::read_to_string(&path).ok()?;
    Some(vali_compiler::Source::scheme(
        vali_compiler::Identity::new(&format!("library-path:{}", path.display())),
        text,
    ))
}

/// The 1-based line and column of `offset` in `source`, counting columns in
/// characters.
fn line_column(source: &str, offset: TextSize) -> (usize, usize) {
    let before = &source[..usize::from(offset)];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line, column)
}

/// Every `.scm` file directly in `dir`, sorted by name.
pub fn discover(dir: &Path) -> std::io::Result<Vec<TestFile>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "scm") {
            let name = path
                .file_stem()
                .expect("a .scm file has a stem")
                .to_string_lossy()
                .into_owned();
            let source = std::fs::read_to_string(&path)?;
            files.push(TestFile { name, path, source });
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}

/// Decides whether a test passed, given its header and what running it
/// produced.
pub fn verdict(header: &Header, outcome: Outcome) -> Result<(), String> {
    match (&header.expect_fail, outcome) {
        (_, Outcome::Invalid(message)) => Err(message),
        (Some(_), Outcome::Passed) => Err("now passes: remove its `expect: fail` line".to_owned()),
        (Some(_), Outcome::Failed(_) | Outcome::Unsupported(_)) => Ok(()),
        (None, Outcome::Passed) => Ok(()),
        (None, Outcome::Failed(message)) => Err(message),
        (None, Outcome::Unsupported(reason)) => Err(format!(
            "this backend cannot run it ({reason}); mark it `expect: fail` with a reason"
        )),
    }
}

/// One `cargo test` test per file, run through `backend`. Tests expected to
/// fail are labelled `xfail`.
pub fn trials(files: Vec<TestFile>, backend: Arc<dyn Backend + Send + Sync>) -> Vec<Trial> {
    // Vali's own libraries load prebuilt, as they do for every host.
    vali_stdlib::install();
    files
        .into_iter()
        .map(|file| {
            let header = parse_header(&file.source);
            let expected_to_fail = header.as_ref().is_ok_and(|h| h.expect_fail.is_some());
            let backend = Arc::clone(&backend);
            let name = file.name.clone();
            let trial = Trial::test(name, move || {
                let header =
                    header.map_err(|e| Failed::from(format!("{}: {e}", file.path.display())))?;
                let outcome = backend.run(&file, &header);
                verdict(&header, outcome).map_err(Failed::from)
            });
            if expected_to_fail {
                trial.with_kind("xfail")
            } else {
                trial
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(expect_fail: Option<&str>) -> Header {
        Header {
            requirement: "r".to_owned(),
            found_in: None,
            kind: Kind::Scheme,
            host: None,
            grants: Vec::new(),
            expect_fail: expect_fail.map(str::to_owned),
        }
    }

    #[test]
    fn expected_failures() {
        let xfail = header(Some("until M1"));
        assert_eq!(verdict(&xfail, Outcome::Failed("x".into())), Ok(()));
        assert_eq!(verdict(&xfail, Outcome::Unsupported("x".into())), Ok(()));
        assert!(
            verdict(&xfail, Outcome::Passed)
                .unwrap_err()
                .contains("now passes")
        );
        assert!(verdict(&xfail, Outcome::Invalid("broken".into())).is_err());
    }

    #[test]
    fn compile_time_growth() {
        let base = Duration::from_millis(40);
        // Linear: passes.
        assert_eq!(
            growth(|n| Ok(base + Duration::from_micros(500) * n)),
            Ok(())
        );
        // Quadratic: fails, naming the sizes.
        let quadratic = growth(|n| Ok(base + Duration::from_micros(u64::from(n * n))));
        assert!(
            quadratic
                .as_ref()
                .is_err_and(|e| e.contains("linear growth gives about 4")),
            "{quadratic:?}"
        );
        // Flat: nothing to compare.
        assert!(growth(|_| Ok(base)).is_err_and(|e| e.contains("nothing to compare")));
    }

    #[test]
    fn ordinary_tests() {
        let plain = header(None);
        assert_eq!(verdict(&plain, Outcome::Passed), Ok(()));
        assert!(verdict(&plain, Outcome::Failed("x".into())).is_err());
        assert!(
            verdict(&plain, Outcome::Unsupported("x".into()))
                .unwrap_err()
                .contains("expect: fail")
        );
    }

    #[test]
    fn read_only_backend() {
        let file = |source: &str| TestFile {
            name: "t".into(),
            path: "t.scm".into(),
            source: source.into(),
        };
        let h = header(Some(""));
        assert!(matches!(
            ReadOnly.run(&file("(display 1)"), &h),
            Outcome::Unsupported(_)
        ));
        match ReadOnly.run(&file("(display 1)\n  (oops"), &h) {
            Outcome::Invalid(message) => assert!(message.starts_with("t.scm:2:3:"), "{message}"),
            other => panic!("expected invalid, got {other:?}"),
        }
    }
}
