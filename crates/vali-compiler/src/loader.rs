//! Finding libraries (ADR 0009 #1 to #3).
//!
//! A host supplies a [`Loader`] or an [`AsyncLoader`]: given a library's
//! name and the library importing it, it returns the library's source with
//! a canonical [`Identity`], or nothing. Compiling a program first fetches
//! its whole import graph, awaiting the loader as it goes, then expands and
//! compiles with everything in memory, so the expander never awaits.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use vali_dist::{Digest, Hasher};
use vali_syntax::{Datum, DatumKind, Span};

use crate::CompileError;
use crate::library::{self, ImportSet, LibraryForm, LibraryName};

mod graph;

pub(crate) use graph::*;

type Result<T> = std::result::Result<T, CompileError>;

/// A library's canonical identity: what Vali caches and reports it by.
/// It must not depend on the machine: a path relative to a root, say,
/// never an absolute one (ADR 0009 #2 and #11).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Identity(Arc<str>);

impl Identity {
    /// The identity `text`.
    pub fn new(text: &str) -> Identity {
        Identity(text.into())
    }

    /// Its text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A library as a loader serves it: Scheme source, or a native library of
/// Rust functions (ADR 0016).
#[derive(Clone, Debug)]
pub struct Source {
    /// The library's identity.
    pub identity: Identity,
    /// Its text, holding one library form; empty for a native library.
    pub text: String,
    /// The native library, if it is one.
    pub native: Option<Arc<vali_runtime::NativeLibrary>>,
}

impl Source {
    /// A library's Scheme source.
    pub fn scheme(identity: Identity, text: impl Into<String>) -> Source {
        Source {
            identity,
            text: text.into(),
            native: None,
        }
    }

    /// A native library. The compiled program carries it, and loading the
    /// program into an isolate binds its functions there.
    pub fn native(identity: Identity, library: Arc<vali_runtime::NativeLibrary>) -> Source {
        Source {
            identity,
            text: String::new(),
            native: Some(library),
        }
    }
}

/// A native library's form: its name and what it exports, its functions.
/// It has no body: the expander makes a module whose variables link to the
/// functions as it loads (ADR 0016 #8).
fn native_form(library: &vali_runtime::NativeLibrary) -> String {
    let symbol = |s: &str| format!("|{}|", s.replace('\\', "\\\\").replace('|', "\\|"));
    let name: Vec<String> = library
        .name()
        .iter()
        .map(|part| match part.parse::<u64>() {
            Ok(_) => part.clone(),
            Err(_) => symbol(part),
        })
        .collect();
    let exports: Vec<String> = library
        .functions()
        .iter()
        .map(|f| symbol(f.name()))
        .collect();
    format!(
        "(define-library ({}) (export {}))",
        name.join(" "),
        exports.join(" ")
    )
}

/// Why a loader could not load a library: one it has, but cannot read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadError {
    /// What went wrong.
    pub message: String,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LoadError {}

/// Finds libraries, synchronously.
pub trait Loader {
    /// The source of the library `name`, which `importer` imports (`None`
    /// for a program), or `None` if this loader has no such library.
    fn load(
        &self,
        name: &LibraryName,
        importer: Option<&Identity>,
    ) -> std::result::Result<Option<Source>, LoadError>;

    /// The text of the file at `path`, which the library or program
    /// `from` includes (R7RS 4.1.7 and 5.6.1), or `None` if `from` may not
    /// read it. A relative path is relative to `from` (ADR 0009 #12); what
    /// each identity may read is the host's to decide, and by default
    /// nothing may be (ADR 0009 #8).
    fn include(
        &self,
        path: &str,
        from: &Identity,
    ) -> std::result::Result<Option<String>, LoadError> {
        let _ = (path, from);
        Ok(None)
    }
}

/// Finds libraries, asynchronously: the same shape as [`Loader`], with the
/// result as a future that can run on a multi-threaded executor. Every
/// `Loader` that is `Sync` is one.
pub trait AsyncLoader {
    /// As [`Loader::load`].
    fn load(
        &self,
        name: &LibraryName,
        importer: Option<&Identity>,
    ) -> impl Future<Output = std::result::Result<Option<Source>, LoadError>> + Send;

