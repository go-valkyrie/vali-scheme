//! A reference syntax type, for tests only (ADR 0015 #8).
//!
//! [`Syntax`] is a plain Rust tree that propagates scope changes eagerly, to
//! every node, at once. It exists to test the scope machinery against known
//! hygiene examples, and later to check the runtime's syntax objects against.
//! It is compiled only for tests and with the `reference` feature.

use std::sync::Arc;

use crate::binding::{BindingTable, Phase, Resolution};
use crate::scope::{Scope, ScopeContext, ScopeKind, ScopeSetId, TooManyScopes};
use crate::{Datum, DatumKind, Origin, Span};

/// A syntax object: a datum's shape, with a scope set, span and origin on
/// every node.
#[derive(Clone, Debug)]
pub struct Syntax {
    /// What the node is.
    pub shape: Shape,
    /// Its scopes.
    pub scopes: ScopeSetId,
    /// Where it was written.
    pub span: Span,
    /// The macro use that produced it, if any.
    pub origin: Option<Arc<Origin>>,
}

/// The shapes of [`Syntax`].
#[derive(Clone, Debug)]
pub enum Shape {
    /// An identifier.
    Identifier(Box<str>),
    /// A list, proper when `tail` is `None`.
    List {
        /// The elements before any `.`.
        items: Vec<Syntax>,
        /// The element after the `.`, if there is one.
        tail: Option<Box<Syntax>>,
    },
    /// A vector.
    Vector(Vec<Syntax>),
    /// Any other datum, kept whole. Labels are kept whole too, so the
    /// reference type never sees shared structure.
    Atom(DatumKind),
}

impl Syntax {
    /// `datum` as syntax, with `scopes` on every node and no origin.
    pub fn from_datum(datum: &Datum, scopes: ScopeSetId) -> Syntax {
        let shape = match &datum.kind {
            DatumKind::Symbol(name) => Shape::Identifier(name.as_str().into()),
            DatumKind::List { items, tail } => Shape::List {
                items: items
                    .iter()
                    .map(|d| Syntax::from_datum(d, scopes))
                    .collect(),
                tail: tail
                    .as_ref()
                    .map(|d| Box::new(Syntax::from_datum(d, scopes))),
            },
            DatumKind::Vector(items) => Shape::Vector(
                items
                    .iter()
                    .map(|d| Syntax::from_datum(d, scopes))
                    .collect(),
            ),
            other => Shape::Atom(other.clone()),
        };
        Syntax {
            shape,
            scopes,
            span: datum.span,
            origin: None,
        }
    }

    /// `datum` as syntax with the scopes, span and origin of `context`:
    /// `datum->syntax`.
    pub fn datum_to_syntax(context: &Syntax, datum: &Datum) -> Syntax {
        let mut syntax = Syntax::from_datum(datum, context.scopes);
        syntax.for_each_mut(&mut |node| {
            node.span = context.span;
            node.origin.clone_from(&context.origin);
        });
        syntax
    }

    /// The datum, without scopes: `syntax->datum`.
    pub fn to_datum(&self) -> Datum {
        let kind = match &self.shape {
            Shape::Identifier(name) => DatumKind::Symbol(name.to_string()),
            Shape::List { items, tail } => DatumKind::List {
                items: items.iter().map(Syntax::to_datum).collect(),
                tail: tail.as_ref().map(|t| Box::new(t.to_datum())),
            },
            Shape::Vector(items) => DatumKind::Vector(items.iter().map(Syntax::to_datum).collect()),
            Shape::Atom(kind) => kind.clone(),
        };
        Datum {
            kind,
            span: self.span,
        }
    }

    /// The identifier's name, if this is an identifier.
    pub fn identifier(&self) -> Option<&str> {
        match &self.shape {
            Shape::Identifier(name) => Some(name),
            _ => None,
        }
    }

    /// The elements of a proper list, if this is one.
    pub fn list(&self) -> Option<&[Syntax]> {
        match &self.shape {
            Shape::List { items, tail: None } => Some(items),
            _ => None,
        }
    }

