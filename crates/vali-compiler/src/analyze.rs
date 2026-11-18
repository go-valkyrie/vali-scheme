//! Which `letrec` procedures compile as loops.
//!
//! A procedure bound by `letrec` is a loop when it is never assigned, takes
//! no rest argument, and every reference to it is a call with the right
//! number of arguments: either from its own body in tail position, or one
//! call from the frame that binds it. Such a procedure needs no closure: its
//! body is compiled once, at that one call, inside the binding frame, and
//! its own tail calls become jumps. Named `let` loops are the common case.

use std::collections::{HashMap, HashSet};

use crate::ast::{Expr, Lambda, LambdaId, VarId, VarInfo};
use crate::expand::{RED_ZONE, STACK_GROWTH};

struct Reference {
    /// The arguments, if the reference is the operator of a call.
    call: Option<usize>,
    /// The `lambda` whose body the reference is in.
    lambda: LambdaId,
    /// Whether it is in tail position in that body.
    tail: bool,
}

struct Walker<'a> {
    lambdas: &'a [Lambda],
    references: HashMap<VarId, Vec<Reference>>,
    candidates: Vec<(VarId, LambdaId)>,
}

impl Walker<'_> {
    fn walk(&mut self, e: &Expr, lambda: LambdaId, tail: bool) {
        stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || self.walk_here(e, lambda, tail));
    }

    fn walk_here(&mut self, e: &Expr, lambda: LambdaId, tail: bool) {
        match e {
            Expr::Unspecified
            | Expr::Const(_)
            | Expr::Global(_)
            | Expr::Free(_)
            | Expr::ReplGlobal(_)
            | Expr::Imported(..) => {}
            Expr::Local(v) => self.reference(*v, None, lambda, tail),
            Expr::SetLocal(_, value) | Expr::SetGlobal(_, value) | Expr::SetFree(_, value) => {
                self.walk(value, lambda, false)
            }
            Expr::If(c, t, f) => {
                self.walk(c, lambda, false);
                self.walk(t, lambda, tail);
                self.walk(f, lambda, tail);
            }
            Expr::Begin(es) => {
                for (i, e) in es.iter().enumerate() {
                    self.walk(e, lambda, tail && i + 1 == es.len());
                }
            }
            Expr::Lambda(l) => self.walk(&self.lambdas[*l].body, *l, true),
            Expr::Let(bindings, body) => {
                for (_, init) in bindings {
                    self.walk(init, lambda, false);
                }
                self.walk(body, lambda, tail);
            }
            Expr::Letrec(bindings, body) => {
                for (v, init) in bindings {
                    if let Expr::Lambda(l) = init {
                        self.candidates.push((*v, *l));
                    }
                    self.walk(init, lambda, false);
                }
                self.walk(body, lambda, tail);
            }
            Expr::Call(f, args, _) => {
                match &**f {
                    Expr::Local(v) => self.reference(*v, Some(args.len()), lambda, tail),
                    f => self.walk(f, lambda, false),
                }
                for a in args {
                    self.walk(a, lambda, false);
                }
            }
        }
    }

    fn reference(&mut self, v: VarId, call: Option<usize>, lambda: LambdaId, tail: bool) {
        self.references
            .entry(v)
            .or_default()
            .push(Reference { call, lambda, tail });
    }
}

/// The `letrec` variables whose procedures compile as loops.
pub(crate) fn loops(vars: &[VarInfo], lambdas: &[Lambda], program: LambdaId) -> HashSet<VarId> {
    let mut walker = Walker {
        lambdas,
        references: HashMap::new(),
        candidates: Vec::new(),
    };
    walker.walk(&lambdas[program].body, program, true);
    walker
        .candidates
        .iter()
        .filter(|&&(v, l)| {
            let lambda = &lambdas[l];
            if vars[v].assigned || lambda.rest.is_some() {
                return false;
            }
            let arity = lambda.required.len();
            let binder = vars[v].binder;
            let references = walker.references.get(&v).map_or(&[][..], |r| &r[..]);
            let entries = references.iter().filter(|r| r.lambda == binder).count();
            entries == 1
                && references.iter().all(|r| {
                    r.call == Some(arity) && (r.lambda == binder || (r.lambda == l && r.tail))
                })
        })
        .map(|&(v, _)| v)
        .collect()
}
