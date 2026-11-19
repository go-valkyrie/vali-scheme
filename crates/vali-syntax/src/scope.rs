//! Scopes and interned scope sets (ADR 0015).
//!
//! A [`ScopeContext`] numbers scopes and scope sets for one expansion, from
//! zero, so the numbers depend only on what is being expanded. Scope sets
//! are interned: equal sets get the same [`ScopeSetId`], so comparing two
//! sets compares two integers. Adding, removing and flipping a scope are
//! memoised.
//!
//! A set is stored as its parent, the set without its largest scope, and
//! that scope, so nested code, whose sets each hold one scope more than the
//! set around them, stores one entry a level, not one scope per scope held.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::pending::Pending;

/// A scope: an opaque identity, numbered within its [`ScopeContext`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Scope(u32);

impl Scope {
    /// The scope's number within its context.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// What made a scope. Only tools look at it: resolution never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    /// Flipped on a macro's input and output, for one macro step.
    Macro,
    /// Added to a macro's input when it is used in a definition context.
    UseSite,
    /// Added by a binding form, such as `lambda` or `let`, to its body.
    Binder,
    /// Added to a library's body.
    Library,
    /// Anything else, such as a scope a host creates.
    Other,
}

/// An interned set of scopes, numbered within its [`ScopeContext`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeSetId(u32);

impl ScopeSetId {
    /// The empty set, which every context interns first.
    pub const EMPTY: ScopeSetId = ScopeSetId(0);

    /// The set's number within its context.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// A context ran out of numbers for scopes or scope sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooManyScopes {
    /// The limit that was reached.
    pub limit: u32,
}

impl fmt::Display for TooManyScopes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "too many scopes in one expansion (the limit is {})",
            self.limit
        )
    }
}

impl std::error::Error for TooManyScopes {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Op {
    Add,
    Remove,
    Flip,
}

/// One interned set: the set without its largest scope, and that scope.
#[derive(Clone, Copy, Debug)]
struct SetNode {
    parent: ScopeSetId,
    /// The set's largest scope. Meaningless in the empty set.
    newest: Scope,
    /// How many scopes the set holds.
    len: u32,
    /// An ancestor chosen so that any ancestor is found in a number of
    /// steps logarithmic in the depth (skew-binary jump pointers).
    jump: ScopeSetId,
}

/// The fewest changes in a list that is remembered once applied.
const LONG: usize = 8;

/// A list of changes applied to a set: the result, and the list, to keep
/// its address its own.
type Applied = (ScopeSetId, Pending);

/// Numbers scopes and interns scope sets for one expansion.
///
/// Two caches speed up the work: the result of each single change, and the
/// result of applying each long list of pending changes (one link at a
/// time, so lists that share their tails share the work). They are this
/// context's own and grow with the work done, so a context that lives long
/// needs [`ScopeContext::clear_caches`], called now and then. A clone
/// starts with empty ones, so a finished context that is kept for its sets
/// does not keep them too.
#[derive(Debug)]
pub struct ScopeContext {
    limit: u32,
    kinds: Vec<ScopeKind>,
    /// The sets, by number. The first is the empty one.
    nodes: Vec<SetNode>,
    /// Each set by its parent and its largest scope.
    interned: HashMap<(ScopeSetId, Scope), ScopeSetId>,
    memo: HashMap<(ScopeSetId, Scope, Op), ScopeSetId>,
    /// Lists of pending changes applied, by the set they were applied to
    /// and the list's address. The list is kept, so its address stays its
    /// own.
    applied: HashMap<(ScopeSetId, usize), Applied>,
}

impl Clone for ScopeContext {
    /// A copy of the scopes and sets, with empty caches.
    fn clone(&self) -> Self {
        ScopeContext {
            limit: self.limit,
            kinds: self.kinds.clone(),
            nodes: self.nodes.clone(),
            interned: self.interned.clone(),
            memo: HashMap::new(),
            applied: HashMap::new(),
        }
    }
}

impl Default for ScopeContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeContext {
    /// A context that can number up to `u32::MAX` scopes and as many sets.
    pub fn new() -> Self {
        Self::with_limit(u32::MAX)
    }

