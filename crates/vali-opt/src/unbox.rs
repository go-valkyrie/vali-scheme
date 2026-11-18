//! Assigned variables back into SSA (ADR 0017 #5).
//!
//! Lowering boxes every assigned variable. A box can go when nothing
//! could tell: it is only unboxed and set, never captured by a closure or
//! passed anywhere, and no call that might capture a continuation happens
//! in its scope, where re-entering that continuation would copy the frame
//! and with it a value the box would have shared. For now any non-tail
//! call in its scope keeps a box, which is more than needed: only a call
//! the variable is live across must.
//!
//! Such a box becomes values: reading it is its current value, setting it
//! makes the value set current, and every join point in its scope takes
//! the current value as one more parameter, which every jump to it passes.
//! The copies this leaves are for simplification to remove.

use std::collections::HashMap;

use vali_dist::{Constant, ConstantId, JoinId, Module, Operand, Term, TermId, Value, Var};

use crate::build::{Builder, Field, HOLE, Hole};

/// Which boxes of function `n` can go.
fn candidates(module: &Module, n: usize) -> Vec<Var> {
    let f = &module.functions[n];
    let vars = f.vars as usize;
    let mut boxed = vec![false; vars];
    let mut uses = vec![0u32; vars];
    let mut allowed = vec![0u32; vars];
    let mut crossed = vec![false; vars];
    enum Task {
        Enter(TermId),
        Exit,
    }
    let mut open: Vec<Var> = Vec::new();
    let mut tasks = vec![Task::Enter(f.body)];
    let count = |xs: &[Operand], uses: &mut Vec<u32>| {
        for x in xs {
            if let Operand::Var(v) = x {
                uses[v.0 as usize] += 1;
            }
        }
    };
    while let Some(task) = tasks.pop() {
        let id = match task {
            Task::Exit => {
                open.pop();
                continue;
            }
            Task::Enter(id) => id,
        };
        match f.term(id) {
            Term::Let {
                var, value, body, ..
            } => {
                match value {
                    Value::Box(x) => {
                        count(&[*x], &mut uses);
                        boxed[var.0 as usize] = true;
                        open.push(*var);
                        tasks.push(Task::Exit);
                    }
                    Value::Unbox(x) => {
                        count(&[*x], &mut uses);
                        if let Operand::Var(b) = x {
                            allowed[b.0 as usize] += 1;
                        }
                    }
                    Value::SetBox(cell, x) => {
                        count(&[*cell, *x], &mut uses);
                        if let Operand::Var(b) = cell {
                            allowed[b.0 as usize] += 1;
                        }
                    }
                    Value::Call { callee, args } => {
                        count(&[*callee], &mut uses);
                        count(args, &mut uses);
                        for b in &open {
                            crossed[b.0 as usize] = true;
                        }
                    }
                    Value::Operand(x) | Value::SetGlobal(_, x) => count(&[*x], &mut uses),
                    Value::Primitive { args, .. } => count(args, &mut uses),
                    Value::Closure { free, .. } => count(free, &mut uses),
                    Value::Global(_) => {}
                    Value::SetFree { closure, value, .. } => {
                        count(&[*closure, *value], &mut uses);
                    }
                }
                tasks.push(Task::Enter(*body));
            }
            Term::Join { handler, body, .. } => {
                tasks.push(Task::Enter(*handler));
                tasks.push(Task::Enter(*body));
            }
            Term::Jump { args, .. } => count(args, &mut uses),
            Term::If {
                test,
                then,
                otherwise,
            } => {
                count(&[*test], &mut uses);
                tasks.push(Task::Enter(*then));
                tasks.push(Task::Enter(*otherwise));
            }
            Term::Return(x) => count(&[*x], &mut uses),
            Term::TailCall { callee, args, .. } => {
                count(&[*callee], &mut uses);
                count(args, &mut uses);
            }
        }
    }
    (0..vars)
        .filter(|&v| boxed[v] && uses[v] == allowed[v] && !crossed[v])
        .map(|v| Var(v as u32))
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
pub(crate) fn unbox(module: &mut Module, n: usize) -> bool {
    let boxes = candidates(module, n);
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
                term @ (Term::Return(_) | Term::TailCall { .. }) => {
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