    /// As [`Loader::include`].
    fn include(
        &self,
        path: &str,
        from: &Identity,
    ) -> impl Future<Output = std::result::Result<Option<String>, LoadError>> + Send {
        let _ = (path, from);
        std::future::ready(Ok(None))
    }
}

impl<L: Loader + Sync> AsyncLoader for L {
    fn load(
        &self,
        name: &LibraryName,
        importer: Option<&Identity>,
    ) -> impl Future<Output = std::result::Result<Option<Source>, LoadError>> + Send {
        std::future::ready(Loader::load(self, name, importer))
    }

    fn include(
        &self,
        path: &str,
        from: &Identity,
    ) -> impl Future<Output = std::result::Result<Option<String>, LoadError>> + Send {
        std::future::ready(Loader::include(self, path, from))
    }
}

/// What fetching needs from a loader of either kind.
trait Fetch {
    fn fetch(
        &self,
        name: &LibraryName,
        importer: Option<&Identity>,
    ) -> impl Future<Output = std::result::Result<Option<Source>, LoadError>>;

    fn include(
        &self,
        path: &str,
        from: &Identity,
    ) -> impl Future<Output = std::result::Result<Option<String>, LoadError>>;
}

/// A synchronous loader, which needn't be `Sync`.
struct Sync_<'a, L>(&'a L);

impl<L: Loader> Fetch for Sync_<'_, L> {
    fn fetch(
        &self,
        name: &LibraryName,
        importer: Option<&Identity>,
    ) -> impl Future<Output = std::result::Result<Option<Source>, LoadError>> {
        std::future::ready(self.0.load(name, importer))
    }

    fn include(
        &self,
        path: &str,
        from: &Identity,
    ) -> impl Future<Output = std::result::Result<Option<String>, LoadError>> {
        std::future::ready(self.0.include(path, from))
    }
}

struct Async<'a, L>(&'a L);

impl<L: AsyncLoader> Fetch for Async<'_, L> {
    fn fetch(
        &self,
        name: &LibraryName,
        importer: Option<&Identity>,
    ) -> impl Future<Output = std::result::Result<Option<Source>, LoadError>> {
        AsyncLoader::load(self.0, name, importer)
    }

    fn include(
        &self,
        path: &str,
        from: &Identity,
    ) -> impl Future<Output = std::result::Result<Option<String>, LoadError>> {
        AsyncLoader::include(self.0, path, from)
    }
}

