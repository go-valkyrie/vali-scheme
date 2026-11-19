//! Pending scope changes: what a syntax node still owes its children
//! (ADR 0015 #6).
//!
//! The changes to different scopes commute, and the changes to one scope
//! compose into one (two flips cancel), so a list of pending changes is at
//! most one change per scope. It is kept as a chain, the newest scope's
//! change first, whose tails are shared: a node that adds one more scope
//! to what its parent owes makes one link, not a copy of the list, so
//! code nested to a depth *d* holds *d* links in all, not *d*²/2.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::scope::Scope;
use crate::syntax::Effect;

/// A non-empty list of changes, one per scope. Cloning shares it.
#[derive(Clone)]
pub(crate) struct Pending(Arc<Link>);

struct Link {
    scope: Scope,
    effect: Effect,
    /// The changes to older scopes.
    rest: Option<Pending>,
    len: u32,
}

impl Drop for Link {
    /// Drops a long chain without recursing down it.
    fn drop(&mut self) {
        let mut next = self.rest.take();
        while let Some(Pending(link)) = next {
            match Arc::try_unwrap(link) {
                Ok(mut link) => next = link.rest.take(),
                Err(_) => break,
            }
        }
    }
}

impl fmt::Debug for Pending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.ops()).finish()
    }
}

impl Pending {
    fn cons(rest: Option<Pending>, scope: Scope, effect: Effect) -> Pending {
        let len = rest.as_ref().map_or(0, |r| r.0.len) + 1;
        Pending(Arc::new(Link {
            scope,
            effect,
            rest,
            len,
        }))
    }

    /// How many scopes it changes.
    pub(crate) fn len(&self) -> usize {
        self.0.len as usize
    }

    /// Its changes, by increasing scope.
    pub(crate) fn ops(&self) -> Vec<(Scope, Effect)> {
        let mut ops = Vec::with_capacity(self.len());
        let mut at = Some(self);
        while let Some(link) = at {
            ops.push((link.0.scope, link.0.effect));
            at = link.0.rest.as_ref();
        }
        ops.reverse();
        ops
    }

    /// A list of `ops`, which must be sorted by increasing scope, one a
    /// scope.
    pub(crate) fn from_ops(ops: impl IntoIterator<Item = (Scope, Effect)>) -> Option<Pending> {
        ops.into_iter().fold(None, |rest, (scope, effect)| {
            Some(Pending::cons(rest, scope, effect))
        })
    }

    /// `first`, then the change `effect` to `scope`. `None` is no change.
    pub(crate) fn push(first: Option<&Pending>, scope: Scope, effect: Effect) -> Option<Pending> {
        // The links above `scope` have to be made again; usually there are
        // none, since a scope is usually the newest.
        let mut above = Vec::new();
        let mut at = first;
        while let Some(link) = at
            && link.0.scope > scope
        {
            above.push((link.0.scope, link.0.effect));
            at = link.0.rest.as_ref();
        }
        let (base, merged) = match at {
            Some(link) if link.0.scope == scope => {
                (link.0.rest.clone(), link.0.effect.then(effect))
            }
            _ => (at.cloned(), Some(effect)),
        };
        let mut out = base;
        if let Some(effect) = merged {
            out = Some(Pending::cons(out, scope, effect));
        }
        for (scope, effect) in above.into_iter().rev() {
            out = Some(Pending::cons(out, scope, effect));
        }
        out
    }

    /// `first`, then `then`.
    pub(crate) fn compose(first: Option<&Pending>, then: &Pending) -> Option<Pending> {
        let Some(first) = first else {
            return Some(then.clone());
        };
        let mut out = Some(first.clone());
        for (scope, effect) in then.ops() {
            out = Pending::push(out.as_ref(), scope, effect);
        }
        out
    }

    /// An identity for the list while it is alive, for caches.
    pub(crate) fn addr(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    /// The list without its newest scope's change.
    pub(crate) fn rest(&self) -> Option<&Pending> {
        self.0.rest.as_ref()
    }

    /// The change to its newest scope.
    pub(crate) fn newest(&self) -> (Scope, Effect) {
        (self.0.scope, self.0.effect)
    }

    /// The list with every scope mapped by `map`. `memo` remembers the
    /// results for the links it has met, by address, so that lists that
    /// share their tails are mapped once, not once each; it is only good
    /// while the lists it has met are alive.
    pub(crate) fn map_scopes<E>(
        &self,
        memo: &mut HashMap<usize, Option<Pending>>,
        map: &mut impl FnMut(Scope) -> Result<Scope, E>,
    ) -> Result<Option<Pending>, E> {
        let mut stack = Vec::new();
        let mut at = Some(self);
        let mut out = None;
        while let Some(link) = at {
            if let Some(done) = memo.get(&link.addr()) {
                out = done.clone();
                break;
            }
            stack.push(link);
            at = link.rest();
        }
        while let Some(link) = stack.pop() {
            let (scope, effect) = link.newest();
            out = Pending::push(out.as_ref(), map(scope)?, effect);
            memo.insert(link.addr(), out.clone());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{ScopeContext, ScopeKind};

    #[test]
    fn composing_cancels_and_sorts() {
        let mut cx = ScopeContext::new();
        let [a, b, c] = [0; 3].map(|_| cx.new_scope(ScopeKind::Macro).unwrap());
        let flipped = Pending::push(None, b, Effect::Flip);
        let both = Pending::push(flipped.as_ref(), a, Effect::Add);
        assert_eq!(
            both.as_ref().unwrap().ops(),
            [(a, Effect::Add), (b, Effect::Flip)]
        );
        let then = Pending::from_ops([(b, Effect::Flip), (c, Effect::Add)]).unwrap();
        let composed = Pending::compose(both.as_ref(), &then).unwrap();
        assert_eq!(composed.ops(), [(a, Effect::Add), (c, Effect::Add)]);
        assert!(Pending::push(flipped.as_ref(), b, Effect::Flip).is_none());
    }

    #[test]
    fn a_long_chain_drops_without_overflowing() {
        let mut cx = ScopeContext::new();
        let mut chain = None;
        for _ in 0..300_000 {
            let scope = cx.new_scope(ScopeKind::Binder).unwrap();
            chain = Pending::push(chain.as_ref(), scope, Effect::Add);
        }
        assert_eq!(chain.as_ref().unwrap().len(), 300_000);
        drop(chain);
    }
}
