//! Assigned variables back into SSA (ADR 0017 #5).
//!
//! Lowering boxes every assigned variable. A box can go when nothing
//! could tell: it is only unboxed and set, never captured by a closure or
//! passed anywhere, and no call that might capture a continuation could
//! show the difference. Re-entering a continuation copies the frame, and
//! with it the value the variable had when the call captured it, where a
//! box would hold the value last set. That differs only when, after the
//! call returns, the variable may be read before it is set again, and it
//! may also be set again. So a call keeps a box only when both hold, and
//! the call may capture: a call to a quiet function the module defines
//! cannot ([`Known`]).
//!
//! Such a box becomes values: reading it is its current value, setting it
//! makes the value set current, and every join point in its scope takes
//! the current value as one more parameter, which every jump to it passes.
//! The copies this leaves are for simplification to remove.

use std::collections::HashMap;

use vali_dist::{
    Constant, ConstantId, Function, JoinId, Module, Operand, Term, TermId, Value, Var,
};

use crate::build::{Builder, Field, HOLE, Hole};
use crate::known::Known;

/// The boxes of `f` that are only unboxed and set.
fn private(f: &Function) -> Vec<Var> {
    let vars = f.vars as usize;
    let mut boxed = vec![false; vars];
    let mut uses = vec![0u32; vars];
    let mut allowed = vec![0u32; vars];
    let count = |xs: &[Operand], uses: &mut Vec<u32>| {
        for x in xs {
            if let Operand::Var(v) = x {
                uses[v.0 as usize] += 1;
            }
        }
    };
    for term in &f.terms {
        match term {
            Term::Let { var, value, .. } => {
                match value {
                    Value::Box(_) => boxed[var.0 as usize] = true,
                    Value::Unbox(Operand::Var(b)) | Value::SetBox(Operand::Var(b), _) => {
                        allowed[b.0 as usize] += 1;
                    }
                    _ => {}
                }
                count(&crate::operands(value), &mut uses);
            }
            Term::Jump { args, .. } => count(args, &mut uses),
            Term::If { test, .. } | Term::Return(test) => count(&[*test], &mut uses),
            Term::TailCall { callee, args, .. } => {
                count(&[*callee], &mut uses);
                count(args, &mut uses);
            }
            Term::TailCallDirect { args, .. } => count(args, &mut uses),
            Term::Join { .. } => {}
        }
    }
    (0..vars)
        .filter(|&v| boxed[v] && uses[v] == allowed[v])
        .map(|v| Var(v as u32))
        .collect()
}

/// A set of boxes, by their place in the candidates.
#[derive(Clone, PartialEq)]
struct Set(Vec<u64>);

impl Set {
    fn new(n: usize) -> Set {
        Set(vec![0; n.div_ceil(64)])
    }

    fn insert(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }

    fn remove(&mut self, i: usize) {
        self.0[i / 64] &= !(1 << (i % 64));
    }

    fn union(&mut self, other: &Set) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a |= b;
        }
    }

    /// Adds every box in both `a` and `b`.
    fn union_both(&mut self, a: &Set, b: &Set) {
        for ((x, y), z) in self.0.iter_mut().zip(&a.0).zip(&b.0) {
            *x |= y & z;
        }
    }

    fn contains(&self, i: usize) -> bool {
        self.0[i / 64] & (1 << (i % 64)) != 0
    }
}

/// From the start of a term on: the boxes that may be read before they
/// are set, and the boxes that may be set.
#[derive(Clone, PartialEq)]
struct Flow {
    live: Set,
    set: Set,
}

