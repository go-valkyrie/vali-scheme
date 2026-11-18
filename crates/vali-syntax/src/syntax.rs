//! Syntax objects (ADR 0015 #6, #7 and #13).
//!
//! A [`Syntax`] is a datum's shape with a scope set, a span and an origin.
//! Its content sits behind an [`Arc`], so cloning one is cheap and a macro
//! that passes a subform through shares it.
//!
//! Scope changes propagate lazily. Adding, removing or flipping a scope
//! changes the node's own scope set and records the change as *pending* for
//! everything below it. The change reaches the children only when the node
//! is taken apart with [`Syntax::expose`], one level at a time. Pending
//! changes to one scope compose: the two flips a macro step makes on a
//! subform it passes through cancel out, so pending changes never outgrow
//! the number of distinct scopes they touch.
//!
//! Origins are not propagated: whatever builds a node gives it the origin
//! of the macro step building it.

use std::sync::Arc;

use crate::binding::{BindingTable, Phase, Resolution};
use crate::scope::{Remap, Scope, ScopeContext, ScopeSetId, TooManyScopes};
use crate::{Datum, DatumKind, Origin, Span};

/// A syntax object.
#[derive(Clone, Debug)]
pub struct Syntax {
    content: Arc<Content>,
    scopes: ScopeSetId,
    /// Changes to apply to every node below this one, sorted by scope.
    pending: Option<Arc<[(Scope, Effect)]>>,
    span: Span,
    origin: Option<Arc<Origin>>,
}

#[derive(Debug)]
enum Content {
    Identifier(Box<str>),
    List {
        items: Vec<Syntax>,
        tail: Option<Syntax>,
    },
    Vector(Vec<Syntax>),
    Atom(DatumKind),
}

/// What one scope change does to whether a set holds the scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Add,
    Remove,
    Flip,
}

impl Effect {
    /// `self`, then `then`. `None` is no change.
    fn then(self, then: Effect) -> Option<Effect> {
        match (self, then) {
            (_, Effect::Add) => Some(Effect::Add),
            (_, Effect::Remove) => Some(Effect::Remove),
            (Effect::Add, Effect::Flip) => Some(Effect::Remove),
            (Effect::Remove, Effect::Flip) => Some(Effect::Add),
            (Effect::Flip, Effect::Flip) => None,
        }
    }

    pub(crate) fn apply(
        self,
        context: &mut ScopeContext,
        set: ScopeSetId,
        scope: Scope,
    ) -> Result<ScopeSetId, TooManyScopes> {
        match self {
            Effect::Add => context.add(set, scope),
            Effect::Remove => context.remove(set, scope),
            Effect::Flip => context.flip(set, scope),
        }
    }
}

/// `first`, then `then`, as one sorted list of changes.
fn compose(
    first: Option<&Arc<[(Scope, Effect)]>>,
    then: &[(Scope, Effect)],
) -> Option<Arc<[(Scope, Effect)]>> {
    let first = first.map_or(&[][..], |p| &p[..]);
    let mut out = Vec::with_capacity(first.len() + then.len());
    let (mut i, mut j) = (0, 0);
    while i < first.len() || j < then.len() {
        let next = match (first.get(i), then.get(j)) {
            (Some(&(a, x)), Some(&(b, y))) if a == b => {
                i += 1;
                j += 1;
                x.then(y).map(|e| (a, e))
            }
            (Some(&(a, x)), Some(&(b, _))) if a < b => {
                i += 1;
                Some((a, x))
            }
            (Some(&change), None) => {
                i += 1;
                Some(change)
            }
            (_, Some(&change)) => {
                j += 1;
                Some(change)
            }
            (None, None) => unreachable!(),
        };
        out.extend(next);
    }
    (!out.is_empty()).then(|| out.into())
}

