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
//! - [`unbox`](optimise): assigned variables that need no box become
//!   values again, threaded through join points (ADR 0017 #5). Runs after
//!   contification, using what the module's own code shows about which
//!   calls cannot capture a continuation.
//! - [`known`](optimise): calls to the module's own procedures, through
//!   globals nothing assigns again, become direct calls (ADR 0017 #5).

mod build;
mod contify;
mod known;
mod simplify;
mod unbox;

use vali_dist::{ConstantId, FunId, Module, Operand, Term, Value};

/// The most closures contified in one function: each makes the function
/// bigger, and a long chain of them is rare.
const CONTIFICATIONS: usize = 256;

/// What a value reads.
pub(crate) fn operands(value: &Value) -> Vec<Operand> {
    match value {
        Value::Operand(x) | Value::Box(x) | Value::Unbox(x) | Value::SetGlobal(_, x) => vec![*x],
        Value::Primitive { args, .. } => args.clone(),
        Value::Call { callee, args } => std::iter::once(*callee)
            .chain(args.iter().copied())
            .collect(),
        Value::CallDirect { args, .. } => args.clone(),
        Value::Closure { free, .. } => free.clone(),
        Value::Global(_) => Vec::new(),
        Value::SetBox(a, b) => vec![*a, *b],
        Value::SetFree { closure, value, .. } => vec![*closure, *value],
    }
}

/// The cheap set of passes, for evaluated code and REPL inputs (ADR 0013
/// #13): contification alone, so their loops are loops.
pub fn contify(module: &mut Module) {
    for n in 0..module.functions.len() {
        for _ in 0..CONTIFICATIONS {
            if !contify::contify(module, n) {
                break;
            }
        }
    }
    compact_functions(module);
}

/// How many rounds of simplification a function gets at most: each round
/// can expose more, but few programs need more than two.
const ROUNDS: usize = 4;

/// Optimises `module` in place. It stays valid.
pub fn optimise(module: &mut Module) {
    let procedures = known::find(module);
    // Every function is contified before any is analysed: a function whose
    // only calls were to its loops calls nothing once they are join points.
    for n in 0..module.functions.len() {
        for _ in 0..CONTIFICATIONS {
            if !contify::contify(module, n) {
                break;
            }
        }
    }
    let known = known::Known::new(module);
    for n in 0..module.functions.len() {
        unbox::unbox(module, n, &known);
        known::direct(module, n, &procedures);
        for _ in 0..ROUNDS {
            if !simplify::simplify(module, n, &procedures) {
                break;
            }
        }
    }
    compact_functions(module);
    compact_constants(module);
    debug_assert!(
        vali_dist::validate(module).is_ok(),
        "the optimiser produced invalid IR: {:?}",
        vali_dist::validate(module)
    );
}

/// Drops the functions no closure or direct call reaches from the body,
/// which contification leaves behind, keeping the others in order.
fn compact_functions(module: &mut Module) {
    let mut reached = vec![false; module.functions.len()];
    reached[module.body.0 as usize] = true;
    let mut pending = vec![module.body];
    while let Some(id) = pending.pop() {
        for term in &module.functions[id.0 as usize].terms {
            if let Some(function) = function_of(term)
                && !std::mem::replace(&mut reached[function.0 as usize], true)
            {
                pending.push(function);
            }
        }
    }
    if reached.iter().all(|&r| r) {
        return;
    }
    let mut renumber = vec![0u32; reached.len()];
    let mut next = 0;
    for (n, &r) in reached.iter().enumerate() {
        renumber[n] = next;
        next += u32::from(r);
    }
    let mut n = 0;
    module.functions.retain(|_| {
        n += 1;
        reached[n - 1]
    });
    module.body = FunId(renumber[module.body.0 as usize]);
    for f in &mut module.functions {
        for term in &mut f.terms {
            match term {
                Term::Let {
                    value: Value::Closure { function, .. } | Value::CallDirect { function, .. },
                    ..
                }
                | Term::TailCallDirect { function, .. } => {
                    *function = FunId(renumber[function.0 as usize]);
                }
                _ => {}
            }
        }
    }
}

/// The function `term` makes a closure of or calls directly, if any.
fn function_of(term: &Term) -> Option<FunId> {
    match term {
        Term::Let {
            value: Value::Closure { function, .. } | Value::CallDirect { function, .. },
            ..
        }
        | Term::TailCallDirect { function, .. } => Some(*function),
        _ => None,
    }
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
                    Value::CallDirect { args, .. } => args.iter_mut().for_each(&mut visit),
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
                Term::TailCallDirect { args, .. } => args.iter_mut().for_each(&mut visit),
            }
        }
    }
}
