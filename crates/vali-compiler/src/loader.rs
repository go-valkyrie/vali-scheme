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

use vali_syntax::{Datum, DatumKind, Span};

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
    (
        &["scheme", "process-context"],
        include_str!("../lib/scheme/process-context.sld"),
    ),
    (&["scheme", "read"], include_str!("../lib/scheme/read.sld")),
    (&["scheme", "repl"], include_str!("../lib/scheme/repl.sld")),
    (&["scheme", "time"], include_str!("../lib/scheme/time.sld")),
    (&["vali", "exit"], include_str!("../lib/vali/exit.sld")),
    (&["vali", "syntax"], include_str!("../lib/vali/syntax.sld")),
    (
        &["scheme", "write"],
        include_str!("../lib/scheme/write.sld"),
    ),
    (&["srfi", "1"], include_str!("../lib/srfi/1.sld")),
    (&["srfi", "64"], include_str!("../lib/srfi/64.sld")),
    (&["srfi", "88"], include_str!("../lib/srfi/88.sld")),
    (&["srfi", "132"], include_str!("../lib/srfi/132.sld")),
];

/// The names of Vali's own libraries written in Scheme.
pub(crate) fn standard_names() -> Vec<LibraryName> {
    STANDARD
        .iter()
        .map(|(parts, _)| LibraryName::new(parts.iter().copied()))
        .collect()
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

/// A library in the import graph.
#[derive(Clone, Debug)]
pub(crate) struct Library {
    pub(crate) identity: Identity,
    pub(crate) form: LibraryForm,
    /// The native library, if it is one.
    pub(crate) native: Option<Arc<vali_runtime::NativeLibrary>>,
    /// Whether it is one of Vali's own, served from the sources Vali
    /// carries, never by a host's loader: only these may import `(vali
    /// primitive)`.
    pub(crate) own: bool,
}

/// A program, read: the libraries it defines itself, as read, its
/// imports, and its body.
#[derive(Clone, Debug)]
pub(crate) struct Program {
    pub(crate) libraries: Vec<Datum>,
    pub(crate) imports: Vec<ImportSet>,
    /// Whether the imports are the implicit `(scheme base)` of a program
    /// with no `import` form.
    pub(crate) implicit: bool,
    pub(crate) body: Vec<Datum>,
    /// Whether it may import `(vali primitive)`, as only Vali's own tests'
    /// programs may.
    pub(crate) internal: bool,
}

/// Reads a program's parts: leading library forms, then leading imports.
pub(crate) fn program(datums: &[Datum]) -> Result<Program> {
    let mut libraries = Vec::new();
    let mut rest = datums;
    while let Some((first, more)) = rest.split_first() {
        if !library::is_library_form(first) {
            break;
        }
        libraries.push(first.clone());
        rest = more;
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
        internal: false,
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
    /// The files bodies include, by the identity including them and the
    /// path: their text, or why it could not be read.
    pub(crate) includes: Includes,
    /// The libraries bodies' `cond-expand`s test, and whether each is
    /// available.
    pub(crate) available: Available,
}

/// Included files' texts, by including identity and path, fetched before
/// expansion so the expander never awaits, and each read once (ADR 0009
/// #13).
pub(crate) type Includes = HashMap<(String, String), std::result::Result<String, String>>;

/// Whether each library a `cond-expand` in a body tests with `(library
/// name)` is available, asked of the loader before expansion as the
/// declaration-level `cond-expand` asks, so the answer never depends on
/// what was compiled first.
pub(crate) type Available = HashMap<LibraryName, bool>;

fn error(message: impl Into<String>, span: Option<Span>) -> CompileError {
    CompileError {
        message: message.into(),
        span,
        location: None,
    }
}

/// Fetches the import graph of `program` with a synchronous loader.
pub(crate) fn fetch(
    program: &Program,
    identity: &Identity,
    loader: &impl Loader,
    sources: &mut vali_syntax::Sources,
) -> Result<Graph> {
    let loader = Sync_(loader);
    let future = pin!(walk(program, identity, &loader, sources));
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
    sources: &mut vali_syntax::Sources,
) -> Result<Graph> {
    let loader = Async(loader);
    walk(program, identity, &loader, sources).await
}

fn list(items: Vec<Datum>, span: Span) -> Datum {
    Datum {
        kind: DatumKind::List { items, tail: None },
        span,
    }
}

/// A list's head symbol and the rest, if it has one.
fn head(d: &Datum) -> Option<(&str, &[Datum])> {
    match &d.kind {
        DatumKind::List { items, tail: None } => match items.split_first() {
            Some((
                Datum {
                    kind: DatumKind::Symbol(h),
                    ..
                },
                rest,
            )) => Some((h, rest)),
            _ => None,
        },
        _ => None,
    }
}

/// Reads the text of an included file as data, case-folded for
/// `include-ci`, registering it in `sources` under its path. The directive
/// `include-ci` adds goes on the first line, so lines keep their numbers.
pub(crate) fn read_included(
    text: &str,
    ci: bool,
    path: &str,
    sources: &mut vali_syntax::Sources,
) -> Result<Vec<Datum>> {
    let text = if ci {
        format!("#!fold-case {text}")
    } else {
        text.to_owned()
    };
    let id = sources.add(path, &text);
    let read = vali_syntax::read(&text, id);
    if let Some(e) = read.errors.first() {
        let span = Span {
            source: id,
            range: e.range,
        };
        return Err(error(format!("read: {}", e.kind), Some(span)).located(sources));
    }
    Ok(read.datums)
}

/// The paths an `include` or `include-ci` form names.
fn include_paths<'a>(keyword: &str, args: &'a [Datum], span: Span) -> Result<Vec<&'a str>> {
    args.iter()
        .map(|a| match &a.kind {
            DatumKind::String(s) => Ok(s.as_str()),
            _ => Err(error(
                format!("{keyword}: expected file names as strings"),
                Some(span),
            )),
        })
        .collect()
}