    /// A context that can number up to `limit` scopes and `limit` sets
    /// (at least one, since the empty set is always there). Hosts can use
    /// a lower limit to bound the work one expansion does.
    pub fn with_limit(limit: u32) -> Self {
        ScopeContext {
            limit: limit.max(1),
            kinds: Vec::new(),
            nodes: vec![SetNode {
                parent: ScopeSetId::EMPTY,
                newest: Scope(0),
                len: 0,
                jump: ScopeSetId::EMPTY,
            }],
            interned: HashMap::new(),
            memo: HashMap::new(),
            applied: HashMap::new(),
        }
    }

    /// Drops the remembered results of changes, which are only there to
    /// save time. What the context numbers and interns is untouched.
    pub fn clear_caches(&mut self) {
        self.memo = HashMap::new();
        self.applied = HashMap::new();
    }

    /// How many entries the caches hold: remembered lists of changes and
    /// results of single changes. For tests that the caches stay in check.
    #[doc(hidden)]
    pub fn cache_len(&self) -> usize {
        self.applied.len() + self.memo.len()
    }

    /// A fresh scope.
    pub fn new_scope(&mut self, kind: ScopeKind) -> Result<Scope, TooManyScopes> {
        let index = self.next_index(self.kinds.len())?;
        self.kinds.push(kind);
        Ok(Scope(index))
    }

    /// What made `scope`.
    pub fn kind(&self, scope: Scope) -> ScopeKind {
        self.kinds[scope.0 as usize]
    }

    /// How many scopes this context has made.
    pub fn scope_count(&self) -> usize {
        self.kinds.len()
    }

    /// How many distinct sets this context has interned, the empty set
    /// included.
    pub fn set_count(&self) -> usize {
        self.nodes.len()
    }

    /// Every scope this context has made, in the order it made them.
    pub fn all_scopes(&self) -> impl Iterator<Item = Scope> {
        (0..self.kinds.len() as u32).map(Scope)
    }

    /// Every set this context has interned, in the order it interned them:
    /// each after its parent.
    pub fn all_sets(&self) -> impl Iterator<Item = ScopeSetId> {
        (0..self.nodes.len() as u32).map(ScopeSetId)
    }

    fn node(&self, set: ScopeSetId) -> &SetNode {
        &self.nodes[set.0 as usize]
    }

    /// The scopes in `set`, in increasing order. This walks the whole set:
    /// code that can work from [`ScopeContext::parent`] and
    /// [`ScopeContext::newest`] a set at a time should.
    pub fn scopes(&self, set: ScopeSetId) -> Vec<Scope> {
        let mut scopes = Vec::with_capacity(self.len(set));
        let mut at = set;
        while at != ScopeSetId::EMPTY {
            let node = self.node(at);
            scopes.push(node.newest);
            at = node.parent;
        }
        scopes.reverse();
        scopes
    }

    /// How many scopes `set` holds.
    pub fn len(&self, set: ScopeSetId) -> usize {
        self.node(set).len as usize
    }

    /// The largest scope of `set`, if it has any.
    pub fn newest(&self, set: ScopeSetId) -> Option<Scope> {
        (set != ScopeSetId::EMPTY).then(|| self.node(set).newest)
    }

    /// `set` without its largest scope, which is another interned set: none
    /// for the empty set. Resolution uses it to build on what it found for
    /// the smaller set.
    pub fn parent(&self, set: ScopeSetId) -> Option<ScopeSetId> {
        (set != ScopeSetId::EMPTY).then(|| self.node(set).parent)
    }

