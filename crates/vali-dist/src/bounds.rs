//! What converting a function to run format stays under, worked out from
//! its IR alone, so the validator can refuse a function the converter
//! could not convert (ADR 0017 #13).

use crate::ir::{Function, JoinId, Operand, Term, TermId, Value, Var};

/// What a function's conversion to run format stays under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The most operands any one term has: a call's callee and arguments,
    /// the free variables of a closure, a jump's arguments.
    pub operands: usize,
    /// The most variables that can be live at once, by an interval for
    /// each that is never shorter than the converter's own.
    pub live: usize,
    /// The highest slot register allocation may use: first-fit allocation
    /// by start never needs a slot above the number of variables live at
    /// once, and parameters, which have slots of their own, are counted
    /// again. The converter only accepts hinted placements of call
    /// operands up to here, and goes without them otherwise.
    pub allocated_slot: usize,
    /// The highest slot the converted function touches: the allocated
    /// slots, then a call's window (a header of 3 slots, then the callee
    /// and arguments) or the scratch slots operands are loaded into, which
    /// no term uses more of than its operands, plus slack for the few
    /// temporaries a term takes.
    pub top_slot: usize,
    /// The most words of code the converted function takes. A read of an
    /// operand, a move, load or free-variable read, takes at most 2 words
    /// (a `wide` prefix and the instruction), and a call's operands are
    /// placed with at most one more move each to break cycles, so a term
    /// with `n` operands takes at most `4n` plus its own instructions: 10
    /// for a `let` (three reads, the instruction and a load of the
    /// unspecified value), 2 for a tail call, 1 for a jump, 8 for an `if`
    /// (two reads and a wide branch), 4 for a `return`, and none for a
    /// join point.
    pub code_words: u64,
}

/// The bounds of `function`'s conversion. They are only meaningful for a
/// function that [`validate`](crate::validate) has checked.
pub fn bounds(function: &Function) -> Bounds {
    let mut operands = function.free.len().max(function.params.len());
    let mut code_words = 0u64;
    for term in &function.terms {
        let (n, words) = match term {
            Term::Let { value, .. } => {
                let n = match value {
                    Value::Closure { free, .. } => free.len(),
                    Value::Call { args, .. } => args.len() + 1,
                    Value::CallDirect { args, .. } | Value::Primitive { args, .. } => args.len(),
                    _ => 2,
                };
                (n, 10 + 4 * n as u64)
            }
            Term::Jump { args, .. } => (args.len(), 1 + 4 * args.len() as u64),
            Term::TailCall { args, .. } => (args.len() + 1, 2 + 4 * (args.len() as u64 + 1)),
            Term::TailCallDirect { args, .. } => (args.len(), 2 + 4 * args.len() as u64),
            Term::If { .. } => (2, 8),
            Term::Return(_) => (1, 4),
            Term::Join { .. } => (0, 0),
        };
        operands = operands.max(n);
        code_words += words;
    }
    let live = max_live(function);
    let allocated_slot = live + function.params.len() + operands + 8;
    Bounds {
        operands,
        live,
        allocated_slot,
        top_slot: allocated_slot + operands + 8,
        code_words,
    }
}

enum Step<'a> {
    Term(TermId),
    /// A join point's handler starts, binding these parameters.
    Bind(JoinId, &'a [Var]),
    /// A join point's handler has ended.
    End(JoinId),
}