/// A syntax object's own node, with pending scope changes not applied:
/// what an encoding stores (ADR 0001 #7), from [`Syntax::node`].
#[derive(Clone, Copy, Debug)]
pub enum Node<'a> {
    Identifier(&'a str),
    List {
        items: &'a [Syntax],
        tail: Option<&'a Syntax>,
    },
    Vector(&'a [Syntax]),
    Atom(&'a DatumKind),
}

/// A node to build a syntax object from, with [`Syntax::from_parts`].
#[derive(Clone, Debug)]
pub enum NewNode {
    Identifier(Box<str>),
    List {
        items: Vec<Syntax>,
        tail: Option<Syntax>,
    },
    Vector(Vec<Syntax>),
    Atom(DatumKind),
}

/// A syntax object taken apart one level, from [`Syntax::expose`].
#[derive(Clone, Debug)]
pub enum Exposed<'a> {
    /// An identifier, by name.
    Identifier(&'a str),
    /// A list, proper when `tail` is `None`.
    List {
        /// The elements before any `.`.
        items: Vec<Syntax>,
        /// The element after the `.`, if there is one.
        tail: Option<Syntax>,
    },
    /// A vector.
    Vector(Vec<Syntax>),
    /// Any other datum.
    Atom(&'a DatumKind),
}

impl Syntax {
    /// This syntax object, made in `from`, with its scopes mapped into
    /// `to` by `remap`, pending changes included. Spans and origins are
    /// kept.
    pub fn remap(
        &self,
        remap: &mut Remap,
        from: &ScopeContext,
        to: &mut ScopeContext,
    ) -> Result<Syntax, TooManyScopes> {
        let scopes = remap.set(from, to, self.scopes)?;
        let pending = match &self.pending {
            None => None,
            Some(changes) => {
                let mut mapped = changes
                    .iter()
                    .map(|&(scope, effect)| Ok((remap.scope(from, to, scope)?, effect)))
                    .collect::<Result<Vec<_>, TooManyScopes>>()?;
                // Pending changes are kept sorted by scope.
                mapped.sort_by_key(|&(scope, _)| scope);
                Some(mapped.into())
            }
        };
        let content = match &*self.content {
            Content::Identifier(name) => Content::Identifier(name.clone()),
            Content::List { items, tail } => Content::List {
                items: items
                    .iter()
                    .map(|item| item.remap(remap, from, to))
                    .collect::<Result<_, _>>()?,
                tail: tail
                    .as_ref()
                    .map(|tail| tail.remap(remap, from, to))
                    .transpose()?,
            },
            Content::Vector(items) => Content::Vector(
                items
                    .iter()
                    .map(|item| item.remap(remap, from, to))
                    .collect::<Result<_, _>>()?,
            ),
            Content::Atom(datum) => Content::Atom(datum.clone()),
        };
        Ok(Syntax {
            content: Arc::new(content),
            scopes,
            pending,
            span: remap.span(self.span),
            origin: if remap.moves_sources() {
                self.origin.as_ref().map(|o| remap_origin(remap, o))
            } else {
                self.origin.clone()
            },
        })
    }

    fn new(content: Content, scopes: ScopeSetId, span: Span, origin: Option<Arc<Origin>>) -> Self {
        Syntax {
            content: Arc::new(content),
            scopes,
            pending: None,
            span,
            origin,
        }
    }

    /// An identifier.
    pub fn identifier(
        name: &str,
        scopes: ScopeSetId,
        span: Span,
        origin: Option<Arc<Origin>>,
    ) -> Self {
        Syntax::new(Content::Identifier(name.into()), scopes, span, origin)
    }

    /// A list of `items`, dotted with `tail` if there is one. A dotted tail
    /// that is itself a list is spliced in, so each list has one shape.
    pub fn list(
        mut items: Vec<Syntax>,
        mut tail: Option<Syntax>,
        scopes: ScopeSetId,
        span: Span,
        origin: Option<Arc<Origin>>,
    ) -> Self {
        // Only a tail with nothing pending can be spliced without taking
        // it apart; anything else stays a dotted tail, which means the same.
        while let Some(t) = &tail
            && t.pending.is_none()
            && t.scopes == scopes
            && let Content::List {
                items: more,
                tail: rest,
            } = &*t.content
        {
            items.extend(more.iter().cloned());
            tail = rest.clone();
        }
        Syntax::new(Content::List { items, tail }, scopes, span, origin)
    }

    /// A vector.
    pub fn vector(
        items: Vec<Syntax>,
        scopes: ScopeSetId,
        span: Span,
        origin: Option<Arc<Origin>>,
    ) -> Self {
        Syntax::new(Content::Vector(items), scopes, span, origin)
    }

    /// Any datum other than an identifier, list or vector.
    pub fn atom(
        datum: DatumKind,
        scopes: ScopeSetId,
        span: Span,
        origin: Option<Arc<Origin>>,
    ) -> Self {
        Syntax::new(Content::Atom(datum), scopes, span, origin)
    }

    /// `datum` as syntax, with `scopes` on every node and no origin.
    pub fn from_datum(datum: &Datum, scopes: ScopeSetId) -> Self {
        Syntax::from_datum_with(datum, scopes, None, None)
    }

    /// `datum` as syntax with the scopes, span and origin of `context`:
    /// `datum->syntax`.
    pub fn datum_to_syntax(context: &Syntax, datum: &Datum) -> Self {
        Syntax::from_datum_with(
            datum,
            context.scopes,
            Some(context.span),
            context.origin.as_ref(),
        )
    }

    fn from_datum_with(
        datum: &Datum,
        scopes: ScopeSetId,
        span: Option<Span>,
        origin: Option<&Arc<Origin>>,
    ) -> Self {
        let each = |d: &Datum| Syntax::from_datum_with(d, scopes, span, origin);
        let content = match &datum.kind {
            DatumKind::Symbol(name) => Content::Identifier(name.as_str().into()),
            DatumKind::List { items, tail } => Content::List {
                items: items.iter().map(each).collect(),
                tail: tail.as_deref().map(each),
            },
            DatumKind::Vector(items) => Content::Vector(items.iter().map(each).collect()),
            other => Content::Atom(other.clone()),
        };
        Syntax::new(content, scopes, span.unwrap_or(datum.span), origin.cloned())
    }

    /// The datum, without scopes: `syntax->datum`. Pending changes don't
    /// matter to it, so it takes nothing apart.
    pub fn to_datum(&self) -> Datum {
        let kind = match &*self.content {
            Content::Identifier(name) => DatumKind::Symbol(name.to_string()),
            Content::List { items, tail } => {
                let mut items: Vec<Datum> = items.iter().map(Syntax::to_datum).collect();
                let mut tail = tail.as_ref();
                // As in `expose`, a list-shaped tail continues the list.
                while let Some(t) = tail
                    && let Content::List {
                        items: more,
                        tail: rest,
                    } = &*t.content
                {
                    items.extend(more.iter().map(Syntax::to_datum));
                    tail = rest.as_ref();
                }
                DatumKind::List {
                    items,
                    tail: tail.map(|t| Box::new(t.to_datum())),
                }
            }
            Content::Vector(items) => {
                DatumKind::Vector(items.iter().map(Syntax::to_datum).collect())
            }
            Content::Atom(kind) => kind.clone(),
        };
        Datum {
            kind,
            span: self.span,
        }
    }

    /// Its scopes.
    pub fn scopes(&self) -> ScopeSetId {
        self.scopes
    }

    /// Where it was written, or where the code that built it was.
    pub fn span(&self) -> Span {
        self.span
    }

    /// The macro use that produced it, if any.
    pub fn origin(&self) -> Option<&Arc<Origin>> {
        self.origin.as_ref()
    }

    /// Its own node, children and all, with pending changes not applied.
    pub fn node(&self) -> Node<'_> {
        match &*self.content {
            Content::Identifier(name) => Node::Identifier(name),
            Content::List { items, tail } => Node::List {
                items,
                tail: tail.as_ref(),
            },
            Content::Vector(items) => Node::Vector(items),
            Content::Atom(datum) => Node::Atom(datum),
        }
    }

    /// The scope changes waiting to reach its children, sorted by scope.
    pub fn pending(&self) -> &[(Scope, Effect)] {
        self.pending.as_deref().unwrap_or(&[])
    }

    /// A syntax object from its parts, as [`Syntax::node`] and
    /// [`Syntax::pending`] give them: the node as it is, a list's tail not
    /// spliced. `None` if `pending` is not sorted by scope, one change a
    /// scope.
    pub fn from_parts(
        node: NewNode,
        scopes: ScopeSetId,
        pending: Vec<(Scope, Effect)>,
        span: Span,
        origin: Option<Arc<Origin>>,
    ) -> Option<Syntax> {
        if pending.windows(2).any(|w| w[0].0 >= w[1].0) {
            return None;
        }
        let content = match node {
            NewNode::Identifier(name) => Content::Identifier(name),
            NewNode::List { items, tail } => Content::List { items, tail },
            NewNode::Vector(items) => Content::Vector(items),
            NewNode::Atom(datum) => Content::Atom(datum),
        };
        Some(Syntax {
            content: Arc::new(content),
            scopes,
            pending: (!pending.is_empty()).then(|| pending.into()),
            span,
            origin,
        })
    }

    /// The identifier's name, if this is an identifier.
    pub fn as_identifier(&self) -> Option<&str> {
        match &*self.content {
            Content::Identifier(name) => Some(name),
            _ => None,
        }
    }

    /// The datum, if this is neither an identifier, a list nor a vector.
    pub fn as_atom(&self) -> Option<&DatumKind> {
        match &*self.content {
            Content::Atom(kind) => Some(kind),
            _ => None,
        }
    }

    /// Whether this is a list, proper or dotted, or the empty list.
    pub fn is_list(&self) -> bool {
        matches!(&*self.content, Content::List { .. })
    }

    /// For a list, how many elements it has before any `.`, and whether
    /// it has a dotted tail, without taking it apart.
    pub fn list_shape(&self) -> Option<(usize, bool)> {
        let Content::List { items, tail } = &*self.content else {
            return None;
        };
        // A dotted tail that is itself a list continues this one.
        let mut length = items.len();
        let mut tail = tail.as_ref();
        while let Some(t) = tail {
            match &*t.content {
                Content::List { items, tail: rest } => {
                    length += items.len();
                    tail = rest.as_ref();
                }
                _ => return Some((length, true)),
            }
        }
        Some((length, false))
    }

    /// Takes the syntax object apart one level, giving the children their
    /// share of the pending scope changes.
    pub fn expose(&self, context: &mut ScopeContext) -> Result<Exposed<'_>, TooManyScopes> {
        let mut push = |child: &Syntax| self.push_down(context, child);
        Ok(match &*self.content {
            Content::Identifier(name) => Exposed::Identifier(name),
            Content::Atom(kind) => Exposed::Atom(kind),
            Content::List { items, tail } => {
                let mut items: Vec<Syntax> =
                    items.iter().map(&mut push).collect::<Result<_, _>>()?;
                let mut tail = tail.as_ref().map(&mut push).transpose()?;
                // A dotted tail that is itself a list, as a template with
                // `(a . rest)` builds, continues this one: one list, one
                // shape, whatever built it.
                while let Some(t) = tail.take() {
                    if !t.is_list() {
                        tail = Some(t);
                        break;
                    }
                    let Exposed::List {
                        items: more,
                        tail: rest,
                    } = t.expose(context)?
                    else {
                        unreachable!("a list exposes as one")
                    };
                    items.extend(more);
                    tail = rest;
                }
                Exposed::List { items, tail }
            }
            Content::Vector(items) => {
                Exposed::Vector(items.iter().map(push).collect::<Result<_, _>>()?)
            }
        })
    }

    /// The elements of a proper list, if this is one: [`Syntax::expose`]
    /// for the common case.
    pub fn list_items(
        &self,
        context: &mut ScopeContext,
    ) -> Result<Option<Vec<Syntax>>, TooManyScopes> {
        Ok(match self.expose(context)? {
            Exposed::List { items, tail: None } => Some(items),
            _ => None,
        })
    }

    fn push_down(
        &self,
        context: &mut ScopeContext,
        child: &Syntax,
    ) -> Result<Syntax, TooManyScopes> {
        let Some(pending) = &self.pending else {
            return Ok(child.clone());
        };
        let scopes = context.apply_changes(child.scopes, pending)?;
        // A leaf has nothing below it to pend for; a child with nothing
        // pending of its own shares its parent's list.
        let pending = match (&*child.content, &child.pending) {
            (Content::Identifier(_) | Content::Atom(_), _) => None,
            (_, None) => Some(Arc::clone(pending)),
            (_, Some(own)) => compose(Some(own), pending),
        };
        Ok(Syntax {
            content: Arc::clone(&child.content),
            scopes,
            pending,
            span: child.span,
            origin: child.origin.clone(),
        })
    }

    fn change(
        &self,
        context: &mut ScopeContext,
        scope: Scope,
        effect: Effect,
    ) -> Result<Syntax, TooManyScopes> {
        let scopes = effect.apply(context, self.scopes, scope)?;
        let pending = match &*self.content {
            Content::Identifier(_) | Content::Atom(_) => None,
            _ => compose(self.pending.as_ref(), &[(scope, effect)]),
        };
        Ok(Syntax {
            content: Arc::clone(&self.content),
            scopes,
            pending,
            span: self.span,
            origin: self.origin.clone(),
        })
    }

    /// Adds `scope` to this node and, lazily, to every node below it.
    pub fn add(&self, context: &mut ScopeContext, scope: Scope) -> Result<Syntax, TooManyScopes> {
        self.change(context, scope, Effect::Add)
    }

    /// Removes `scope` from this node and, lazily, from every node below it.
    pub fn remove(
        &self,
        context: &mut ScopeContext,
        scope: Scope,
    ) -> Result<Syntax, TooManyScopes> {
        self.change(context, scope, Effect::Remove)
    }

    /// Flips `scope` on this node and, lazily, on every node below it.
    pub fn flip(&self, context: &mut ScopeContext, scope: Scope) -> Result<Syntax, TooManyScopes> {
        self.change(context, scope, Effect::Flip)
    }

    /// This syntax object with another origin, for nodes a macro step
    /// builds from pieces.
    pub fn with_origin(&self, origin: Option<Arc<Origin>>) -> Syntax {
        Syntax {
            origin,
            ..self.clone()
        }
    }

    /// What this identifier refers to at `phase`. Not an identifier: unbound.
    pub fn resolve<'t, B>(
        &self,
        context: &ScopeContext,
        table: &'t BindingTable<String, B>,
        phase: Phase,
    ) -> Resolution<'t, B> {
        match self.as_identifier() {
            Some(name) => table.resolve(context, name, self.scopes, phase),
            None => Resolution::Unbound,
        }
    }
}

