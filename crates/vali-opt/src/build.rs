//! Building a function's terms top-down: each term placed fills the field
//! of an earlier one, its hole, as lowering does.

use vali_dist::{Term, TermId};

/// A term field not filled yet.
pub(crate) const HOLE: TermId = TermId(u32::MAX);

/// Which field of a term a later term fills.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Field {
    /// The body.
    Root,
    LetBody,
    JoinHandler,
    JoinBody,
    Then,
    Otherwise,
}

/// Where a term goes: a field of a term already placed.
pub(crate) type Hole = (TermId, Field);

/// A function's new arena.
#[derive(Default)]
pub(crate) struct Builder {
    pub(crate) terms: Vec<Term>,
    pub(crate) root: Option<TermId>,
}

impl Builder {
    /// Places `term` in `hole`, returning its id.
    pub(crate) fn place(&mut self, hole: Hole, term: Term) -> TermId {
        let id = TermId(self.terms.len() as u32);
        self.terms.push(term);
        match (hole.1, self.terms.get_mut(hole.0.0 as usize)) {
            (Field::Root, _) => self.root = Some(id),
            (Field::LetBody, Some(Term::Let { body, .. }))
            | (Field::JoinBody, Some(Term::Join { body, .. })) => *body = id,
            (Field::JoinHandler, Some(Term::Join { handler, .. })) => *handler = id,
            (Field::Then, Some(Term::If { then, .. })) => *then = id,
            (Field::Otherwise, Some(Term::If { otherwise, .. })) => *otherwise = id,
            _ => unreachable!("holes name fields of the terms they belong to"),
        }
        id
    }
}
