//! Inlining: a direct call to a small function of the same module becomes
//! a copy of its body (ADR 0017 #9).
//!
//! Direct calls go only to the module's own known procedures (see
//! [`crate::known`]), so a body is never copied out of another library
//! (ADR 0012 #7). The copy is spliced in as contification splices a loop,
//! but as a plain join point: `let r = call direct g(a) in k` becomes
//! `join k'(r) = k in join j(p) = body in jump j(a)`, which simplification
//! then reduces to the body with `a` for `p`, its returns jumping to `k'`.
//! A tail call's body returns from the caller instead.
//!
//! A function is inlined when it is small, takes exactly its required
//! arguments, and never calls itself directly, so copying cannot go on
//! forever; each caller takes a bounded number of copies, and stops
//! growing at a bounded size.

use vali_dist::{FunId, Function, Module, Term, Value};

use crate::contify::{Candidate, splice};

/// The most terms a function inlined may have.
const SMALL: usize = 12;

/// The most calls inlined into one function.
const COPIES: usize = 32;

/// The size past which a function takes no more copies.
const LARGEST: usize = 2000;

/// Whether calls to function `g` of `module` may be replaced by its body.
fn inlinable(module: &Module, g: FunId) -> bool {
    let f = &module.functions[g.0 as usize];
    g != module.body
        && f.free.is_empty()
        && f.arity.optional == 0
        && !f.arity.rest
        && f.terms.len() <= SMALL
        && !calls(f, g)
}

/// Whether `f` calls function `g` directly.
fn calls(f: &Function, g: FunId) -> bool {
    f.terms.iter().any(|term| match term {
        Term::Let {
            value: Value::CallDirect { function, .. },
            ..
        }
        | Term::TailCallDirect { function, .. } => *function == g,
        _ => false,
    })
}

/// The first direct call in function `n`'s body to a function that may be
/// inlined there.
fn find(module: &Module, n: usize) -> Option<Candidate> {
    let f = &module.functions[n];
    let mut pending = vec![f.body];
    while let Some(id) = pending.pop() {
        let target = match f.term(id) {
            Term::Let { value, body, .. } => {
                pending.push(*body);
                match value {
                    Value::CallDirect { function, args } => Some((*function, args.len())),
                    _ => None,
                }
            }
            Term::Join { handler, body, .. } => {
                pending.push(*handler);
                pending.push(*body);
                None
            }
            Term::If {
                then, otherwise, ..
            } => {
                pending.push(*otherwise);
                pending.push(*then);
                None
            }
            Term::TailCallDirect { function, args, .. } => Some((*function, args.len())),
            Term::Jump { .. } | Term::Return(_) | Term::TailCall { .. } => None,
        };
        if let Some((g, argc)) = target
            && g.0 as usize != n
            && inlinable(module, g)
            && argc == usize::from(module.functions[g.0 as usize].arity.required)
        {
            return Some(Candidate {
                function: g,
                free: Vec::new(),
                made_at: None,
                ties: Vec::new(),
                call: id,
                recursive: false,
            });
        }
    }
    None
}

/// Inlines the direct calls in function `n` that it can, up to its bounds.
/// Returns whether it inlined any.
pub(crate) fn inline(module: &mut Module, n: usize) -> bool {
    let mut changed = false;
    for _ in 0..COPIES {
        if module.functions[n].terms.len() > LARGEST {
            break;
        }
        let Some(c) = find(module, n) else {
            break;
        };
        splice(module, n, &c);
        changed = true;
    }
    changed
}
