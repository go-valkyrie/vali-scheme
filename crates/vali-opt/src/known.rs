//! Which calls cannot capture a continuation, from what one module's own
//! code shows.
//!
//! A call's callee is known when it is a closure the same function made,
//! or a global the module defines and assigns exactly once, to a closure.
//! Nothing outside the module can assign such a global: importers cannot
//! `set!` it, and neither can `eval`'d code, which is another unit. Read
//! before it is assigned, it is unbound, and reading it raises.
//!
//! A function is quiet when every call and tail call it makes is to a
//! known, quiet function: running it calls nothing that could capture a
//! continuation. Primitive operations never capture, and an error one
//! raises never returns to the frame that raised it, so a handler that
//! captures there cannot re-enter the function's caller after the call.
//! Recursion keeps a function quiet; any call to an unknown procedure,
//! including the ones Rust implements, does not.

use vali_dist::{FunId, GlobalId, GlobalLink, Module, Operand, Term, Value};

/// What a module's code shows about its calls.
pub(crate) struct Known {
    /// The function each global is assigned a closure of, if it is
    /// assigned exactly once.
    globals: Vec<Option<FunId>>,
    /// Whether each function is quiet.
    quiet: Vec<bool>,
}

impl Known {
    /// Analyses `module`. Later passes may remove calls but never add one,
    /// so what this finds stays true while they run.
    pub(crate) fn new(module: &Module) -> Known {
        let mut assigned = vec![0u32; module.globals.len()];
        let mut globals: Vec<Option<FunId>> = vec![None; module.globals.len()];
        for f in &module.functions {
            let closures = closures(f);
            for term in &f.terms {
                if let Term::Let {
                    value: Value::SetGlobal(g, x),
                    ..
                } = term
                {
                    let g = g.0 as usize;
                    assigned[g] += 1;
                    globals[g] = match x {
                        Operand::Var(v) => closures[v.0 as usize],
                        Operand::Const(_) => None,
                    };
                }
            }
        }
        for (g, global) in module.globals.iter().enumerate() {
            if assigned[g] != 1 || global.link != GlobalLink::Defined {
                globals[g] = None;
            }
        }
        let mut known = Known {
            globals,
            quiet: vec![true; module.functions.len()],
        };
        // The greatest fixed point: a function stops being quiet when it
        // calls one that is not.
        let mut changed = true;
        while changed {
            changed = false;
            for (n, f) in module.functions.iter().enumerate() {
                if !known.quiet[n] {
                    continue;
                }
                let callees = known.callees(module, n);
                let loud = f.terms.iter().any(|term| match term {
                    Term::Let {
                        value: Value::Call { callee, .. },
                        ..
                    }
                    | Term::TailCall { callee, .. } => !known.is_quiet(&callees, *callee),
                    _ => false,
                });
                if loud {
                    known.quiet[n] = false;
                    changed = true;
                }
            }
        }
        known
    }

    /// The function each variable of function `n` is known to hold a
    /// closure of.
    pub(crate) fn callees(&self, module: &Module, n: usize) -> Vec<Option<FunId>> {
        let f = &module.functions[n];
        let mut callees = closures(f);
        for term in &f.terms {
            if let Term::Let {
                var,
                value: Value::Global(g),
                ..
            } = term
            {
                callees[var.0 as usize] = self.global(*g);
            }
        }
        callees
    }

    /// The function the global `g` holds a closure of, if it is known.
    pub(crate) fn global(&self, g: GlobalId) -> Option<FunId> {
        self.globals.get(g.0 as usize).copied().flatten()
    }

    /// Whether calling `callee`, given the `callees` of the function it is
    /// in, cannot capture a continuation.
    pub(crate) fn is_quiet(&self, callees: &[Option<FunId>], callee: Operand) -> bool {
        match callee {
            Operand::Var(v) => callees[v.0 as usize].is_some_and(|f| self.quiet[f.0 as usize]),
            Operand::Const(_) => false,
        }
    }
}

/// The function each variable of `f` is bound to a new closure of.
fn closures(f: &vali_dist::Function) -> Vec<Option<FunId>> {
    let mut closures = vec![None; f.vars as usize];
    for term in &f.terms {
        if let Term::Let {
            var,
            value: Value::Closure { function, .. },
            ..
        } = term
        {
            closures[var.0 as usize] = Some(*function);
        }
    }
    closures
}
