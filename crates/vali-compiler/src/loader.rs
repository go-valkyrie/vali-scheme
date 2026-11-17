//! Finding libraries (ADR 0009 #1 to #3).
//!
//! A host supplies a [`Loader`] or an [`AsyncLoader`]: given a library's
//! name and the library importing it, it returns the library's source with
//! a canonical [`Identity`], or nothing. Compiling a program first fetches
//! its whole import graph, awaiting the loader as it goes, then expands and
//! compiles with everything in memory, so the expander never awaits.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use vali_syntax::{Datum, SourceId, Span};

use crate::CompileError;
use crate::library::{self, ImportSet, LibraryForm, LibraryName};

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

/// A native library's form: a library whose variables are its functions,
/// as `%native-procedure` finds them in the isolate (ADR 0016 #8).
fn native_form(identity: &Identity, library: &vali_runtime::NativeLibrary) -> String {
    let symbol = |s: &str| format!("|{}|", s.replace('\\', "\\\\").replace('|', "\\|"));
    let string = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let name: Vec<String> = library
        .name()
        .iter()
        .map(|part| match part.parse::<u64>() {
            Ok(_) => part.clone(),
            Err(_) => symbol(part),
        })
        .collect();
    let functions = library.functions();
    let exports: Vec<String> = functions.iter().map(|f| symbol(f.name())).collect();
    let definitions: Vec<String> = functions
        .iter()
        .map(|f| {
            format!(
                "(define {} (%native-procedure {} {}))",
                symbol(f.name()),
                string(identity.as_str()),
                string(f.name())
            )
        })
        .collect();
    format!(
        "(define-library ({}) (import (vali core) (vali primitive)) (export {}) (begin {}))",
        name.join(" "),
        exports.join(" "),
        definitions.join(" ")
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
}

impl<L: Loader + Sync> AsyncLoader for L {
    fn load(
        &self,
        name: &LibraryName,
        importer: Option<&Identity>,
    ) -> impl Future<Output = std::result::Result<Option<Source>, LoadError>> + Send {
        std::future::ready(Loader::load(self, name, importer))
    }
}

/// What fetching needs from a loader of either kind.
trait Fetch {
    fn fetch(
        &self,
        name: &LibraryName,
        importer: Option<&Identity>,
    ) -> impl Future<Output = std::result::Result<Option<Source>, LoadError>>;
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
    (&["scheme", "lazy"], include_str!("../lib/scheme/lazy.sld")),
    (
        &["scheme", "write"],
        include_str!("../lib/scheme/write.sld"),
    ),
    (&["srfi", "64"], include_str!("../lib/srfi/64.sld")),
];

/// The source of the standard library `name`, if it is one.
fn standard(name: &LibraryName) -> Option<Source> {
    let (parts, text) = STANDARD
        .iter()
        .find(|(parts, _)| *name == LibraryName::new(parts.iter().copied()))?;
    Some(Source::scheme(
        Identity::new(&format!("vali:{}", parts.join("/"))),
        *text,
    ))
}

/// A library in the import graph.
#[derive(Clone, Debug)]
pub(crate) struct Library {
    pub(crate) identity: Identity,
    pub(crate) form: LibraryForm,
    /// The native library, if it is one.
    pub(crate) native: Option<Arc<vali_runtime::NativeLibrary>>,
}

/// A program, read: the libraries it defines itself, its imports, and its
/// body.
#[derive(Clone, Debug)]
pub(crate) struct Program {
    pub(crate) libraries: Vec<LibraryForm>,
    pub(crate) imports: Vec<ImportSet>,
    /// Whether the imports are the implicit `(scheme base)` of a program
    /// with no `import` form.
    pub(crate) implicit: bool,
    pub(crate) body: Vec<Datum>,
}

/// Reads a program's parts: leading library forms, then leading imports.
pub(crate) fn program(datums: &[Datum]) -> Result<Program> {
    let mut libraries = Vec::new();
    let mut rest = datums;
    while let Some((first, more)) = rest.split_first() {
        match library::library_form(first)? {
            Some(form) => {
                libraries.push(form);
                rest = more;
            }
            None => break,
        }
    }
    let (mut imports, body) = library::program_imports(rest)?;
    let implicit = imports.is_empty();
    if implicit {
        imports.push(ImportSet::Library(LibraryName::new(["scheme", "base"])));
    }
    Ok(Program {
        libraries,
        imports,
        implicit,
        body: body.to_vec(),
    })
}

/// Every library a program imports, directly or not, each after the
/// libraries it imports.
#[derive(Debug, Default)]
pub(crate) struct Graph {
    pub(crate) libraries: Vec<Library>,
    /// Each name, to its library's place in `libraries`. Two names with
    /// the same identity are the same library.
    pub(crate) by_name: HashMap<LibraryName, usize>,
}

fn error(message: impl Into<String>, span: Option<Span>) -> CompileError {
    CompileError {
        message: message.into(),
        span,
    }
}

/// Fetches the import graph of `program` with a synchronous loader.
pub(crate) fn fetch(program: &Program, identity: &Identity, loader: &impl Loader) -> Result<Graph> {
    let loader = Sync_(loader);
    let future = pin!(walk(program, identity, &loader));
    // A synchronous loader's futures are ready at once, so the walk never
    // waits.
    match future.poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(result) => result,
        Poll::Pending => unreachable!("a synchronous loader never waits"),
    }
}

