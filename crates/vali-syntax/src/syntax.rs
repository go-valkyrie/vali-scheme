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

use std::collections::HashMap;
use std::sync::Arc;

use crate::binding::{BindingTable, Phase, Resolution};
use crate::pending::Pending;
use crate::scope::{Remap, Scope, ScopeContext, ScopeSetId, TooManyScopes};
use crate::walk::{Split, TreeMap, map_tree};
use crate::{Datum, DatumKind, Origin, Span};

/// A syntax object.
#[derive(Clone)]
pub struct Syntax {
    content: Arc<Content>,
    scopes: ScopeSetId,
    /// Changes to apply to every node below this one, sorted by scope.
    pending: Option<Pending>,
    span: Span,
    origin: Option<Arc<Origin>>,
}

/// Prints at most 200 levels of nesting, and `...` for what is deeper, so
/// that formatting deeply nested syntax cannot overflow the stack.
impl std::fmt::Debug for Syntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        crate::debug::bounded(f, |f| {
            f.debug_struct("Syntax")
                .field("content", &self.content)
                .field("scopes", &self.scopes)
                .field("pending", &self.pending)
                .field("span", &self.span)
                .field("origin", &self.origin)
                .finish()
        })
    }
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

impl Content {
    /// Moves the syntax objects directly inside into `out`.
    fn take_children(&mut self, out: &mut Vec<Syntax>) {
        match self {
            Content::List { items, tail } => {
                out.append(items);
                out.extend(tail.take());
            }
            Content::Vector(items) => out.append(items),
            Content::Identifier(_) | Content::Atom(_) => {}
        }
    }
}