/// Reads the files an include declaration names, for `from`.
async fn read_includes(
    keyword: &str,
    args: &[Datum],
    span: Span,
    from: &Identity,
    loader: &impl Fetch,
    sources: &mut vali_syntax::Sources,
) -> Result<Vec<Datum>> {
    let mut forms = Vec::new();
    for path in include_paths(keyword, args, span)? {
        let text = loader
            .include(path, from)
            .await
            .map_err(|e| error(format!("{keyword}: {path}: {e}"), Some(span)))?
            .ok_or_else(|| error(format!("{keyword}: {from} may not read {path}"), Some(span)))?;
        forms.extend(read_included(
            &text,
            keyword == "include-ci",
            path,
            sources,
        )?);
    }
    Ok(forms)
}

/// Resolves a `define-library` form's `include`, `include-ci`,
/// `include-library-declarations` and `cond-expand` declarations into the
/// declarations they stand for (R7RS 5.6.1), reading files for `identity`.
async fn resolve_library(
    datum: &Datum,
    identity: &Identity,
    loader: &impl Fetch,
    local: &HashSet<LibraryName>,
    sources: &mut vali_syntax::Sources,
) -> Result<Datum> {
    let Some(("define-library", [name, declarations @ ..])) = head(datum) else {
        return Ok(datum.clone());
    };
    let mut work: std::collections::VecDeque<Datum> = declarations.iter().cloned().collect();
    let mut resolved = vec![
        Datum {
            kind: DatumKind::Symbol("define-library".to_owned()),
            span: datum.span,
        },
        name.clone(),
    ];
    while let Some(d) = work.pop_front() {
        match head(&d) {
            Some((keyword @ ("include" | "include-ci"), args)) => {
                let mut forms = vec![Datum {
                    kind: DatumKind::Symbol("begin".to_owned()),
                    span: d.span,
                }];
                forms
                    .extend(read_includes(keyword, args, d.span, identity, loader, sources).await?);
                resolved.push(list(forms, d.span));
            }
            Some(("include-library-declarations", args)) => {
                let forms = read_includes(
                    "include-library-declarations",
                    args,
                    d.span,
                    identity,
                    loader,
                    sources,
                )
                .await?;
                for form in forms.into_iter().rev() {
                    work.push_front(form);
                }
            }
            Some(("cond-expand", clauses)) => {
                let chosen = cond_expand(clauses, d.span, identity, loader, local).await?;
                for form in chosen.into_iter().rev() {
                    work.push_front(form);
                }
            }
            _ => resolved.push(d),
        }
    }
    Ok(list(resolved, datum.span))
}