/// The most variables that can be live at once, in the converter's order
/// of terms: each term, a join point's body before its handler, an `if`'s
/// branches in turn. A variable is live from its binding to just after its
/// last use, and to the end of the outermost loop it is used in that
/// began after it was bound. The converter's intervals are no longer: it
/// ends them at the last use, and a fused comparison's at its binding.
fn max_live(f: &Function) -> usize {
    let n = f.vars as usize;
    // A free variable has no slot, so no interval.
    let mut def = vec![u32::MAX; n];
    let mut end = vec![0u32; n];
    for v in &f.params {
        def[v.0 as usize] = 0;
    }
    // Position 0 is the function's entry.
    let mut position = 0u32;
    let mut pending = vec![Step::Term(f.body)];
    let mut recursive = vec![false; f.joins as usize];
    let mut starts = vec![0u32; f.joins as usize];
    let mut regions: Vec<(u32, u32)> = Vec::new();
    let mut used_at: Vec<(Var, u32)> = Vec::new();
    while let Some(step) = pending.pop() {
        position += 1;
        let p = position;
        let mut used = |operands: &[Operand]| {
            for x in operands {
                if let Operand::Var(v) = x {
                    used_at.push((*v, p));
                }
            }
        };
        match step {
            Step::Bind(join, params) => {
                starts[join.0 as usize] = p;
                for v in params {
                    def[v.0 as usize] = p;
                }
            }
            Step::End(join) => {
                if recursive[join.0 as usize] {
                    regions.push((starts[join.0 as usize], p));
                }
            }
            Step::Term(id) => match f.term(id) {
                Term::Let {
                    var, value, body, ..
                } => {
                    def[var.0 as usize] = p;
                    match value {
                        Value::Operand(x)
                        | Value::Box(x)
                        | Value::Unbox(x)
                        | Value::SetGlobal(_, x) => used(&[*x]),
                        Value::Primitive { args, .. } | Value::CallDirect { args, .. } => {
                            used(args);
                        }
                        Value::Closure { free, .. } => used(free),
                        Value::Call { callee, args } => {
                            used(&[*callee]);
                            used(args);
                        }
                        Value::SetBox(a, b) => used(&[*a, *b]),
                        Value::SetFree { closure, value, .. } => used(&[*closure, *value]),
                        Value::Global(_) => {}
                    }
                    pending.push(Step::Term(*body));
                }
                Term::Join {
                    join,
                    params,
                    recursive: rec,
                    handler,
                    body,
                } => {
                    recursive[join.0 as usize] = *rec;
                    pending.push(Step::End(*join));
                    pending.push(Step::Term(*handler));
                    pending.push(Step::Bind(*join, params));
                    pending.push(Step::Term(*body));
                }
                Term::If {
                    test,
                    then,
                    otherwise,
                } => {
                    used(&[*test]);
                    pending.push(Step::Term(*otherwise));
                    pending.push(Step::Term(*then));
                }
                Term::Return(x) => used(&[*x]),
                Term::TailCall { callee, args, .. } => {
                    used(&[*callee]);
                    used(args);
                }
                Term::TailCallDirect { args, .. } | Term::Jump { args, .. } => used(args),
            },
        }
    }
    // Where the interval of a variable bound before a loop and used inside
    // it ends: at the loop's end. Loops nest or are disjoint, so those a
    // use is inside form a chain, outermost first.
    regions.sort_unstable();
    used_at.sort_by_key(|&(_, p)| p);
    let mut next = 0;
    let mut open: Vec<(u32, u32)> = Vec::new();
    for &(v, p) in &used_at {
        while next < regions.len() && regions[next].0 < p {
            let region = regions[next];
            next += 1;
            while open.last().is_some_and(|&(_, stop)| stop < region.0) {
                open.pop();
            }
            open.push(region);
        }
        while open.last().is_some_and(|&(_, stop)| stop < p) {
            open.pop();
        }
        let v = v.0 as usize;
        end[v] = end[v].max(p);
        let first = open.partition_point(|&(start, _)| start <= def[v]);
        if let Some(&(_, stop)) = open.get(first) {
            end[v] = end[v].max(stop);
        }
    }
    // Each interval is [def, end + 1), at least one position long.
    let mut edges: Vec<(u32, i32)> = Vec::new();
    for (v, &d) in def.iter().enumerate() {
        if d != u32::MAX {
            edges.push((d, 1));
            edges.push((end[v].max(d) + 1, -1));
        }
    }
    // An interval ending where another starts frees its slot first.
    edges.sort_unstable();
    let mut live = 0i32;
    let mut most = 0i32;
    for (_, change) in edges {
        live += change;
        most = most.max(live);
    }
    most as usize
}