    /// The set at or above `set` (the nearest ancestor, `set` itself
    /// included) whose largest scope is no larger than `scope`, or the
    /// empty set.
    fn skip_above(&self, set: ScopeSetId, scope: Scope) -> ScopeSetId {
        let mut at = set;
        while at != ScopeSetId::EMPTY && self.node(at).newest > scope {
            let node = self.node(at);
            let jump = node.jump;
            at = if jump != ScopeSetId::EMPTY && self.node(jump).newest > scope {
                jump
            } else {
                node.parent
            };
        }
        at
    }

    /// Whether `set` contains `scope`.
    pub fn contains(&self, set: ScopeSetId, scope: Scope) -> bool {
        let at = self.skip_above(set, scope);
        at != ScopeSetId::EMPTY && self.node(at).newest == scope
    }

    /// Whether every scope in `a` is in `b`.
    pub fn is_subset(&self, a: ScopeSetId, b: ScopeSetId) -> bool {
        let (mut x, mut y) = (a, b);
        loop {
            if x == y || x == ScopeSetId::EMPTY {
                return true;
            }
            if y == ScopeSetId::EMPTY || self.node(x).len > self.node(y).len {
                return false;
            }
            let (sx, sy) = (self.node(x).newest, self.node(y).newest);
            if sx > sy {
                return false;
            }
            if sx == sy {
                x = self.node(x).parent;
                y = self.node(y).parent;
            } else {
                // `b` has scopes above `a`'s largest: skip to where they
                // end, which is `a` itself when it is an ancestor of `b`.
                y = self.skip_above(y, sx);
            }
        }
    }

    /// The set of `scopes`, interned.
    pub fn intern(&mut self, scopes: &[Scope]) -> Result<ScopeSetId, TooManyScopes> {
        let mut sorted = scopes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let mut set = ScopeSetId::EMPTY;
        for scope in sorted {
            set = self.child(set, scope)?;
        }
        Ok(set)
    }

    /// `set` with `scope` added.
    pub fn add(&mut self, set: ScopeSetId, scope: Scope) -> Result<ScopeSetId, TooManyScopes> {
        self.apply(set, scope, Op::Add)
    }

    /// `set` with `scope` removed.
    pub fn remove(&mut self, set: ScopeSetId, scope: Scope) -> Result<ScopeSetId, TooManyScopes> {
        self.apply(set, scope, Op::Remove)
    }

    /// `set` with `scope` added if it was absent, and removed if it was
    /// present.
    pub fn flip(&mut self, set: ScopeSetId, scope: Scope) -> Result<ScopeSetId, TooManyScopes> {
        self.apply(set, scope, Op::Flip)
    }

    /// `set` with every change in `pending` made. A long list, which is
    /// what a deeply nested form carries to each of its children, is
    /// applied a link at a time and each step remembered, so a list that
    /// extends another one starts from the other's result, and the
    /// children that share a list share the work.
    pub(crate) fn apply_pending(
        &mut self,
        set: ScopeSetId,
        pending: &Pending,
    ) -> Result<ScopeSetId, TooManyScopes> {
        if pending.len() < LONG {
            let mut set = set;
            for (scope, effect) in pending.ops() {
                set = effect.apply(self, set, scope)?;
            }
            return Ok(set);
        }
        let mut stack = Vec::new();
        let mut at = Some(pending);
        let mut result = set;
        while let Some(link) = at {
            if let Some((done, _)) = self.applied.get(&(set, link.addr())) {
                result = *done;
                break;
            }
            stack.push(link);
            at = link.rest();
        }
        while let Some(link) = stack.pop() {
            let (scope, effect) = link.newest();
            result = effect.apply(self, result, scope)?;
            self.applied
                .insert((set, link.addr()), (result, link.clone()));
        }
        Ok(result)
    }

