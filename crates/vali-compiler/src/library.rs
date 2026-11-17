//! Library forms, read at the level of datums: names, import sets, export
//! specifications, and `define-library` and R6RS `library` (ADR 0007 #11).
//!
//! These are read before anything is expanded, both to find a program's
//! import graph and to expand each library: library forms are never the
//! output of a macro.

// Compiling libraries, which uses these, comes in the next commit.
#![allow(dead_code, reason = "used once libraries compile")]

use std::fmt;

use vali_num::Number;
use vali_syntax::{Datum, DatumKind, Span};

use crate::CompileError;

type Result<T> = std::result::Result<T, CompileError>;

fn error(message: impl Into<String>, span: Span) -> CompileError {
    CompileError {
        message: message.into(),
        span: Some(span),
    }
}

/// A library's name: `(scheme base)`, `(srfi 64)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LibraryName(pub Vec<NamePart>);

/// One part of a library name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NamePart {
    /// An identifier.
    Symbol(String),
    /// An exact non-negative integer.
    Number(u64),
}

impl LibraryName {
    /// A name from its parts, given as text: numbers become numbers.
    pub fn new<'a>(parts: impl IntoIterator<Item = &'a str>) -> LibraryName {
        LibraryName(
            parts
                .into_iter()
                .map(|p| match p.parse() {
                    Ok(n) => NamePart::Number(n),
                    Err(_) => NamePart::Symbol(p.to_owned()),
                })
                .collect(),
        )
    }
}

impl fmt::Display for LibraryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(")?;
        for (i, part) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            match part {
                NamePart::Symbol(s) => f.write_str(s)?,
                NamePart::Number(n) => write!(f, "{n}")?,
            }
        }
        f.write_str(")")
    }
}

/// An import set (R7RS 5.6.1), naming which bindings of a library an
/// import brings in, and under what names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImportSet {
    Library(LibraryName),
    Only(Box<ImportSet>, Vec<String>),
    Except(Box<ImportSet>, Vec<String>),
    Prefix(Box<ImportSet>, String),
    Rename(Box<ImportSet>, Vec<(String, String)>),
}

impl ImportSet {
    /// The library it imports from.
    pub(crate) fn library(&self) -> &LibraryName {
        match self {
            ImportSet::Library(name) => name,
            ImportSet::Only(set, _)
            | ImportSet::Except(set, _)
            | ImportSet::Prefix(set, _)
            | ImportSet::Rename(set, _) => set.library(),
        }
    }
}

/// What a library exports: an identifier, or `(rename internal external)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExportSpec {
    pub(crate) internal: String,
    pub(crate) external: String,
    pub(crate) span: Span,
}

/// A library form, read but not expanded.
#[derive(Clone, Debug)]
pub(crate) struct LibraryForm {
    pub(crate) name: LibraryName,
    pub(crate) exports: Vec<ExportSpec>,
    pub(crate) imports: Vec<ImportSet>,
    /// The body's forms, in order.
    pub(crate) body: Vec<Datum>,
    pub(crate) span: Span,
}

fn symbol(d: &Datum) -> Option<&str> {
    match &d.kind {
        DatumKind::Symbol(s) => Some(s),
        _ => None,
    }
}

fn items(d: &Datum) -> Option<&[Datum]> {
    match &d.kind {
        DatumKind::List { items, tail: None } => Some(items),
        _ => None,
    }
}

/// A library name. An R6RS version, a list at the end, is ignored.
pub(crate) fn library_name(d: &Datum) -> Result<LibraryName> {
    let parts = items(d).ok_or_else(|| error("expected a library name", d.span))?;
    let parts = match parts.split_last() {
        Some((last, rest)) if items(last).is_some() && !rest.is_empty() => rest,
        _ => parts,
    };
    if parts.is_empty() {
        return Err(error("a library name cannot be empty", d.span));
    }
    parts
        .iter()
        .map(|p| match &p.kind {
            DatumKind::Symbol(s) => Ok(NamePart::Symbol(s.clone())),
            DatumKind::Number(Number::Integer(n)) => n
                .to_i64()
                .and_then(|n| u64::try_from(n).ok())
                .map(NamePart::Number)
                .ok_or_else(|| error("a library name's numbers must be small and exact", p.span)),
            _ => Err(error(
                "a library name is made of identifiers and numbers",
                p.span,
            )),
        })
        .collect::<Result<_>>()
        .map(LibraryName)
}

fn identifiers(ds: &[Datum], what: &str) -> Result<Vec<String>> {
    ds.iter()
        .map(|d| {
            symbol(d)
                .map(str::to_owned)
                .ok_or_else(|| error(format!("{what}: expected an identifier"), d.span))
        })
        .collect()
}

