//! Vali's optimiser (ADR 0017 #9): passes over one library's IR, using
//! only that library's own code, so nothing it does can go stale when a
//! dependency changes.
//!
//! Every pass works on one function at a time and rebuilds its terms
//! without recursing, so no input can overflow the stack. A pass never
//! removes or reorders an operation its effects forbid (ADR 0017 #4).
//!
//! - [`simplify`](optimise): constant folding, copy propagation, `if`s on
//!   constants, and removing `let`s whose values are unused and have no
//!   effect beyond allocating or reading, and join points nothing jumps
//!   to. Run to a fixed point, at most a few rounds.

mod build;
mod simplify;

use vali_dist::{ConstantId, Module, Operand, Term, Value};

/// How many rounds of simplification a function gets at most: each round
/// can expose more, but few programs need more than two.
const ROUNDS: usize = 4;

/// Optimises `module` in place. It stays valid.
pub fn optimise(module: &mut Module) {
    for n in 0..module.functions.len() {
        for _ in 0..ROUNDS {
            if !simplify::simplify(module, n) {
                break;
            }
        }
    }
    compact_constants(module);
    debug_assert!(
        vali_dist::validate(module).is_ok(),
        "the optimiser produced invalid IR: {:?}",
        vali_dist::validate(module)
    );
}

/// Drops the constants no code uses, which folding leaves behind, keeping
/// the others in order.
fn compact_constants(module: &mut Module) {
    let mut used = vec![false; module.constants.len()];
    for_each_operand(module, |x| {
        if let Operand::Const(c) = *x {
            used[c.0 as usize] = true;
        }
    });
    if used.iter().all(|&u| u) {
        return;
    }
    let mut renumber = vec![0u32; used.len()];
    let mut next = 0;
    for (n, &u) in used.iter().enumerate() {
        renumber[n] = next;
        next += u32::from(u);
    }
    let mut n = 0;
    module.constants.retain(|_| {
        n += 1;
        used[n - 1]
    });
    for_each_operand(module, |x| {
        if let Operand::Const(c) = x {
            *c = ConstantId(renumber[c.0 as usize]);
        }
    });
}

/// Calls `visit` on every operand in `module`.
fn for_each_operand(module: &mut Module, mut visit: impl FnMut(&mut Operand)) {
    for f in &mut module.functions {
        for term in &mut f.terms {
            match term {
                Term::Let { value, .. } => match value {
                    Value::Operand(x)
                    | Value::Box(x)
                    | Value::Unbox(x)
                    | Value::SetGlobal(_, x) => visit(x),
                    Value::Primitive { args, .. } => args.iter_mut().for_each(&mut visit),
                    Value::Call { callee, args } => {
                        visit(callee);
                        args.iter_mut().for_each(&mut visit);
                    }
                    Value::Closure { free, .. } => free.iter_mut().for_each(&mut visit),
                    Value::Global(_) => {}
                    Value::SetBox(a, b) => {
                        visit(a);
                        visit(b);
                    }
                    Value::SetFree { closure, value, .. } => {
                        visit(closure);
                        visit(value);
                    }
                },
                Term::Join { .. } => {}
                Term::Jump { args, .. } => args.iter_mut().for_each(&mut visit),
                Term::If { test, .. } | Term::Return(test) => visit(test),
                Term::TailCall { callee, args, .. } => {
                    visit(callee);
                    args.iter_mut().for_each(&mut visit);
                }
            }
        }
    }
}