    fn apply(
        &mut self,
        set: ScopeSetId,
        scope: Scope,
        op: Op,
    ) -> Result<ScopeSetId, TooManyScopes> {
        // Adding a scope above the set's own is what nested code does, and
        // is one lookup.
        if op != Op::Remove && self.newest(set).is_none_or(|n| n < scope) {
            return self.child(set, scope);
        }
        if let Some(&result) = self.memo.get(&(set, scope, op)) {
            return Ok(result);
        }
        let at = self.skip_above(set, scope);
        let present = at != ScopeSetId::EMPTY && self.node(at).newest == scope;
        let result = match (present, op) {
            (true, Op::Add) | (false, Op::Remove) => set,
            (false, Op::Add | Op::Flip) => {
                let above = self.above(set, at);
                let base = self.child(at, scope)?;
                self.push_all(base, above)?
            }
            (true, Op::Remove | Op::Flip) => {
                let above = self.above(set, at);
                let base = self.node(at).parent;
                self.push_all(base, above)?
            }
        };
        self.memo.insert((set, scope, op), result);
        Ok(result)
    }

    /// The scopes of `set` that `ancestor` lacks, by increasing scope.
    fn above(&self, set: ScopeSetId, ancestor: ScopeSetId) -> Vec<Scope> {
        let mut scopes = Vec::new();
        let mut at = set;
        while at != ancestor {
            let node = self.node(at);
            scopes.push(node.newest);
            at = node.parent;
        }
        scopes.reverse();
        scopes
    }

    /// `base` with each of `scopes`, which are increasing and above all of
    /// its own, added.
    fn push_all(
        &mut self,
        mut base: ScopeSetId,
        scopes: Vec<Scope>,
    ) -> Result<ScopeSetId, TooManyScopes> {
        for scope in scopes {
            base = self.child(base, scope)?;
        }
        Ok(base)
    }

    /// The set of `parent` with `scope`, which is above all of its own,
    /// interned.
    fn child(&mut self, parent: ScopeSetId, scope: Scope) -> Result<ScopeSetId, TooManyScopes> {
        if let Some(&id) = self.interned.get(&(parent, scope)) {
            return Ok(id);
        }
        debug_assert!(self.newest(parent).is_none_or(|n| n < scope));
        let id = ScopeSetId(self.next_index(self.nodes.len())?);
        let p = *self.node(parent);
        // The jump pointer of a node is its parent's own jump pointer's
        // when the two steps are equal, and the parent otherwise: that is
        // what makes the jumps skew-binary.
        let jump = if parent != ScopeSetId::EMPTY
            && p.len - self.node(p.jump).len
                == self.node(p.jump).len - self.node(self.node(p.jump).jump).len
        {
            self.node(p.jump).jump
        } else {
            parent
        };
        self.nodes.push(SetNode {
            parent,
            newest: scope,
            len: p.len + 1,
            jump,
        });
        self.interned.insert((parent, scope), id);
        Ok(id)
    }