/// Whether `a` and `b` are the same identifier with the same scopes:
/// `bound-identifier=?`. A binding for one would bind the other.
pub fn bound_identifier_eq(a: &Syntax, b: &Syntax) -> bool {
    a.as_identifier().is_some() && a.as_identifier() == b.as_identifier() && a.scopes == b.scopes
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
            a.as_identifier().is_some() && a.as_identifier() == b.as_identifier()
        }
        _ => false,
    }
}

/// `origin`'s chain, with its spans moved as `remap` says.
fn remap_origin(remap: &Remap, origin: &Arc<Origin>) -> Arc<Origin> {
    // Iteratively: a chain can be as long as macro steps nest.
    let mut chain = Vec::new();
    let mut at = Some(origin);
    while let Some(o) = at {
        chain.push(o);
        at = o.parent.as_ref();
    }
    let mut parent = None;
    for o in chain.into_iter().rev() {
        parent = Some(Arc::new(Origin {
            macro_name: o.macro_name.clone(),
            use_site: remap.span(o.use_site),
            parent,
        }));
    }
    parent.expect("a chain of at least one")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_compose() {
        use Effect::{Add, Flip, Remove};
        for (a, b, expected) in [
            (Add, Flip, Some(Remove)),
            (Remove, Flip, Some(Add)),
            (Flip, Flip, None),
            (Flip, Add, Some(Add)),
            (Add, Remove, Some(Remove)),
        ] {
            assert_eq!(a.then(b), expected, "{a:?} then {b:?}");
        }
    }
}