/// The standard libraries' sources, embedded in the binary (ADR 0007 #11).
const STANDARD: &[(&[&str], &str)] = &[
    (&["scheme", "base"], include_str!("../lib/scheme/base.sld")),
    (
        &["scheme", "case-lambda"],
        include_str!("../lib/scheme/case-lambda.sld"),
    ),
    (&["scheme", "char"], include_str!("../lib/scheme/char.sld")),
    (
        &["scheme", "complex"],
        include_str!("../lib/scheme/complex.sld"),
    ),
    (&["scheme", "cxr"], include_str!("../lib/scheme/cxr.sld")),
    (
        &["scheme", "inexact"],
        include_str!("../lib/scheme/inexact.sld"),
    ),
    (&["scheme", "eval"], include_str!("../lib/scheme/eval.sld")),
    (&["scheme", "file"], include_str!("../lib/scheme/file.sld")),
    (&["scheme", "lazy"], include_str!("../lib/scheme/lazy.sld")),
    (&["scheme", "load"], include_str!("../lib/scheme/load.sld")),
    (
        &["scheme", "process-context"],
        include_str!("../lib/scheme/process-context.sld"),
    ),
    (&["scheme", "r5rs"], include_str!("../lib/scheme/r5rs.sld")),
    (&["scheme", "read"], include_str!("../lib/scheme/read.sld")),
    (&["scheme", "repl"], include_str!("../lib/scheme/repl.sld")),
    (&["scheme", "time"], include_str!("../lib/scheme/time.sld")),
    (&["vali", "exit"], include_str!("../lib/vali/exit.sld")),
    (
        &["vali", "pipeline"],
        include_str!("../lib/vali/pipeline.sld"),
    ),
    (&["vali", "syntax"], include_str!("../lib/vali/syntax.sld")),
    (
        &["scheme", "write"],
        include_str!("../lib/scheme/write.sld"),
    ),
    (&["srfi", "0"], include_str!("../lib/srfi/0.sld")),
    (&["srfi", "1"], include_str!("../lib/srfi/1.sld")),
    (&["srfi", "2"], include_str!("../lib/srfi/2.sld")),
    (&["srfi", "6"], include_str!("../lib/srfi/6.sld")),
    (&["srfi", "8"], include_str!("../lib/srfi/8.sld")),
    (&["srfi", "9"], include_str!("../lib/srfi/9.sld")),
    (&["srfi", "11"], include_str!("../lib/srfi/11.sld")),
    (&["srfi", "16"], include_str!("../lib/srfi/16.sld")),
    (&["srfi", "23"], include_str!("../lib/srfi/23.sld")),
    (&["srfi", "26"], include_str!("../lib/srfi/26.sld")),
    (&["srfi", "39"], include_str!("../lib/srfi/39.sld")),
    (&["srfi", "64"], include_str!("../lib/srfi/64.sld")),
    (&["srfi", "69"], include_str!("../lib/srfi/69.sld")),
    (&["srfi", "87"], include_str!("../lib/srfi/87.sld")),
    (&["srfi", "88"], include_str!("../lib/srfi/88.sld")),
    (&["srfi", "111"], include_str!("../lib/srfi/111.sld")),
    (&["srfi", "116"], include_str!("../lib/srfi/116.sld")),
    (&["srfi", "117"], include_str!("../lib/srfi/117.sld")),
    (&["srfi", "125"], include_str!("../lib/srfi/125.sld")),
    (&["srfi", "127"], include_str!("../lib/srfi/127.sld")),
    (&["srfi", "128"], include_str!("../lib/srfi/128.sld")),
    (&["srfi", "132"], include_str!("../lib/srfi/132.sld")),
    (&["srfi", "133"], include_str!("../lib/srfi/133.sld")),
    (&["srfi", "141"], include_str!("../lib/srfi/141.sld")),
    (&["srfi", "145"], include_str!("../lib/srfi/145.sld")),
    (&["srfi", "197"], include_str!("../lib/srfi/197.sld")),
    (&["srfi", "227"], include_str!("../lib/srfi/227.sld")),
    (
        &["srfi", "227", "definitions"],
        include_str!("../lib/srfi/227/definitions.sld"),
    ),
];

/// The names of Vali's own libraries written in Scheme.
pub(crate) fn standard_names() -> Vec<LibraryName> {
    STANDARD
        .iter()
        .map(|(parts, _)| LibraryName::new(parts.iter().copied()))
        .collect()
}

/// The `srfi-N` feature identifiers of the SRFIs Vali ships: one for each
/// number `N` that some `(srfi N ...)` library in [`STANDARD`] has, so the
/// features cannot drift from the libraries.
pub(crate) fn srfi_features() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (parts, _) in STANDARD {
        if let [first, number, ..] = parts
            && *first == "srfi"
        {
            let feature = format!("srfi-{number}");
            if !out.contains(&feature) {
                out.push(feature);
            }
        }
    }
    out
}

/// Whether `name` is one of Vali's own libraries.
pub(crate) fn is_standard(name: &LibraryName) -> bool {
    library::is_builtin(name)
        || *name == LibraryName::new(["vali", "ports"])
        || STANDARD
            .iter()
            .any(|(parts, _)| *name == LibraryName::new(parts.iter().copied()))
}

/// The libraries that reach outside the program, which a host serves
/// itself to grant what they need: a host's loader is asked for these
/// first, and Vali's own versions, which have nothing, serve otherwise
/// (ADR 0009 #6).
const HOST_SERVED: &[&[&str]] = &[
    &["scheme", "file"],
    &["scheme", "process-context"],
    &["scheme", "time"],
];

/// Whether a host's loader may serve `name` in place of Vali's own.
fn is_host_served(name: &LibraryName) -> bool {
    HOST_SERVED
        .iter()
        .any(|parts| *name == LibraryName::new(parts.iter().copied()))
}