    fn next_index(&self, count: usize) -> Result<u32, TooManyScopes> {
        u32::try_from(count)
            .ok()
            .filter(|&n| n < self.limit)
            .ok_or(TooManyScopes { limit: self.limit })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_sets_are_interned_once() {
        let mut cx = ScopeContext::new();
        let [a, b] = [0; 2].map(|_| cx.new_scope(ScopeKind::Other).unwrap());
        let ab = cx.intern(&[b, a, b]).unwrap();
        assert_eq!(cx.scopes(ab), &[a, b]);
        let a_then_b = cx.add(ScopeSetId::EMPTY, a).unwrap();
        let a_then_b = cx.add(a_then_b, b).unwrap();
        assert_eq!(a_then_b, ab);
        assert_eq!(cx.intern(&[]).unwrap(), ScopeSetId::EMPTY);
        assert_eq!(cx.set_count(), 3);
    }

    #[test]
    fn add_remove_and_flip() {
        let mut cx = ScopeContext::new();
        let [a, b] = [0; 2].map(|_| cx.new_scope(ScopeKind::Macro).unwrap());
        let s = cx.intern(&[a]).unwrap();
        assert_eq!(cx.add(s, a).unwrap(), s);
        assert_eq!(cx.remove(s, b).unwrap(), s);
        let flipped = cx.flip(s, b).unwrap();
        assert_eq!(cx.scopes(flipped), &[a, b]);
        assert_eq!(cx.flip(flipped, b).unwrap(), s);
        assert_eq!(cx.remove(flipped, a).unwrap(), cx.intern(&[b]).unwrap());
        // Memoised: the same operation finds the same set without interning.
        let count = cx.set_count();
        assert_eq!(cx.flip(s, b).unwrap(), flipped);
        assert_eq!(cx.set_count(), count);
    }

    #[test]
    fn subsets() {
        let mut cx = ScopeContext::new();
        let [a, b, c] = [0; 3].map(|_| cx.new_scope(ScopeKind::Binder).unwrap());
        let ac = cx.intern(&[a, c]).unwrap();
        let abc = cx.intern(&[a, b, c]).unwrap();
        let bc = cx.intern(&[b, c]).unwrap();
        assert!(cx.is_subset(ac, abc));
        assert!(cx.is_subset(ScopeSetId::EMPTY, ac));
        assert!(cx.is_subset(ac, ac));
        assert!(!cx.is_subset(abc, ac));
        assert!(!cx.is_subset(ac, bc));
        assert!(cx.contains(ac, c) && !cx.contains(ac, b));
        assert_eq!(cx.kind(b), ScopeKind::Binder);
    }

    #[test]
    fn running_out_is_an_error() {
        let mut cx = ScopeContext::with_limit(3);
        let scopes: Vec<_> = (0..3)
            .map(|_| cx.new_scope(ScopeKind::Other).unwrap())
            .collect();
        let error = cx.new_scope(ScopeKind::Other).unwrap_err();
        assert_eq!(error, TooManyScopes { limit: 3 });
        assert_eq!(
            error.to_string(),
            "too many scopes in one expansion (the limit is 3)"
        );
        // Sets run out too: the empty set, {0} and {1} fill three slots.
        let one = cx.add(ScopeSetId::EMPTY, scopes[0]).unwrap();
        cx.add(ScopeSetId::EMPTY, scopes[1]).unwrap();
        assert!(cx.add(one, scopes[1]).is_err());
        assert_eq!(cx.scope_count(), 3);
    }

    #[test]
    fn nested_sets_take_a_node_each() {
        let mut cx = ScopeContext::new();
        let mut set = ScopeSetId::EMPTY;
        let mut sets = vec![set];
        for _ in 0..10_000 {
            let scope = cx.new_scope(ScopeKind::Binder).unwrap();
            set = cx.add(set, scope).unwrap();
            sets.push(set);
        }
        assert_eq!(cx.set_count(), 10_001);
        assert_eq!(cx.len(set), 10_000);
        // Any ancestor is a subset, and found in a few steps.
        for &ancestor in sets.iter().step_by(997) {
            assert!(cx.is_subset(ancestor, set));
            assert!(!cx.is_subset(set, ancestor) || ancestor == set);
        }
        let middle = cx.scopes(sets[5_000])[4_999];
        assert!(cx.contains(set, middle));
        assert!(cx.contains(sets[5_000], middle));
        assert!(!cx.contains(sets[4_000], middle));
    }
}

/// Maps the scopes of one context into another, each to a fresh scope of
/// the same kind the first time it is met, so syntax made in one
/// expansion context can be used in another (ADR 0015 #5): a library's
/// macros in an importer's context, or stored syntax as it loads.
#[derive(Debug, Default)]
pub struct Remap {
    scopes: HashMap<Scope, Scope>,
    sets: HashMap<ScopeSetId, ScopeSetId>,
    /// The sets all of whose scopes have been met.
    met: HashSet<ScopeSetId>,
    /// Where spans' sources are in the target, when they move too: syntax
    /// merged with its sources into another table (`Sources::absorb`).
    sources: HashMap<crate::SourceId, crate::SourceId>,
}

impl Remap {
    /// A remapping that has met no scope yet.
    pub fn new() -> Remap {
        Remap::default()
    }