/// Which boxes of function `n` can go.
fn candidates(module: &Module, n: usize, known: &Known) -> Vec<Var> {
    let f = &module.functions[n];
    let boxes = private(f);
    if boxes.is_empty() {
        return boxes;
    }
    let mut place = vec![usize::MAX; f.vars as usize];
    for (i, b) in boxes.iter().enumerate() {
        place[b.0 as usize] = i;
    }
    let of = |x: &Operand| match x {
        Operand::Var(v) => Some(place[v.0 as usize]).filter(|&i| i != usize::MAX),
        Operand::Const(_) => None,
    };
    let callees = known.callees(module, n);
    // The terms reached from the body, each after the ones it contains.
    let mut order = Vec::new();
    let mut stack = vec![(f.body, false)];
    while let Some((id, done)) = stack.pop() {
        if done {
            order.push(id);
            continue;
        }
        stack.push((id, true));
        match f.term(id) {
            Term::Let { body, .. } => stack.push((*body, false)),
            Term::Join { handler, body, .. } => {
                stack.push((*handler, false));
                stack.push((*body, false));
            }
            Term::If {
                then, otherwise, ..
            } => {
                stack.push((*then, false));
                stack.push((*otherwise, false));
            }
            Term::Jump { .. }
            | Term::Return(_)
            | Term::TailCall { .. }
            | Term::TailCallDirect { .. } => {}
        }
    }
    let empty = Flow {
        live: Set::new(boxes.len()),
        set: Set::new(boxes.len()),
    };
    let mut flow = vec![empty.clone(); f.terms.len()];
    let mut joins = vec![empty.clone(); f.joins as usize];
    // Each round can only add to the sets, so this ends; a jump to a join
    // point seen later, or back to a loop, needs another round.
    loop {
        let mut changed = false;
        for &id in &order {
            let next = match f.term(id) {
                Term::Let {
                    var, value, body, ..
                } => {
                    let mut next = flow[body.0 as usize].clone();
                    match value {
                        Value::Unbox(x) => {
                            if let Some(i) = of(x) {
                                next.live.insert(i);
                            }
                        }
                        // Making a box again, in a loop, is a new box;
                        // counting it as a set is simpler, and safe.
                        Value::SetBox(x, _) | Value::Box(x) => {
                            let i = match value {
                                Value::Box(_) => of(&Operand::Var(*var)),
                                _ => of(x),
                            };
                            if let Some(i) = i {
                                next.live.remove(i);
                                next.set.insert(i);
                            }
                        }
                        _ => {}
                    }
                    next
                }
                Term::Join {
                    join,
                    handler,
                    body,
                    ..
                } => {
                    let handler = &flow[handler.0 as usize];
                    if joins[join.0 as usize] != *handler {
                        joins[join.0 as usize] = handler.clone();
                        changed = true;
                    }
                    flow[body.0 as usize].clone()
                }
                Term::Jump { join, .. } => joins[join.0 as usize].clone(),
                Term::If {
                    then, otherwise, ..
                } => {
                    let mut next = flow[then.0 as usize].clone();
                    next.live.union(&flow[otherwise.0 as usize].live);
                    next.set.union(&flow[otherwise.0 as usize].set);
                    next
                }
                Term::Return(_) | Term::TailCall { .. } | Term::TailCallDirect { .. } => continue,
            };
            flow[id.0 as usize] = next;
        }
        if !changed {
            break;
        }
    }
    // A box is kept by a call that may capture, if after it the box may
    // be read before it is set, and may be set.
    let mut kept = Set::new(boxes.len());
    for &id in &order {
        let Term::Let { value, body, .. } = f.term(id) else {
            continue;
        };
        let quiet = match value {
            Value::Call { callee, .. } => known.is_quiet(&callees, *callee),
            Value::CallDirect { function, .. } => known.is_quiet_function(*function),
            _ => true,
        };
        if !quiet {
            let after = &flow[body.0 as usize];
            kept.union_both(&after.live, &after.set);
        }
    }
    boxes
        .into_iter()
        .enumerate()
        .filter(|&(i, _)| !kept.contains(i))
        .map(|(_, b)| b)
        .collect()
}
/// Each unboxed variable's current value, sorted by variable.
type Env = Vec<(Var, Operand)>;

fn current(env: &Env, b: Var) -> Operand {
    env.iter()
        .find(|(v, _)| *v == b)
        .map(|(_, x)| *x)
        .expect("an unboxed variable has a value in its scope")
}

