//! Macro origins (ADR 0014 #7, ADR 0015 #7).

use std::sync::Arc;

use crate::Span;

/// The macro use that produced a syntax object. Origins form a chain, shared
/// between every syntax object one macro step produces, back to code a
/// person wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    /// The macro's name, as written at the use.
    pub macro_name: Box<str>,
    /// Where the macro was used.
    pub use_site: Span,
    /// The origin of the macro use itself, if a macro produced it too.
    pub parent: Option<Arc<Origin>>,
}

impl Origin {
    /// This origin, then its parent, and so on: innermost macro use first.
    pub fn chain(&self) -> impl Iterator<Item = &Origin> {
        std::iter::successors(Some(self), |o| o.parent.as_deref())
    }
}