/// Fetches the import graph of `program` with an asynchronous loader.
pub(crate) async fn fetch_async(
    program: &Program,
    identity: &Identity,
    loader: &impl AsyncLoader,
) -> Result<Graph> {
    let loader = Async(loader);
    walk(program, identity, &loader).await
}

/// Reads one library form, named `name`, from a loader's source.
fn read_library(name: &LibraryName, source: &Source) -> Result<LibraryForm> {
    let text = match &source.native {
        Some(library) => native_form(&source.identity, library),
        None => source.text.clone(),
    };
    let read = vali_syntax::read(&text, SourceId(0));
    if let Some(e) = read.errors.first() {
        return Err(error(
            format!("{}: read: {}", source.identity, e.kind),
            None,
        ));
    }
    let [datum] = &read.datums[..] else {
        return Err(error(
            format!("{}: expected exactly one library form", source.identity),
            None,
        ));
    };
    let form = library::library_form(datum)?.ok_or_else(|| {
        error(
            format!("{}: expected a library form", source.identity),
            Some(datum.span),
        )
    })?;
    if form.name != *name {
        return Err(error(
            format!(
                "{}: defines {}, but was loaded as {name}",
                source.identity, form.name
            ),
            Some(form.span),
        ));
    }
    Ok(form)
}

async fn walk(program: &Program, identity: &Identity, loader: &impl Fetch) -> Result<Graph> {
    // Libraries the program defines itself need no loader.
    let local: HashMap<&LibraryName, &LibraryForm> =
        program.libraries.iter().map(|l| (&l.name, l)).collect();
    // Found libraries, unordered, and each name's place among them.
    let mut found: Vec<Library> = Vec::new();
    let mut by_name: HashMap<LibraryName, usize> = HashMap::new();
    let mut by_identity: HashMap<Identity, usize> = HashMap::new();
    let mut work: Vec<(LibraryName, Option<Identity>)> = program
        .imports
        .iter()
        .map(|set| (set.library().clone(), None))
        .collect();
    work.reverse();
    while let Some((name, importer)) = work.pop() {
        if by_name.contains_key(&name) || library::is_builtin(&name) {
            continue;
        }
        // Standard libraries come first, so no loader can replace them.
        let (library_identity, form, native) = match local.get(&name) {
            Some(form) => (
                Identity::new(&format!("{identity}#{name}")),
                (*form).clone(),
                None,
            ),
            None => {
                let source = match standard(&name) {
                    Some(source) => Some(source),
                    None => loader
                        .fetch(&name, importer.as_ref())
                        .await
                        .map_err(|e| error(format!("{name}: {e}"), None))?,
                };
                let source = source.ok_or_else(|| {
                    let by = importer
                        .as_ref()
                        .map_or_else(|| "the program".to_owned(), |i| i.to_string());
                    error(format!("library {name} not found, imported by {by}"), None)
                })?;
                if let Some(&index) = by_identity.get(&source.identity) {
                    by_name.insert(name, index);
                    continue;
                }
                let form = read_library(&name, &source)?;
                (source.identity, form, source.native)
            }
        };
        for set in form.imports.iter().rev() {
            work.push((set.library().clone(), Some(library_identity.clone())));
        }
        let index = found.len();
        by_identity.insert(library_identity.clone(), index);
        by_name.insert(name, index);
        found.push(Library {
            identity: library_identity,
            form,
            native,
        });
    }
    order(found, by_name)
}

