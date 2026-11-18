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
//! including the ones Rust implements, does not. A direct call is quiet
//! when the function it calls is.
//!
//! Known procedures ([`find`]) are narrower: globals that hold a closure of
//! one of the module's own functions, with no free variables, from before
//! any of the module's code can run, and that nothing assigns again.
//! Reading one cannot fail, and a call through one becomes a direct call
//! ([`direct`], ADR 0017 #5). Only the module's own definitions count, so
//! nothing here can go stale when another library changes (ADR 0012 #7):
//! an importer cannot assign them (R7RS 5.6.1), and only this module's
//! code sees them as known.

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
                    Term::Let {
                        value: Value::CallDirect { function, .. },
                        ..
                    }
                    | Term::TailCallDirect { function, .. } => !known.is_quiet_function(*function),
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

    /// Whether calling function `f` directly cannot capture a continuation.
    pub(crate) fn is_quiet_function(&self, f: FunId) -> bool {
        self.quiet[f.0 as usize]
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

/// For each global of `module`, the function it is known to hold a closure
/// of, if it is.
///
/// The module's body is followed from its start while what it does cannot
/// run code or raise: making closures and assigning globals. A global
/// assigned a closure there, of a function with no free variables, and
/// assigned nowhere else, holds that closure from then on, and no code
/// that could read it runs before then.
pub(crate) fn find(module: &Module) -> Vec<Option<FunId>> {
    let mut sets = vec![0u32; module.globals.len()];
    for f in &module.functions {
        for term in &f.terms {
            if let Term::Let {
                value: Value::SetGlobal(g, _),
                ..
            } = term
            {
                sets[g.0 as usize] += 1;
            }
        }
    }
    let body = &module.functions[module.body.0 as usize];
    let mut closures: Vec<Option<FunId>> = vec![None; body.vars as usize];
    let mut known = vec![None; module.globals.len()];
    let mut id = body.body;
    while let Term::Let {
        var,
        value,
        body: next,
        ..
    } = body.term(id)
    {
        match value {
            Value::Closure { function, free } if free.is_empty() => {
                closures[var.0 as usize] = Some(*function);
            }
            Value::SetGlobal(g, Operand::Var(v)) => {
                let g = g.0 as usize;
                if sets[g] == 1 && module.globals[g].link == GlobalLink::Defined {
                    known[g] = closures[v.0 as usize];
                }
            }
            value if !quiet(value) => break,
            _ => {}
        }
        id = *next;
    }
    known
}

/// Whether `value` may appear in the module body's prefix: it neither runs
/// code nor raises.
fn quiet(value: &Value) -> bool {
    use vali_dist::Effects;
    let effects = value.effects();
    !effects.contains(Effects::RAISE) && !effects.contains(Effects::CAPTURE)
}

/// Turns the calls in function `n` whose callee is a known global, read
/// there, into direct calls, when the callee takes that many arguments.
/// What read the global is left for simplification to remove. Returns
/// whether anything changed.
pub(crate) fn direct(module: &mut Module, n: usize, known: &[Option<FunId>]) -> bool {
    let arities: Vec<_> = module.functions.iter().map(|f| f.arity).collect();
    let f = &mut module.functions[n];
    let mut of: Vec<Option<FunId>> = vec![None; f.vars as usize];
    for term in &f.terms {
        if let Term::Let {
            var,
            value: Value::Global(g),
            ..
        } = term
        {
            of[var.0 as usize] = known[g.0 as usize];
        }
    }
    let target = |callee: &Operand, argc: usize| match callee {
        Operand::Var(v) => of[v.0 as usize].filter(|t| arities[t.0 as usize].accepts(argc)),
        Operand::Const(_) => None,
    };
    let mut changed = false;
    for term in &mut f.terms {
        match term {
            Term::Let { value, .. } => {
                if let Value::Call { callee, args } = value
                    && let Some(function) = target(callee, args.len())
                {
                    *value = Value::CallDirect {
                        function,
                        args: std::mem::take(args),
                    };
                    changed = true;
                }
            }
            Term::TailCall {
                callee,
                args,
                position,
            } => {
                if let Some(function) = target(callee, args.len()) {
                    *term = Term::TailCallDirect {
                        function,
                        args: std::mem::take(args),
                        position: *position,
                    };
                    changed = true;
                }
            }
            _ => {}
        }
    }
    changed
}
