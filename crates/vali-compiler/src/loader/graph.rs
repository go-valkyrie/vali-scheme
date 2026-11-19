//! The import graph: a program's libraries and theirs, fetched from the
//! loader, with the files they include and the libraries their
//! `cond-expand`s ask about, in the order they are compiled.
//!
//! A standard library with a prebuilt artifact is not read from its
//! source: what it imports comes from the artifact, which is loaded in
//! place of expanding it.

use super::*;

/// What `fetch_includes` collects: the files included, the answers to the
/// libraries asked about, and which names this walk asked about.
pub(super) struct Found<'a> {
    pub(super) includes: &'a mut Includes,
    pub(super) available: &'a mut Available,
    pub(super) asked: &'a mut Vec<LibraryName>,
}

/// A library in the import graph.
#[derive(Clone, Debug)]
pub(crate) struct Library {
    pub(crate) identity: Identity,
    /// The name it was first fetched under.
    pub(crate) name: LibraryName,
    /// Its form, as read from its source; `None` for a standard library
    /// whose prebuilt artifact is to be loaded instead.
    pub(crate) form: Option<LibraryForm>,
    /// The libraries it imports, by name, built-in ones included.
    pub(crate) imports: Vec<LibraryName>,
    /// The native library, if it is one.
    pub(crate) native: Option<Arc<vali_runtime::NativeLibrary>>,
    /// Whether it is one of Vali's own, served from the sources Vali
    /// carries, never by a host's loader: only these may import `(vali
    /// primitive)`.
    pub(crate) own: bool,
    /// A hash of what was read to make its form: its text, the files it
    /// includes, and what its `cond-expand`s found; `None` if its source
    /// was not read (ADR 0001 #8).
    pub(crate) source: Option<Digest>,
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
        imports.push(ImportSet::Library(
            LibraryName::new(["scheme", "base"]),
            None,
        ));
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

pub(super) fn error(message: impl Into<String>, span: Option<Span>) -> CompileError {
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
    fetch_beyond(program, identity, loader, sources, &|_| false)
}

/// As `fetch`, leaving out the libraries `known` says are compiled
/// already, and what only they import: evaluated code imports what the
/// program has loaded without reading it again.
pub(crate) fn fetch_beyond(
    program: &Program,
    identity: &Identity,
    loader: &impl Loader,
    sources: &mut vali_syntax::Sources,
    known: &impl Fn(&LibraryName) -> bool,
) -> Result<Graph> {
    let loader = Sync_(loader);
    let future = pin!(walk(program, identity, &loader, sources, known));
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
    walk(program, identity, &loader, sources, &|_| false).await
}

pub(super) fn list(items: Vec<Datum>, span: Span) -> Datum {
    Datum {
        kind: DatumKind::List { items, tail: None },
        span,
    }
}

/// A list's head symbol and the rest, if it has one.
pub(super) fn head(d: &Datum) -> Option<(&str, &[Datum])> {
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
pub(super) fn include_paths<'a>(
    keyword: &str,
    args: &'a [Datum],
    span: Span,
) -> Result<Vec<&'a str>> {
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

/// Reads the files an include declaration names, for `from`, noting each
/// in `fingerprint`.
pub(super) async fn read_includes(
    keyword: &str,
    args: &[Datum],
    span: Span,
    from: &Identity,
    loader: &impl Fetch,
    sources: &mut vali_syntax::Sources,
    fingerprint: &mut Hasher,
) -> Result<Vec<Datum>> {
    let mut forms = Vec::new();
    for path in include_paths(keyword, args, span)? {
        let text = loader
            .include(path, from)
            .await
            .map_err(|e| error(format!("{keyword}: {path}: {e}"), Some(span)))?
            .ok_or_else(|| error(format!("{keyword}: {from} may not read {path}"), Some(span)))?;
        fingerprint.field(keyword.as_bytes());
        fingerprint.field(path.as_bytes());
        fingerprint.field(text.as_bytes());
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
pub(super) async fn resolve_library(
    datum: &Datum,
    identity: &Identity,
    loader: &impl Fetch,
    local: &HashSet<LibraryName>,
    sources: &mut vali_syntax::Sources,
    fingerprint: &mut Hasher,
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
                forms.extend(
                    read_includes(
                        keyword,
                        args,
                        d.span,
                        identity,
                        loader,
                        sources,
                        fingerprint,
                    )
                    .await?,
                );
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
                    fingerprint,
                )
                .await?;
                for form in forms.into_iter().rev() {
                    work.push_front(form);
                }
            }
            Some(("cond-expand", clauses)) => {
                let chosen =
                    cond_expand(clauses, d.span, identity, loader, local, fingerprint).await?;
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
pub(super) async fn cond_expand(
    clauses: &[Datum],
    span: Span,
    identity: &Identity,
    loader: &impl Fetch,
    local: &HashSet<LibraryName>,
    fingerprint: &mut Hasher,
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
        fingerprint.field(name.to_string().as_bytes());
        fingerprint.update(&[u8::from(found)]);
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
pub(super) async fn fetch_includes(
    forms: &[Datum],
    from: &Identity,
    loader: &impl Fetch,
    local: &HashSet<LibraryName>,
    known: &impl Fn(&LibraryName) -> bool,
    found: Found<'_>,
) {
    let Found {
        includes,
        available,
        asked: asked_of,
    } = found;
    // The forms are walked by reference, so a deeply nested form costs
    // its size, not its size times its depth. What an included file reads
    // as is owned, so it waits in `included` for a walk of its own.
    let mut included: Vec<Vec<Datum>> = Vec::new();
    let mut first = true;
    loop {
        let owned;
        let roots: &[Datum] = if first {
            first = false;
            forms
        } else if let Some(datums) = included.pop() {
            owned = datums;
            &owned
        } else {
            break;
        };
        let mut work: Vec<&Datum> = roots.iter().collect();
        while let Some(d) = work.pop() {
            match &d.kind {
                DatumKind::List { items, tail } => {
                    match head(d) {
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
                            asked_of.extend(asked.iter().cloned());
                            for name in asked {
                                if available.contains_key(&name) {
                                    continue;
                                }
                                let found = is_standard(&name)
                                    || local.contains(&name)
                                    || known(&name)
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
                                    included.push(datums);
                                }
                                includes.insert(key, text);
                            }
                            continue;
                        }
                        _ => {}
                    }
                    work.extend(items.iter());
                    work.extend(tail.iter().map(|t| &**t));
                }
                DatumKind::Vector(items) => work.extend(items.iter()),
                _ => {}
            }
        }
    }
}

/// Reads one library form, named `name`, from a loader's source.
pub(super) async fn read_library(
    name: &LibraryName,
    source: &Source,
    loader: &impl Fetch,
    local: &HashSet<LibraryName>,
    sources: &mut vali_syntax::Sources,
) -> Result<(LibraryForm, Hasher)> {
    let text = match &source.native {
        Some(library) => native_form(library),
        None => source.text.clone(),
    };
    let mut fingerprint = Hasher::new();
    fingerprint.field(text.as_bytes());
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
    let datum = &resolve_library(
        datum,
        &source.identity,
        loader,
        local,
        sources,
        &mut fingerprint,
    )
    .await?;
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
    Ok((form, fingerprint))
}

pub(super) async fn walk(
    program: &Program,
    identity: &Identity,
    loader: &impl Fetch,
    sources: &mut vali_syntax::Sources,
    known: &impl Fn(&LibraryName) -> bool,
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
        let resolved = resolve_library(
            d,
            &local_identity,
            loader,
            &names,
            sources,
            &mut Hasher::new(),
        )
        .await?;
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
        known,
        Found {
            includes: &mut includes,
            available: &mut available,
            asked: &mut Vec::new(),
        },
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
        if by_name.contains_key(&name) || library::is_builtin(&name) || known(&name) {
            continue;
        }
        // Standard libraries come first, so no loader can replace them,
        // except those a host serves to grant what they need.
        let (library_identity, form, prebuilt, native, own, fingerprint) = match local.get(&name) {
            Some(form) => (
                Identity::new(&format!("{identity}#{name}")),
                Some((*form).clone()),
                None,
                None,
                false,
                None,
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
                // One of Vali's own with a prebuilt artifact is not read.
                let prebuilt = own.then(|| prebuilt_imports(&source.identity)).flatten();
                let (form, fingerprint) = match prebuilt {
                    Some(_) => (None, None),
                    None => {
                        let (form, fingerprint) =
                            read_library(&name, &source, loader, &names, sources).await?;
                        (Some(form), Some(fingerprint))
                    }
                };
                (
                    source.identity,
                    form,
                    prebuilt,
                    source.native,
                    own,
                    fingerprint,
                )
            }
        };
        let imports: Vec<LibraryName> = match (prebuilt, &form) {
            (Some(imports), _) => imports,
            (None, Some(form)) => form
                .imports
                .iter()
                .map(|set| set.library().clone())
                .collect(),
            (None, None) => unreachable!("a library is read unless it is prebuilt"),
        };
        for import in imports.iter().rev() {
            work.push((import.clone(), Some(library_identity.clone())));
        }
        let mut asked = Vec::new();
        if let Some(form) = &form {
            fetch_includes(
                &form.body,
                &library_identity,
                loader,
                &names,
                known,
                Found {
                    includes: &mut includes,
                    available: &mut available,
                    asked: &mut asked,
                },
            )
            .await;
        }
        asked.sort();
        asked.dedup();
        // Included files read along with its body are part of its source.
        let source = fingerprint.map(|mut fingerprint| {
            let mut read: Vec<_> = includes
                .iter()
                .filter(|((from, _), _)| from == library_identity.as_str())
                .collect();
            read.sort_by(|a, b| a.0.1.cmp(&b.0.1));
            for ((_, path), text) in read {
                fingerprint.field(path.as_bytes());
                match text {
                    Ok(text) => {
                        fingerprint.update(&[1]);
                        fingerprint.field(text.as_bytes());
                    }
                    Err(why) => {
                        fingerprint.update(&[0]);
                        fingerprint.field(why.as_bytes());
                    }
                }
            }
            // What its body's `cond-expand`s found about libraries, which
            // its expansion followed.
            for name in &asked {
                fingerprint.field(name.to_string().as_bytes());
                fingerprint.update(&[u8::from(available.get(name) == Some(&true))]);
            }
            fingerprint.finish()
        });
        let index = found.len();
        by_identity.insert(library_identity.clone(), index);
        by_name.insert(name.clone(), index);
        found.push(Library {
            identity: library_identity,
            name,
            form,
            imports,
            native,
            own,
            source,
        });
    }
    let mut graph = order(found, by_name)?;
    graph.includes = includes;
    graph.available = available;
    Ok(graph)
}

/// Orders libraries so each comes after those it imports, refusing cycles.
pub(super) fn order(found: Vec<Library>, by_name: HashMap<LibraryName, usize>) -> Result<Graph> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        New,
        Open,
        Done,
    }
    /// Visits `root` and what it imports, depth first, from a stack of its
    /// own: the chain of imports can be as long as memory allows, so
    /// recursing on it could overflow the native stack. `path` is the
    /// libraries being visited, each with how many of its imports are done.
    fn visit(
        root: usize,
        found: &[Library],
        by_name: &HashMap<LibraryName, usize>,
        state: &mut [State],
        order: &mut Vec<usize>,
    ) -> Result<()> {
        let mut path: Vec<(usize, usize)> = Vec::new();
        let mut next = Some(root);
        loop {
            if let Some(i) = next.take() {
                match state[i] {
                    State::Done => {}
                    State::Open => {
                        let from = path.iter().position(|&(p, _)| p == i).expect("on the path");
                        let cycle: Vec<String> = path[from..]
                            .iter()
                            .map(|&(p, _)| p)
                            .chain([i])
                            .map(|p| found[p].name.to_string())
                            .collect();
                        return Err(error(
                            format!("libraries import each other: {}", cycle.join(" -> ")),
                            found[i].form.as_ref().map(|f| f.span),
                        ));
                    }
                    State::New => {
                        state[i] = State::Open;
                        path.push((i, 0));
                    }
                }
            }
            let Some(&mut (i, ref mut done)) = path.last_mut() else {
                return Ok(());
            };
            match found[i].imports.get(*done) {
                Some(import) => {
                    *done += 1;
                    // Built-in libraries are not in the graph.
                    next = by_name.get(import).copied();
                }
                None => {
                    path.pop();
                    state[i] = State::Done;
                    order.push(i);
                }
            }
        }
    }
    let mut state = vec![State::New; found.len()];
    let mut sequence = Vec::new();
    for i in 0..found.len() {
        visit(i, &found, &by_name, &mut state, &mut sequence)?;
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
    use super::*;

    /// Library `(n)`, importing each of `imports`.
    fn library(n: usize, imports: &[usize]) -> Library {
        let name = |n: usize| LibraryName::new([n.to_string().as_str()]);
        Library {
            identity: Identity::new(&format!("test:{n}")),
            name: name(n),
            form: None,
            imports: imports.iter().map(|&i| name(i)).collect(),
            native: None,
            own: false,
            source: None,
        }
    }

    fn graph_of(libraries: Vec<Library>) -> Result<Graph> {
        let by_name = libraries
            .iter()
            .enumerate()
            .map(|(i, l)| (l.name.clone(), i))
            .collect();
        order(libraries, by_name)
    }

    #[test]
    fn a_long_chain_of_imports_is_ordered_on_a_small_stack() {
        // Each library imports the next, 200,000 deep, listed in the worst
        // order: the first visited starts the longest chain. A recursive
        // search needs a frame for each, far more than a test thread's 2
        // MiB stack holds.
        const DEEP: usize = 200_000;
        let libraries = (0..DEEP)
            .map(|n| {
                if n + 1 < DEEP {
                    library(n, &[n + 1])
                } else {
                    library(n, &[])
                }
            })
            .collect();
        let graph = graph_of(libraries).unwrap();
        // Each library comes after the one it imports: the reverse order.
        let names: Vec<_> = graph.libraries.iter().map(|l| l.name.clone()).collect();
        assert_eq!(names.len(), DEEP);
        assert_eq!(
            names[0],
            LibraryName::new([(DEEP - 1).to_string().as_str()])
        );
        assert_eq!(names[DEEP - 1], LibraryName::new(["0"]));
    }

    #[test]
    fn imports_come_first_and_cycles_are_named() {
        // A diamond: 0 imports 1 and 2, which both import 3.
        let graph = graph_of(vec![
            library(0, &[1, 2]),
            library(1, &[3]),
            library(2, &[3]),
            library(3, &[]),
        ])
        .unwrap();
        let order: Vec<String> = graph.libraries.iter().map(|l| l.name.to_string()).collect();
        assert_eq!(order, ["(3)", "(1)", "(2)", "(0)"]);
        // 1 -> 2 -> 3 -> 1, reached from 0.
        let e = graph_of(vec![
            library(0, &[1]),
            library(1, &[2]),
            library(2, &[3]),
            library(3, &[1]),
        ])
        .unwrap_err();
        assert!(
            e.message
                .contains("libraries import each other: (1) -> (2) -> (3) -> (1)"),
            "{}",
            e.message
        );
    }
}