/// Orders libraries so each comes after those it imports, refusing cycles.
fn order(found: Vec<Library>, by_name: HashMap<LibraryName, usize>) -> Result<Graph> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        New,
        Open,
        Done,
    }
    fn visit(
        i: usize,
        found: &[Library],
        by_name: &HashMap<LibraryName, usize>,
        state: &mut [State],
        path: &mut Vec<usize>,
        order: &mut Vec<usize>,
    ) -> Result<()> {
        match state[i] {
            State::Done => return Ok(()),
            State::Open => {
                let from = path.iter().position(|&p| p == i).expect("on the path");
                let cycle: Vec<String> = path[from..]
                    .iter()
                    .chain([&i])
                    .map(|&p| found[p].form.name.to_string())
                    .collect();
                return Err(error(
                    format!("libraries import each other: {}", cycle.join(" -> ")),
                    Some(found[i].form.span),
                ));
            }
            State::New => {}
        }
        state[i] = State::Open;
        path.push(i);
        for set in &found[i].form.imports {
            // Built-in libraries are not in the graph.
            if let Some(&j) = by_name.get(set.library()) {
                visit(j, found, by_name, state, path, order)?;
            }
        }
        path.pop();
        state[i] = State::Done;
        order.push(i);
        Ok(())
    }
    let mut state = vec![State::New; found.len()];
    let mut sequence = Vec::new();
    for i in 0..found.len() {
        visit(
            i,
            &found,
            &by_name,
            &mut state,
            &mut Vec::new(),
            &mut sequence,
        )?;
    }
    let mut place = vec![0; found.len()];
    for (new, &old) in sequence.iter().enumerate() {
        place[old] = new;
    }
    let mut slots: Vec<Option<Library>> = found.into_iter().map(Some).collect();
    let libraries = sequence
        .iter()
        .map(|&old| slots[old].take().expect("each once"))
        .collect();
    let by_name = by_name
        .into_iter()
        .map(|(name, old)| (name, place[old]))
        .collect();
    Ok(Graph { libraries, by_name })
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
        program(&vali_syntax::read(text, SourceId(0)).datums).unwrap()
    }

    fn names(graph: &Graph) -> Vec<String> {
        graph
            .libraries
            .iter()
            .map(|l| l.form.name.to_string())
            .collect()
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
        let graph = fetch(&program, &Identity::new("main"), &libraries()).unwrap();
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
        let graph = fetch(&program, &Identity::new("main"), &libraries()).unwrap();
        assert_eq!(names(&graph), ["(c)", "(mine)"]);
        assert_eq!(graph.libraries[1].identity.as_str(), "main#(mine)");
    }

    #[test]
    fn fetching_reports_what_is_wrong() {
        let message = |text: &str| {
            fetch(&read(text), &Identity::new("main"), &libraries())
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
        let (graph, polls) = block_on(fetch_async(&program, &Identity::new("main"), &loader));
        assert_eq!(names(&graph.unwrap()), ["(c)", "(b)", "(a)"]);
        // One wait per library fetched.
        assert_eq!(polls, 4);
    }

    #[test]
    fn a_sync_loader_also_works_asynchronously() {
        let program = read("(import (a))");
        let (graph, polls) = block_on(fetch_async(&program, &Identity::new("main"), &libraries()));
        assert_eq!(names(&graph.unwrap()), ["(c)", "(b)", "(a)"]);
        assert_eq!(polls, 1);
    }
}