    /// Adds `scope` to every node.
    pub fn add(&mut self, context: &mut ScopeContext, scope: Scope) -> Result<(), TooManyScopes> {
        self.try_for_each_mut(&mut |node| {
            node.scopes = context.add(node.scopes, scope)?;
            Ok(())
        })
    }

    /// Removes `scope` from every node.
    pub fn remove(
        &mut self,
        context: &mut ScopeContext,
        scope: Scope,
    ) -> Result<(), TooManyScopes> {
        self.try_for_each_mut(&mut |node| {
            node.scopes = context.remove(node.scopes, scope)?;
            Ok(())
        })
    }

    /// Flips `scope` on every node.
    pub fn flip(&mut self, context: &mut ScopeContext, scope: Scope) -> Result<(), TooManyScopes> {
        self.try_for_each_mut(&mut |node| {
            node.scopes = context.flip(node.scopes, scope)?;
            Ok(())
        })
    }

    /// What this identifier refers to at `phase`. Not an identifier: unbound.
    pub fn resolve<'t, B>(
        &self,
        context: &ScopeContext,
        table: &'t BindingTable<String, B>,
        phase: Phase,
    ) -> Resolution<'t, B> {
        match self.identifier() {
            Some(name) => table.resolve(context, name, self.scopes, phase),
            None => Resolution::Unbound,
        }
    }

    fn for_each_mut(&mut self, f: &mut impl FnMut(&mut Syntax)) {
        let _ = self.try_for_each_mut(&mut |node| {
            f(node);
            Ok::<(), ()>(())
        });
    }

    fn try_for_each_mut<E>(
        &mut self,
        f: &mut impl FnMut(&mut Syntax) -> Result<(), E>,
    ) -> Result<(), E> {
        f(self)?;
        match &mut self.shape {
            Shape::List { items, tail } => {
                for item in items.iter_mut().chain(tail.as_deref_mut()) {
                    item.try_for_each_mut(f)?;
                }
            }
            Shape::Vector(items) => {
                for item in items {
                    item.try_for_each_mut(f)?;
                }
            }
            Shape::Identifier(_) | Shape::Atom(_) => {}
        }
        Ok(())
    }
}

/// Whether `a` and `b` are the same identifier with the same scopes:
/// `bound-identifier=?`. A binding for one would bind the other.
pub fn bound_identifier_eq(a: &Syntax, b: &Syntax) -> bool {
    a.identifier().is_some() && a.identifier() == b.identifier() && a.scopes == b.scopes
}

/// Whether `a` and `b` refer to the same binding at `phase`, or are both
/// unbound with the same name: `free-identifier=?`.
pub fn free_identifier_eq<B: PartialEq>(
    context: &ScopeContext,
    table: &BindingTable<String, B>,
    phase: Phase,
    a: &Syntax,
    b: &Syntax,
) -> bool {
    match (
        a.resolve(context, table, phase),
        b.resolve(context, table, phase),
    ) {
        (Resolution::Bound(x), Resolution::Bound(y)) => x == y,
        (Resolution::Unbound, Resolution::Unbound) => {
            a.identifier().is_some() && a.identifier() == b.identifier()
        }
        _ => false,
    }
}

/// One macro step. Flips a fresh macro scope on `input`, runs `transform`,
/// gives `origin` to every node the transformer introduced (those without
/// the macro scope), and flips the scope again on the result. Introduced
/// nodes end with the macro scope, and nodes passed through end without it.
pub fn apply_macro(
    context: &mut ScopeContext,
    mut input: Syntax,
    origin: Option<Arc<Origin>>,
    transform: impl FnOnce(&mut ScopeContext, Syntax) -> Syntax,
) -> Result<Syntax, TooManyScopes> {
    let scope = context.new_scope(ScopeKind::Macro)?;
    input.flip(context, scope)?;
    let mut output = transform(context, input);
    if let Some(origin) = origin {
        output.for_each_mut(&mut |node| {
            if !context.contains(node.scopes, scope) {
                node.origin = Some(Arc::clone(&origin));
            }
        });
    }
    output.flip(context, scope)?;
    Ok(output)
}