/// The source of the standard library `name`, if it is one: Scheme
/// source, or one of Vali's own native libraries.
fn standard(name: &LibraryName) -> Option<Source> {
    if *name == LibraryName::new(["vali", "ports"]) {
        return Some(Source::native(
            Identity::new("vali:vali/ports"),
            vali_runtime::ports::library(),
        ));
    }
    let (parts, text) = STANDARD
        .iter()
        .find(|(parts, _)| *name == LibraryName::new(parts.iter().copied()))?;
    Some(Source::scheme(
        Identity::new(&format!("vali:{}", parts.join("/"))),
        *text,
    ))
}

/// The name of the standard library whose identity is `identity`, if it
/// is one.
fn standard_name(identity: &str) -> Option<LibraryName> {
    if identity == "vali:vali/ports" {
        return Some(LibraryName::new(["vali", "ports"]));
    }
    let path = identity.strip_prefix("vali:")?;
    STANDARD
        .iter()
        .find(|(parts, _)| path.split('/').eq(parts.iter().copied()))
        .map(|(parts, _)| LibraryName::new(parts.iter().copied()))
}

/// What the standard library `identity` imports, by name, read from its
/// prebuilt artifact rather than its source, if it has one that this build
/// of Vali can load and that names only standard libraries.
fn prebuilt_imports(identity: &Identity) -> Option<Vec<LibraryName>> {
    crate::prebuilt::depends(identity.as_str())?
        .iter()
        .map(|dependency| standard_name(dependency))
        .collect()
}