/// The declarations of the first `cond-expand` clause whose requirement
/// holds, or none. A `(library name)` requirement holds if the library is
/// Vali's, the program's own, or one the loader has.
async fn cond_expand(
    clauses: &[Datum],
    span: Span,
    identity: &Identity,
    loader: &impl Fetch,
    local: &HashSet<LibraryName>,
) -> Result<Vec<Datum>> {
    fn clause(c: &Datum, span: Span) -> Result<(&Datum, &[Datum])> {
        match &c.kind {
            DatumKind::List { items, tail: None } if !items.is_empty() => {
                Ok((&items[0], &items[1..]))
            }
            _ => Err(error("cond-expand: expected a clause", Some(span))),
        }
    }
    let mut asked = Vec::new();
    for c in clauses {
        crate::features::libraries(clause(c, span)?.0, &mut asked)?;
    }
    let mut available = HashSet::new();
    for name in asked {
        let found = is_standard(&name)
            || local.contains(&name)
            || loader
                .fetch(&name, Some(identity))
                .await
                .map_err(|e| error(format!("{name}: {e}"), Some(span)))?
                .is_some();
        if found {
            available.insert(name);
        }
    }
    for c in clauses {
        let (requirement, body) = clause(c, span)?;
        if crate::features::is_else(requirement)
            || crate::features::satisfied(requirement, &mut |n| available.contains(n))?
        {
            return Ok(body.to_vec());
        }
    }
    Ok(Vec::new())
}

/// Fetches every file that `forms` include literally, and those files
/// include in turn, for `from`, into `includes`. A file that cannot be read
/// is recorded as such: it is an error only if expansion reaches it.
async fn fetch_includes(
    forms: &[Datum],
    from: &Identity,
    loader: &impl Fetch,
    local: &HashSet<LibraryName>,
    includes: &mut Includes,
    available: &mut Available,
) {
    let mut work: Vec<Datum> = forms.to_vec();
    while let Some(d) = work.pop() {
        match &d.kind {
            DatumKind::List { items, tail } => {
                match head(&d) {
                    Some(("quote" | "quasiquote" | "quote-syntax", _)) => continue,
                    // Its clauses' forms are scanned below, as any list's.
                    Some(("cond-expand", clauses)) => {
                        let mut asked = Vec::new();
                        for clause in clauses {
                            if let DatumKind::List { items, .. } = &clause.kind
                                && let Some(requirement) = items.first()
                            {
                                let _ = crate::features::libraries(requirement, &mut asked);
                            }
                        }
                        for name in asked {
                            if available.contains_key(&name) {
                                continue;
                            }
                            let found = is_standard(&name)
                                || local.contains(&name)
                                || matches!(loader.fetch(&name, Some(from)).await, Ok(Some(_)));
                            available.insert(name, found);
                        }
                    }
                    Some((keyword @ ("include" | "include-ci"), args)) => {
                        let Ok(paths) = include_paths(keyword, args, d.span) else {
                            continue;
                        };
                        for path in paths {
                            let key = (from.as_str().to_owned(), path.to_owned());
                            if includes.contains_key(&key) {
                                continue;
                            }
                            let text = match loader.include(path, from).await {
                                Ok(Some(text)) => Ok(text),
                                Ok(None) => Err(format!("{from} may not read {path}")),
                                Err(e) => Err(format!("{path}: {e}")),
                            };
                            // Read only to find what it includes: the
                            // expander registers it when it reads it.
                            if let Ok(text) = &text
                                && let Ok(datums) = read_included(
                                    text,
                                    keyword == "include-ci",
                                    path,
                                    &mut vali_syntax::Sources::new(),
                                )
                            {
                                work.extend(datums);
                            }
                            includes.insert(key, text);
                        }
                        continue;
                    }
                    _ => {}
                }
                work.extend(items.iter().cloned());
                work.extend(tail.iter().map(|t| (**t).clone()));
            }
            DatumKind::Vector(items) => work.extend(items.iter().cloned()),
            _ => {}
        }
    }
}