/// Frees the syntax objects inside one at a time, from a list of its own,
/// so that dropping deeply nested syntax cannot overflow the stack.
impl Drop for Content {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        self.take_children(&mut pending);
        while let Some(syntax) = pending.pop() {
            // Only the last holder of a node takes it apart. `content` is
            // left without children, so dropping it recurses no further.
            if let Some(mut content) = Arc::into_inner(syntax.content) {
                content.take_children(&mut pending);
            }
        }
    }
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
    pub(crate) fn then(self, then: Effect) -> Option<Effect> {
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
        map_tree(
            &mut Remapper {
                remap,
                from,
                to,
                open: Vec::new(),
                pending_memo: HashMap::new(),
            },
            self,
        )
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
        let Ok(syntax) = map_tree(
            &mut FromDatum {
                scopes,
                span,
                origin,
            },
            datum,
        );
        syntax
    }

    /// The datum, without scopes: `syntax->datum`. Pending changes don't
    /// matter to it, so it takes nothing apart.
    pub fn to_datum(&self) -> Datum {
        let Ok(datum) = map_tree(&mut ToDatum, self);
        datum
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
    pub fn pending(&self) -> Vec<(Scope, Effect)> {
        self.pending.as_ref().map_or_else(Vec::new, Pending::ops)
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
            pending: Pending::from_ops(pending),
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
        Ok(match &*self.content {
            Content::Identifier(name) => Exposed::Identifier(name),
            Content::Atom(kind) => Exposed::Atom(kind),
            Content::List { items, tail } => {
                let mut items: Vec<Syntax> = items
                    .iter()
                    .map(|child| self.push_down(context, child))
                    .collect::<Result<_, _>>()?;
                let mut tail = tail
                    .as_ref()
                    .map(|child| self.push_down(context, child))
                    .transpose()?;
                // A dotted tail that is itself a list, as a template with
                // `(a . rest)` builds, continues this one: one list, one
                // shape, whatever built it. A loop, not a recursive call,
                // so that a long chain of them cannot overflow the stack.
                while let Some(t) = tail.take() {
                    let Content::List {
                        items: more,
                        tail: rest,
                    } = &*t.content
                    else {
                        tail = Some(t);
                        break;
                    };
                    for child in more {
                        items.push(t.push_down(context, child)?);
                    }
                    tail = rest
                        .as_ref()
                        .map(|child| t.push_down(context, child))
                        .transpose()?;
                }
                Exposed::List { items, tail }
            }
            Content::Vector(items) => Exposed::Vector(
                items
                    .iter()
                    .map(|child| self.push_down(context, child))
                    .collect::<Result<_, _>>()?,
            ),
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
        let scopes = context.apply_pending(child.scopes, pending)?;
        // A leaf has nothing below it to pend for; a child with nothing
        // pending of its own shares its parent's list.
        let pending = match (&*child.content, &child.pending) {
            (Content::Identifier(_) | Content::Atom(_), _) => None,
            (_, None) => Some(pending.clone()),
            (_, Some(own)) => Pending::compose(Some(own), pending),
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
            _ => Pending::push(self.pending.as_ref(), scope, effect),
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

/// `datum` as syntax, for [`Syntax::from_datum_with`].
struct FromDatum<'o> {
    scopes: ScopeSetId,
    span: Option<Span>,
    origin: Option<&'o Arc<Origin>>,
}

impl FromDatum<'_> {
    fn node(&self, content: Content, datum: &Datum) -> Syntax {
        Syntax::new(
            content,
            self.scopes,
            self.span.unwrap_or(datum.span),
            self.origin.cloned(),
        )
    }
}

impl<'a> TreeMap<'a> for FromDatum<'_> {
    type Src = Datum;
    type Out = Syntax;
    type Error = std::convert::Infallible;

    fn split(&mut self, datum: &'a Datum) -> Result<Split<'a, Datum, Syntax>, Self::Error> {
        Ok(match &datum.kind {
            DatumKind::Symbol(name) => {
                Split::Leaf(self.node(Content::Identifier(name.as_str().into()), datum))
            }
            DatumKind::List { items, tail } => Split::List {
                items: items.iter().collect(),
                tail: tail.as_deref(),
            },
            DatumKind::Vector(items) => Split::Vector(items.iter().collect()),
            other => Split::Leaf(self.node(Content::Atom(other.clone()), datum)),
        })
    }

    fn list(
        &mut self,
        datum: &'a Datum,
        items: Vec<Syntax>,
        tail: Option<Syntax>,
    ) -> Result<Syntax, Self::Error> {
        Ok(self.node(Content::List { items, tail }, datum))
    }

    fn vector(&mut self, datum: &'a Datum, items: Vec<Syntax>) -> Result<Syntax, Self::Error> {
        Ok(self.node(Content::Vector(items), datum))
    }
}

/// Syntax as a datum, for [`Syntax::to_datum`].
struct ToDatum;

impl<'a> TreeMap<'a> for ToDatum {
    type Src = Syntax;
    type Out = Datum;
    type Error = std::convert::Infallible;

    fn split(&mut self, syntax: &'a Syntax) -> Result<Split<'a, Syntax, Datum>, Self::Error> {
        let leaf = |kind| {
            Split::Leaf(Datum {
                kind,
                span: syntax.span,
            })
        };
        Ok(match &*syntax.content {
            Content::Identifier(name) => leaf(DatumKind::Symbol(name.to_string())),
            Content::Atom(kind) => leaf(kind.clone()),
            Content::List { items, tail } => {
                let mut items: Vec<&Syntax> = items.iter().collect();
                let mut tail = tail.as_ref();
                // As in `expose`, a list-shaped tail continues the list.
                while let Some(t) = tail
                    && let Content::List {
                        items: more,
                        tail: rest,
                    } = &*t.content
                {
                    items.extend(more.iter());
                    tail = rest.as_ref();
                }
                Split::List { items, tail }
            }
            Content::Vector(items) => Split::Vector(items.iter().collect()),
        })
    }

    fn list(
        &mut self,
        syntax: &'a Syntax,
        items: Vec<Datum>,
        tail: Option<Datum>,
    ) -> Result<Datum, Self::Error> {
        Ok(Datum {
            kind: DatumKind::List {
                items,
                tail: tail.map(Box::new),
            },
            span: syntax.span,
        })
    }

    fn vector(&mut self, syntax: &'a Syntax, items: Vec<Datum>) -> Result<Datum, Self::Error> {
        Ok(Datum {
            kind: DatumKind::Vector(items),
            span: syntax.span,
        })
    }
}

/// What [`Remapper`] works out for a node before its parts.
struct Moved {
    scopes: ScopeSetId,
    pending: Option<Pending>,
    span: Span,
    origin: Option<Arc<Origin>>,
}

/// Syntax made in one scope context, in another, for [`Syntax::remap`].
struct Remapper<'r> {
    remap: &'r mut Remap,
    from: &'r ScopeContext,
    to: &'r mut ScopeContext,
    /// The lists and vectors whose parts are being remapped, innermost
    /// last.
    open: Vec<Moved>,
    /// What each link of a pending list was mapped to, for the lists
    /// that share links.
    pending_memo: HashMap<usize, Option<Pending>>,
}

impl Remapper<'_> {
    fn moved(&mut self, syntax: &Syntax) -> Result<Moved, TooManyScopes> {
        let scopes = self.remap.set(self.from, self.to, syntax.scopes)?;
        let pending = match &syntax.pending {
            None => None,
            Some(changes) => {
                let (remap, from, to) = (&mut *self.remap, self.from, &mut *self.to);
                changes.map_scopes(&mut self.pending_memo, &mut |scope| {
                    remap.scope(from, to, scope)
                })?
            }
        };
        Ok(Moved {
            scopes,
            pending,
            span: self.remap.span(syntax.span),
            origin: if self.remap.moves_sources() {
                syntax.origin.as_ref().map(|o| remap_origin(self.remap, o))
            } else {
                syntax.origin.clone()
            },
        })
    }

    fn built(moved: Moved, content: Content) -> Syntax {
        Syntax {
            content: Arc::new(content),
            scopes: moved.scopes,
            pending: moved.pending,
            span: moved.span,
            origin: moved.origin,
        }
    }
}

impl<'a> TreeMap<'a> for Remapper<'_> {
    type Src = Syntax;
    type Out = Syntax;
    type Error = TooManyScopes;

    fn split(&mut self, syntax: &'a Syntax) -> Result<Split<'a, Syntax, Syntax>, TooManyScopes> {
        let moved = self.moved(syntax)?;
        Ok(match &*syntax.content {
            Content::Identifier(name) => {
                Split::Leaf(Self::built(moved, Content::Identifier(name.clone())))
            }
            Content::Atom(datum) => Split::Leaf(Self::built(moved, Content::Atom(datum.clone()))),
            Content::List { items, tail } => {
                self.open.push(moved);
                Split::List {
                    items: items.iter().collect(),
                    tail: tail.as_ref(),
                }
            }
            Content::Vector(items) => {
                self.open.push(moved);
                Split::Vector(items.iter().collect())
            }
        })
    }

    fn list(
        &mut self,
        _: &'a Syntax,
        items: Vec<Syntax>,
        tail: Option<Syntax>,
    ) -> Result<Syntax, TooManyScopes> {
        let moved = self.open.pop().expect("the list's own changes");
        Ok(Self::built(moved, Content::List { items, tail }))
    }

    fn vector(&mut self, _: &'a Syntax, items: Vec<Syntax>) -> Result<Syntax, TooManyScopes> {
        let moved = self.open.pop().expect("the vector's own changes");
        Ok(Self::built(moved, Content::Vector(items)))
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