/// The form of the standard library `name`, read from its source, for one
/// whose prebuilt artifact was meant to be loaded but did not fit. Vali's
/// own libraries include no files and test no libraries in `cond-expand`,
/// so no loader is needed.
pub(crate) fn standard_form(
    name: &LibraryName,
    sources: &mut vali_syntax::Sources,
) -> Result<LibraryForm> {
    let source = standard(name).ok_or_else(|| error(format!("{name} is not standard"), None))?;
    let local = HashSet::new();
    let future = pin!(read_library(
        name,
        &source,
        &Sync_(&crate::NoLibraries),
        &local,
        sources,
    ));
    match future.poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(result) => result.map(|(form, _)| form),
        Poll::Pending => unreachable!("a synchronous loader never waits"),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// Serves libraries from a map of names to texts.
    struct Map(Vec<(&'static str, &'static str)>);

    impl Loader for Map {
        fn load(
            &self,
            name: &LibraryName,
            _importer: Option<&Identity>,
        ) -> std::result::Result<Option<Source>, LoadError> {
            let wanted = name.to_string();
            Ok(self
                .0
                .iter()
                .find(|(n, _)| *n == wanted)
                .map(|(n, text)| Source::scheme(Identity::new(&format!("test:{n}")), *text)))
        }
    }

    fn read(text: &str) -> Program {
        program(&vali_syntax::read(text, vali_syntax::SourceId(0)).datums).unwrap()
    }

    fn names(graph: &Graph) -> Vec<String> {
        graph.libraries.iter().map(|l| l.name.to_string()).collect()
    }

    fn libraries() -> Map {
        Map(vec![
            (
                "(a)",
                "(define-library (a) (import (b) (c)) (export x) (begin (define x 1)))",
            ),
            (
                "(b)",
                "(define-library (b) (import (c)) (export y) (begin (define y 2)))",
            ),
            (
                "(c)",
                "(define-library (c) (export z) (begin (define z 3)))",
            ),
            (
                "(loop one)",
                "(define-library (loop one) (import (loop two)))",
            ),
            (
                "(loop two)",
                "(define-library (loop two) (import (loop one)))",
            ),
            ("(misnamed)", "(define-library (other))"),
        ])
    }

    #[test]
    fn libraries_come_after_their_imports() {
        let program = read("(import (only (a) x) (prefix (b) b:)) x");
        let graph = fetch(
            &program,
            &Identity::new("main"),
            &libraries(),
            &mut vali_syntax::Sources::new(),
        )
        .unwrap();
        assert_eq!(names(&graph), ["(c)", "(b)", "(a)"]);
        assert_eq!(program.body.len(), 1);
    }

    #[test]
    fn a_program_can_define_its_libraries() {
        let program = read(
            "(define-library (mine) (import (c)) (export w) (begin (define w z)))
             (import (mine))
             w",
        );
        let graph = fetch(
            &program,
            &Identity::new("main"),
            &libraries(),
            &mut vali_syntax::Sources::new(),
        )
        .unwrap();
        assert_eq!(names(&graph), ["(c)", "(mine)"]);
        assert_eq!(graph.libraries[1].identity.as_str(), "main#(mine)");
    }

    #[test]
    fn fetching_reports_what_is_wrong() {
        let message = |text: &str| {
            fetch(
                &read(text),
                &Identity::new("main"),
                &libraries(),
                &mut vali_syntax::Sources::new(),
            )
            .unwrap_err()
            .message
        };
        assert_eq!(
            message("(import (nowhere))"),
            "library (nowhere) not found, imported by the program"
        );
        assert_eq!(
            message("(import (loop one))"),
            "libraries import each other: (loop one) -> (loop two) -> (loop one)"
        );
        assert!(
            message("(import (misnamed))")
                .contains("defines (other), but was loaded as (misnamed)")
        );
    }

    /// An asynchronous loader whose every load waits once before it is
    /// ready, as a network fetch would.
    struct Slow(Map);

    impl AsyncLoader for Slow {
        fn load(
            &self,
            name: &LibraryName,
            importer: Option<&Identity>,
        ) -> impl Future<Output = std::result::Result<Option<Source>, LoadError>> + Send {
            let result = Loader::load(&self.0, name, importer);
            let mut waited = false;
            std::future::poll_fn(move |cx| {
                if waited {
                    Poll::Ready(result.clone())
                } else {
                    waited = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
        }
    }

    /// Runs `future` to completion on this thread.
    fn block_on<T>(future: impl Future<Output = T>) -> (T, usize) {
        let mut future = pin!(future);
        let polls = Cell::new(0);
        loop {
            polls.set(polls.get() + 1);
            if let Poll::Ready(value) = future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
            {
                return (value, polls.get());
            }
        }
    }

    #[test]
    fn an_async_loader_is_awaited() {
        let program = read("(import (a))");
        let loader = Slow(libraries());
        let (graph, polls) = block_on(fetch_async(
            &program,
            &Identity::new("main"),
            &loader,
            &mut vali_syntax::Sources::new(),
        ));
        assert_eq!(names(&graph.unwrap()), ["(c)", "(b)", "(a)"]);
        // One wait per library fetched.
        assert_eq!(polls, 4);
    }

    #[test]
    fn a_sync_loader_also_works_asynchronously() {
        let program = read("(import (a))");
        let (graph, polls) = block_on(fetch_async(
            &program,
            &Identity::new("main"),
            &libraries(),
            &mut vali_syntax::Sources::new(),
        ));
        assert_eq!(names(&graph.unwrap()), ["(c)", "(b)", "(a)"]);
        assert_eq!(polls, 1);
    }

    /// The prebuilt artifacts are compiled where they are built, and
    /// record no feature they tested (ADR 0001 #8), so none of Vali's own
    /// libraries may test one: its platform features could differ on the
    /// machine that loads the artifact.
    #[test]
    fn standard_libraries_test_no_features() {
        for (parts, source) in STANDARD {
            // (scheme base) only exports the forms.
            if parts == &["scheme", "base"] {
                continue;
            }
            assert!(
                !source.contains("(cond-expand") && !source.contains("(features)"),
                "{parts:?} tests features, which prebuilt artifacts do not record"
            );
        }
    }

    #[test]
    fn srfi_features_follow_the_libraries() {
        let features = srfi_features();
        for (parts, _) in STANDARD {
            if let ["srfi", n, ..] = parts {
                assert!(features.contains(&format!("srfi-{n}")), "srfi-{n}");
            }
        }
        // One per number, even where a SRFI has several libraries.
        let mut unique = features.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), features.len());
    }
}