    /// Maps `from` to `to`, an existing scope, rather than a fresh one: the
    /// scope of the core forms, which every context has.
    pub fn identify(&mut self, from: Scope, to: Scope) {
        self.scopes.insert(from, to);
    }

    /// The scope `scope` of `from` maps to in `to`.
    pub fn scope(
        &mut self,
        from: &ScopeContext,
        to: &mut ScopeContext,
        scope: Scope,
    ) -> Result<Scope, TooManyScopes> {
        if let Some(&mapped) = self.scopes.get(&scope) {
            return Ok(mapped);
        }
        let mapped = to.new_scope(from.kind(scope))?;
        self.scopes.insert(scope, mapped);
        Ok(mapped)
    }

    /// The set `set` of `from` maps to in `to`. A set is mapped as its
    /// parent is, with its newest scope added, so the scopes are met in
    /// increasing order and a nest of sets costs a step each.
    pub fn set(
        &mut self,
        from: &ScopeContext,
        to: &mut ScopeContext,
        set: ScopeSetId,
    ) -> Result<ScopeSetId, TooManyScopes> {
        let mut chain = Vec::new();
        let mut at = set;
        let mut mapped = loop {
            if at == ScopeSetId::EMPTY {
                break ScopeSetId::EMPTY;
            }
            if let Some(&done) = self.sets.get(&at) {
                break done;
            }
            chain.push(at);
            at = from.parent(at).expect("not the empty set");
        };
        while let Some(next) = chain.pop() {
            let newest = from.newest(next).expect("not the empty set");
            let scope = self.scope(from, to, newest)?;
            mapped = to.add(mapped, scope)?;
            self.sets.insert(next, mapped);
        }
        Ok(mapped)
    }

    /// Moves spans to the sources `map` says, as syntax is remapped.
    pub fn move_sources(&mut self, map: HashMap<crate::SourceId, crate::SourceId>) {
        self.sources = map;
    }

    /// `span`, in the sources syntax is remapped into.
    pub fn span(&self, span: crate::Span) -> crate::Span {
        match self.sources.get(&span.source) {
            Some(&source) => crate::Span { source, ..span },
            None => span,
        }
    }

    /// Whether spans move to other sources.
    pub fn moves_sources(&self) -> bool {
        !self.sources.is_empty()
    }

    /// Whether every scope of `set` has been met: only then can an
    /// identifier mapped so far have a set that holds it.
    pub fn has_met_all(&mut self, from: &ScopeContext, set: ScopeSetId) -> bool {
        let mut chain = Vec::new();
        let mut at = set;
        while at != ScopeSetId::EMPTY && !self.met.contains(&at) {
            chain.push(at);
            at = from.parent(at).expect("not the empty set");
        }
        // The sets whose newest scope has been met, up from the first that
        // has not, are not all met; the ones below it are.
        let mut all = true;
        while let Some(next) = chain.pop() {
            all = all
                && from
                    .newest(next)
                    .is_some_and(|s| self.scopes.contains_key(&s));
            if all {
                self.met.insert(next);
            }
        }
        all
    }
}

#[cfg(test)]
mod fast_paths {
    use super::*;
    use crate::syntax::Effect;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn random_set(cx: &mut ScopeContext, rng: &mut Rng, all: &[Scope], max: u64) -> ScopeSetId {
        let n = rng.below(max + 1) as usize;
        let mut v: Vec<Scope> = (0..n)
            .map(|_| all[rng.below(all.len() as u64) as usize])
            .collect();
        v.sort();
        v.dedup();
        cx.intern(&v).unwrap()
    }