fn set(env: &mut Env, b: Var, x: Operand) {
    match env.iter_mut().find(|(v, _)| *v == b) {
        Some(entry) => entry.1 = x,
        None => {
            env.push((b, x));
            env.sort_by_key(|(v, _)| *v);
        }
    }
}

/// Turns the boxes of function `n` that can go into values. Returns
/// whether any did.
pub(crate) fn unbox(module: &mut Module, n: usize, known: &Known) -> bool {
    let boxes = candidates(module, n, known);
    if boxes.is_empty() {
        return false;
    }
    let is_box = |v: Var| boxes.binary_search(&v).is_ok();
    let unspecified = match module
        .constants
        .iter()
        .position(|c| matches!(c, Constant::Unspecified))
    {
        Some(n) => ConstantId(n as u32),
        None => {
            module.constants.push(Constant::Unspecified);
            ConstantId(module.constants.len() as u32 - 1)
        }
    };
    let f = &module.functions[n];
    let mut vars = f.vars;
    let mut fresh = || {
        vars += 1;
        Var(vars - 1)
    };
    let mut extra: HashMap<JoinId, Vec<Var>> = HashMap::new();
    let mut b = Builder::default();
    let mut pending: Vec<(TermId, Hole, Env)> = vec![(f.body, (HOLE, Field::Root), Vec::new())];
    while let Some((mut id, mut hole, mut env)) = pending.pop() {
        loop {
            match f.term(id) {
                Term::Let {
                    var,
                    value,
                    body,
                    position,
                } => {
                    let value = match value {
                        Value::Box(x) if is_box(*var) => {
                            set(&mut env, *var, *x);
                            id = *body;
                            continue;
                        }
                        Value::Unbox(Operand::Var(cell)) if is_box(*cell) => {
                            Value::Operand(current(&env, *cell))
                        }
                        Value::SetBox(Operand::Var(cell), x) if is_box(*cell) => {
                            set(&mut env, *cell, *x);
                            Value::Operand(Operand::Const(unspecified))
                        }
                        other => other.clone(),
                    };
                    let at = b.place(
                        hole,
                        Term::Let {
                            var: *var,
                            value,
                            body: HOLE,
                            position: *position,
                        },
                    );
                    hole = (at, Field::LetBody);
                    id = *body;
                }
                Term::Join {
                    join,
                    params,
                    recursive,
                    handler,
                    body,
                } => {
                    // The current value of each box in scope, as a
                    // parameter.
                    let added: Vec<Var> = env.iter().map(|_| fresh()).collect();
                    let mut handler_env = env.clone();
                    for ((_, x), &p) in handler_env.iter_mut().zip(&added) {
                        *x = Operand::Var(p);
                    }
                    extra.insert(*join, env.iter().map(|(v, _)| *v).collect());
                    let at = b.place(
                        hole,
                        Term::Join {
                            join: *join,
                            params: params.iter().copied().chain(added).collect(),
                            recursive: *recursive,
                            handler: HOLE,
                            body: HOLE,
                        },
                    );
                    pending.push((*handler, (at, Field::JoinHandler), handler_env));
                    hole = (at, Field::JoinBody);
                    id = *body;
                }
                Term::Jump { join, args } => {
                    let mut args = args.clone();
                    if let Some(boxes) = extra.get(join) {
                        args.extend(boxes.iter().map(|&v| current(&env, v)));
                    }
                    b.place(hole, Term::Jump { join: *join, args });
                    break;
                }
                Term::If {
                    test,
                    then,
                    otherwise,
                } => {
                    let at = b.place(
                        hole,
                        Term::If {
                            test: *test,
                            then: HOLE,
                            otherwise: HOLE,
                        },
                    );
                    pending.push((*otherwise, (at, Field::Otherwise), env.clone()));
                    hole = (at, Field::Then);
                    id = *then;
                }
                term @ (Term::Return(_) | Term::TailCall { .. } | Term::TailCallDirect { .. }) => {
                    b.place(hole, term.clone());
                    break;
                }
            }
        }
    }
    let root = b.root.expect("a body is always placed");
    let f = &mut module.functions[n];
    f.terms = b.terms;
    f.body = root;
    f.vars = vars;
    true
}