/// Reads one library form, named `name`, from a loader's source.
async fn read_library(
    name: &LibraryName,
    source: &Source,
    loader: &impl Fetch,
    local: &HashSet<LibraryName>,
    sources: &mut vali_syntax::Sources,
) -> Result<LibraryForm> {
    let text = match &source.native {
        Some(library) => native_form(library),
        None => source.text.clone(),
    };
    let id = sources.add(source.identity.as_str(), &text);
    let read = vali_syntax::read(&text, id);
    if let Some(e) = read.errors.first() {
        let span = Span {
            source: id,
            range: e.range,
        };
        return Err(error(format!("read: {}", e.kind), Some(span)));
    }
    let [datum] = &read.datums[..] else {
        return Err(error(
            format!("{}: expected exactly one library form", source.identity),
            None,
        ));
    };
    let datum = &resolve_library(datum, &source.identity, loader, local, sources).await?;
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

async fn walk(
    program: &Program,
    identity: &Identity,
    loader: &impl Fetch,
    sources: &mut vali_syntax::Sources,
) -> Result<Graph> {
    // Libraries the program defines itself need no loader, beyond what
    // they include.
    let mut names = HashSet::new();
    for d in &program.libraries {
        names.insert(library::form_name(d)?);
    }
    let mut forms = Vec::new();
    for d in &program.libraries {
        let name = library::form_name(d)?;
        let local_identity = Identity::new(&format!("{identity}#{name}"));
        let resolved = resolve_library(d, &local_identity, loader, &names, sources).await?;
        forms.push(library::library_form(&resolved)?.expect("a library form"));
    }
    let local: HashMap<&LibraryName, &LibraryForm> = forms.iter().map(|l| (&l.name, l)).collect();
    let mut includes = Includes::new();
    let mut available = Available::new();
    fetch_includes(
        &program.body,
        identity,
        loader,
        &names,
        &mut includes,
        &mut available,
    )
    .await;
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
        // Standard libraries come first, so no loader can replace them,
        // except those a host serves to grant what they need.
        let (library_identity, form, native, own) = match local.get(&name) {
            Some(form) => (
                Identity::new(&format!("{identity}#{name}")),
                (*form).clone(),
                None,
                false,
            ),
            None => {
                let fetched = if is_host_served(&name) || standard(&name).is_none() {
                    loader
                        .fetch(&name, importer.as_ref())
                        .await
                        .map_err(|e| error(format!("{name}: {e}"), None))?
                } else {
                    None
                };
                let own = fetched.is_none();
                let source = fetched.or_else(|| standard(&name));
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
                let form = read_library(&name, &source, loader, &names, sources).await?;
                (source.identity, form, source.native, own)
            }
        };
        for set in form.imports.iter().rev() {
            work.push((set.library().clone(), Some(library_identity.clone())));
        }
        fetch_includes(
            &form.body,
            &library_identity,
            loader,
            &names,
            &mut includes,
            &mut available,
        )
        .await;
        let index = found.len();
        by_identity.insert(library_identity.clone(), index);
        by_name.insert(name, index);
        found.push(Library {
            identity: library_identity,
            form,
            native,
            own,
        });
    }
    let mut graph = order(found, by_name)?;
    graph.includes = includes;
    graph.available = available;
    Ok(graph)
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
    Ok(Graph {
        libraries,
        by_name,
        includes: Includes::new(),
        available: Available::new(),
    })
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
}