    #[test]
    fn apply_pending_matches_one_at_a_time() {
        let mut rng = Rng(0x9e3779b97f4a7c15);
        let mut cx = ScopeContext::new();
        let all: Vec<Scope> = (0..400)
            .map(|_| cx.new_scope(ScopeKind::Binder).unwrap())
            .collect();
        for round in 0..5_000 {
            let set = random_set(&mut cx, &mut rng, &all, 200);
            let len = 1 + rng.below(80) as usize;
            let mut scopes: Vec<Scope> = (0..len)
                .map(|_| all[rng.below(all.len() as u64) as usize])
                .collect();
            scopes.sort();
            scopes.dedup();
            let changes: Vec<(Scope, Effect)> = scopes
                .into_iter()
                .map(|s| {
                    (
                        s,
                        [Effect::Add, Effect::Remove, Effect::Flip][rng.below(3) as usize],
                    )
                })
                .collect();
            let pending = Pending::from_ops(changes.iter().copied()).unwrap();
            let mut expected = set;
            for &(s, e) in changes.iter() {
                expected = e.apply(&mut cx, expected, s).unwrap();
            }
            assert_eq!(
                cx.apply_pending(set, &pending).unwrap(),
                expected,
                "round {round}"
            );
            // And again, from the memo.
            assert_eq!(
                cx.apply_pending(set, &pending).unwrap(),
                expected,
                "memo, round {round}"
            );
        }
    }

    #[test]
    fn set_operations_match_naive() {
        let mut rng = Rng(0x2545f4914f6cdd1d);
        let mut cx = ScopeContext::new();
        let all: Vec<Scope> = (0..300)
            .map(|_| cx.new_scope(ScopeKind::Binder).unwrap())
            .collect();
        let mut subsets = 0;
        for round in 0..20_000 {
            let b = random_set(&mut cx, &mut rng, &all, 200);
            // Half the time a is drawn from b (often a subset), else at
            // random.
            let a = if rng.below(2) == 0 {
                let sb = cx.scopes(b);
                let mut v: Vec<Scope> = sb.into_iter().filter(|_| rng.below(8) != 0).collect();
                if rng.below(4) == 0 {
                    v.push(all[rng.below(all.len() as u64) as usize]);
                }
                cx.intern(&v).unwrap()
            } else {
                random_set(&mut cx, &mut rng, &all, 200)
            };
            let (sa, sb) = (cx.scopes(a), cx.scopes(b));
            let naive = sa.iter().all(|s| sb.contains(s));
            subsets += usize::from(naive);
            assert_eq!(cx.is_subset(a, b), naive, "round {round}");
            let probe = all[rng.below(all.len() as u64) as usize];
            assert_eq!(
                cx.contains(b, probe),
                sb.contains(&probe),
                "contains {round}"
            );
            let with = cx.add(b, probe).unwrap();
            let mut expected = sb.clone();
            expected.push(probe);
            expected.sort();
            expected.dedup();
            assert_eq!(cx.scopes(with), expected, "add {round}");
            let without = cx.remove(b, probe).unwrap();
            let expected: Vec<Scope> = sb.iter().copied().filter(|&s| s != probe).collect();
            assert_eq!(cx.scopes(without), expected, "remove {round}");
            assert_eq!(cx.intern(&expected).unwrap(), without, "interned {round}");
        }
        assert!(
            subsets > 1000,
            "{subsets} subsets: the true case is barely exercised"
        );
    }

    #[test]
    fn a_clone_starts_with_empty_caches() {
        let mut cx = ScopeContext::new();
        let all: Vec<Scope> = (0..20)
            .map(|_| cx.new_scope(ScopeKind::Binder).unwrap())
            .collect();
        let set = cx.intern(&all[..10]).unwrap();
        let low = cx.add(set, all[15]).unwrap();
        cx.flip(low, all[3]).unwrap();
        assert!(cx.cache_len() > 0);
        let clone = cx.clone();
        assert_eq!(clone.cache_len(), 0);
        assert_eq!(clone.scopes(low), cx.scopes(low));
        cx.clear_caches();
        assert_eq!(cx.cache_len(), 0);
    }
}
