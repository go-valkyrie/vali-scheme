//! `cond-expand` (R7RS 4.2.1 and 5.6.1): feature requirements, decided the
//! same way for library declarations, which fetching resolves, and for
//! forms, which the expander does.

use vali_syntax::{Datum, DatumKind};

use crate::CompileError;
use crate::library::{self, LibraryName};

type Result<T> = std::result::Result<T, CompileError>;

fn error(d: &Datum) -> CompileError {
    CompileError {
        message: "cond-expand: expected a feature requirement".to_owned(),
        span: Some(d.span),
        location: None,
    }
}

/// A requirement's operator and operands, if it is a list.
fn parts(d: &Datum) -> Option<(&str, &[Datum])> {
    match &d.kind {
        DatumKind::List { items, tail: None } => match items.split_first() {
            Some((
                Datum {
                    kind: DatumKind::Symbol(head),
                    ..
                },
                rest,
            )) => Some((head, rest)),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a clause's requirement is `else`.
pub(crate) fn is_else(requirement: &Datum) -> bool {
    matches!(&requirement.kind, DatumKind::Symbol(s) if s == "else")
}

/// Whether `requirement` holds, `library` saying which libraries are
/// available.
pub(crate) fn satisfied(
    requirement: &Datum,
    library: &mut dyn FnMut(&LibraryName) -> bool,
) -> Result<bool> {
    if let DatumKind::Symbol(name) = &requirement.kind {
        return Ok(vali_runtime::FEATURES.contains(&name.as_str()));
    }
    match parts(requirement).ok_or_else(|| error(requirement))? {
        ("and", rest) => {
            for r in rest {
                if !satisfied(r, library)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        ("or", rest) => {
            for r in rest {
                if satisfied(r, library)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        ("not", [r]) => Ok(!satisfied(r, library)?),
        ("library", [name]) => Ok(library(&library::library_name(name)?)),
        _ => Err(error(requirement)),
    }
}

/// The libraries `requirement` asks about, so they can be looked for first.
pub(crate) fn libraries(requirement: &Datum, out: &mut Vec<LibraryName>) -> Result<()> {
    match parts(requirement) {
        Some(("and" | "or" | "not", rest)) => {
            for r in rest {
                libraries(r, out)?;
            }
        }
        Some(("library", [name])) => out.push(library::library_name(name)?),
        _ => {}
    }
    Ok(())
}