/// An import set: `(only set id ...)`, `(except set id ...)`, `(prefix set
/// id)`, `(rename set (from to) ...)`, R6RS's `(library name)` and `(for
/// set level ...)`, or a library name.
pub(crate) fn import_set(d: &Datum) -> Result<ImportSet> {
    let parts = items(d).ok_or_else(|| error("expected an import set", d.span))?;
    match parts {
        [head, set, rest @ ..] if symbol(head) == Some("only") => Ok(ImportSet::Only(
            Box::new(import_set(set)?),
            identifiers(rest, "only")?,
        )),
        [head, set, rest @ ..] if symbol(head) == Some("except") => Ok(ImportSet::Except(
            Box::new(import_set(set)?),
            identifiers(rest, "except")?,
        )),
        [head, set, prefix] if symbol(head) == Some("prefix") => {
            // A prefix such as `s1:` reads as an SRFI 88 keyword; it means
            // its text, colon included.
            let prefix = match &prefix.kind {
                DatumKind::Symbol(s) => s.clone(),
                DatumKind::Keyword(k) => format!("{k}:"),
                _ => return Err(error("prefix: expected an identifier", prefix.span)),
            };
            Ok(ImportSet::Prefix(Box::new(import_set(set)?), prefix))
        }
        [head, set, renames @ ..] if symbol(head) == Some("rename") => {
            let renames = renames
                .iter()
                .map(|r| match items(r) {
                    Some([from, to]) if symbol(from).is_some() && symbol(to).is_some() => Ok((
                        symbol(from).expect("checked").to_owned(),
                        symbol(to).expect("checked").to_owned(),
                    )),
                    _ => Err(error("rename: expected (from to)", r.span)),
                })
                .collect::<Result<_>>()?;
            Ok(ImportSet::Rename(Box::new(import_set(set)?), renames))
        }
        [head, name] if symbol(head) == Some("library") => {
            Ok(ImportSet::Library(library_name(name)?))
        }
        // Phases are implicit (ADR 0001 #1); for now an explicit level
        // imports at every phase too.
        [head, set, ..] if symbol(head) == Some("for") => import_set(set),
        _ => Ok(ImportSet::Library(library_name(d)?)),
    }
}

fn export_spec(d: &Datum) -> Result<ExportSpec> {
    if let Some(name) = symbol(d) {
        return Ok(ExportSpec {
            internal: name.to_owned(),
            external: name.to_owned(),
            span: d.span,
        });
    }
    match items(d) {
        Some([head, internal, external])
            if symbol(head) == Some("rename")
                && symbol(internal).is_some()
                && symbol(external).is_some() =>
        {
            Ok(ExportSpec {
                internal: symbol(internal).expect("checked").to_owned(),
                external: symbol(external).expect("checked").to_owned(),
                span: d.span,
            })
        }
        _ => Err(error(
            "export: expected an identifier or (rename internal external)",
            d.span,
        )),
    }
}

/// The library form `d` is, if it is `(define-library ...)` or R6RS
/// `(library ...)`.
pub(crate) fn library_form(d: &Datum) -> Result<Option<LibraryForm>> {
    let Some([head, name, declarations @ ..]) = items(d) else {
        return Ok(None);
    };
    match symbol(head) {
        Some("define-library") => {
            let mut form = LibraryForm {
                name: library_name(name)?,
                exports: Vec::new(),
                imports: Vec::new(),
                body: Vec::new(),
                span: d.span,
            };
            for declaration in declarations {
                let Some([keyword, rest @ ..]) = items(declaration) else {
                    return Err(error(
                        "define-library: expected a declaration",
                        declaration.span,
                    ));
                };
                match symbol(keyword) {
                    Some("export") => {
                        for spec in rest {
                            form.exports.push(export_spec(spec)?);
                        }
                    }
                    Some("import") => {
                        for set in rest {
                            form.imports.push(import_set(set)?);
                        }
                    }
                    Some("begin") => form.body.extend(rest.iter().cloned()),
                    Some(other) => {
                        return Err(error(
                            format!("define-library: {other} declarations are not supported yet"),
                            declaration.span,
                        ));
                    }
                    None => {
                        return Err(error(
                            "define-library: expected a declaration",
                            declaration.span,
                        ));
                    }
                }
            }
            Ok(Some(form))
        }
        Some("library") => {
            // `(library name (export spec ...) (import set ...) body ...)`.
            let [exports, imports, body @ ..] = declarations else {
                return Err(error(
                    "library: expected exports, imports and a body",
                    d.span,
                ));
            };
            let exports = match items(exports) {
                Some([k, specs @ ..]) if symbol(k) == Some("export") => {
                    specs.iter().map(export_spec).collect::<Result<Vec<_>>>()?
                }
                _ => return Err(error("library: expected (export ...)", exports.span)),
            };
            let imports = match items(imports) {
                Some([k, sets @ ..]) if symbol(k) == Some("import") => {
                    sets.iter().map(import_set).collect::<Result<Vec<_>>>()?
                }
                _ => return Err(error("library: expected (import ...)", imports.span)),
            };
            Ok(Some(LibraryForm {
                name: library_name(name)?,
                exports,
                imports,
                body: body.to_vec(),
                span: d.span,
            }))
        }
        _ => Ok(None),
    }
}

/// A program's imports: the import sets of its leading `(import ...)`
/// forms, and the forms after them.
pub(crate) fn program_imports(forms: &[Datum]) -> Result<(Vec<ImportSet>, &[Datum])> {
    let mut imports = Vec::new();
    let mut rest = forms;
    while let Some((form, more)) = rest.split_first() {
        match items(form) {
            Some([head, sets @ ..]) if symbol(head) == Some("import") => {
                for set in sets {
                    imports.push(import_set(set)?);
                }
                rest = more;
            }
            _ => break,
        }
    }
    Ok((imports, rest))
}
