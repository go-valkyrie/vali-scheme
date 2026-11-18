//! Contification: local functions that are only ever continued into become
//! join points (ADR 0017 #9).
//!
//! A closure `c` of function `g`, made in function `f`, can be contified
//! when `f` uses it only as the callee of one call, and to tie it to
//! itself with `set-free c i c` (how `letrec` closes a loop), and `g` uses
//! that free variable only as the callee of tail calls to itself, with the
//! arguments its arity fixes. Then `g`'s body is copied into `f` as a
//! `rec join`, its own tail calls becoming jumps:
//!
//! - at a tail call, `tail c(a)` becomes `rec join j(p) = body in jump
//!   j(a)`, and the body's returns are `f`'s;
//! - at a call for a value, `let r = call c(a) in k` becomes `join k'(r) =
//!   k in rec join j(p) = body in jump j(a)`, the body's returns jump to
//!   `k'`, and its tail calls to anything else become a call and a jump.
//!
//! `g`'s free variables become the operands the closure captured, which
//! are in scope wherever `c` is. The closure, and the `set-free` that tied
//! it, go. A named `let` lowers to such a closure; this is what makes it a
//! loop.

use vali_dist::{
    Constant, ConstantId, FunId, Function, JoinId, Module, Operand, Position, Term, TermId, Value,
    Var,
};

use crate::build::{Builder, Field, HOLE, Hole};

/// What makes a closure a candidate, in the function that makes it.
struct Candidate {
    function: FunId,
    free: Vec<Operand>,
    /// The `let` making it, and the `set-free`s tying it to itself, by the
    /// free variable each sets.
    made_at: TermId,
    ties: Vec<(TermId, u32)>,
    /// Its one call.
    call: TermId,
}

/// How each variable of `f` is used, and where closures are made.
fn find(module: &Module, f: &Function) -> Option<Candidate> {
    let vars = f.vars as usize;
    // Per variable: where a closure binds it, its call sites, its ties,
    // and whether it is used any other way.
    let mut made: Vec<Option<(TermId, FunId, Vec<Operand>)>> = vec![None; vars];
    let mut calls: Vec<Vec<TermId>> = vec![Vec::new(); vars];
    let mut ties: Vec<Vec<(TermId, u32)>> = vec![Vec::new(); vars];
    let mut other = vec![false; vars];
    let mark = |xs: &[Operand], other: &mut Vec<bool>| {
        for x in xs {
            if let Operand::Var(v) = x {
                other[v.0 as usize] = true;
            }
        }
    };
    let mut pending = vec![f.body];
    while let Some(id) = pending.pop() {
        match f.term(id) {
            Term::Let {
                var, value, body, ..
            } => {
                match value {
                    Value::Closure { function, free } => {
                        mark(free, &mut other);
                        made[var.0 as usize] = Some((id, *function, free.clone()));
                    }
                    Value::Call { callee, args } => {
                        match callee {
                            Operand::Var(c) => calls[c.0 as usize].push(id),
                            Operand::Const(_) => {}
                        }
                        mark(args, &mut other);
                    }
                    Value::CallDirect { args, .. } => mark(args, &mut other),
                    Value::SetFree {
                        closure: Operand::Var(c),
                        index,
                        value: Operand::Var(v),
                    } if c == v => ties[c.0 as usize].push((id, *index)),
                    Value::SetFree { closure, value, .. } => mark(&[*closure, *value], &mut other),
                    Value::Operand(x)
                    | Value::Box(x)
                    | Value::Unbox(x)
                    | Value::SetGlobal(_, x) => {
                        mark(&[*x], &mut other);
                    }
                    Value::Primitive { args, .. } => mark(args, &mut other),
                    Value::SetBox(a, b) => mark(&[*a, *b], &mut other),
                    Value::Global(_) => {}
                }
                pending.push(*body);
            }
            Term::Join { handler, body, .. } => {
                pending.push(*handler);
                pending.push(*body);
            }
            Term::Jump { args, .. } => mark(args, &mut other),
            Term::If {
                test,
                then,
                otherwise,
            } => {
                mark(&[*test], &mut other);
                pending.push(*then);
                pending.push(*otherwise);
            }
            Term::Return(x) => mark(&[*x], &mut other),
            Term::TailCall { callee, args, .. } => {
                match callee {
                    Operand::Var(c) => calls[c.0 as usize].push(id),
                    Operand::Const(_) => {}
                }
                mark(args, &mut other);
            }
            Term::TailCallDirect { args, .. } => mark(args, &mut other),
        }
    }
    (0..vars).find_map(|v| {
        let (made_at, function, free) = made[v].clone()?;
        if other[v] || calls[v].len() != 1 {
            return None;
        }
        let g = &module.functions[function.0 as usize];
        if g.arity.optional != 0 || g.arity.rest {
            return None;
        }
        let call = calls[v][0];
        let argc = match f.term(call) {
            Term::Let {
                value: Value::Call { args, .. },
                ..
            }
            | Term::TailCall { args, .. } => args.len(),
            _ => return None,
        };
        if argc != usize::from(g.arity.required) {
            return None;
        }
        let selves: Vec<u32> = ties[v].iter().map(|&(_, i)| i).collect();
        if !self_calls_only(g, &selves) {
            return None;
        }
        Some(Candidate {
            function,
            free,
            made_at,
            ties: ties[v].clone(),
            call,
        })
    })
}

/// Whether `g` uses each of its free variables `selves`, which hold its
/// own closure, only as the callee of tail calls with its arity, and
/// whether every free index is in range.
fn self_calls_only(g: &Function, selves: &[u32]) -> bool {
    if selves.iter().any(|&i| i as usize >= g.free.len()) {
        return false;
    }
    let is_self = |x: &Operand| match x {
        Operand::Var(v) => selves.iter().any(|&i| g.free[i as usize] == *v),
        Operand::Const(_) => false,
    };
    let argc = usize::from(g.arity.required);
    let mut pending = vec![g.body];
    while let Some(id) = pending.pop() {
        let (operands, _callee_is_allowed): (Vec<Operand>, bool) = match g.term(id) {
            Term::Let { value, body, .. } => {
                pending.push(*body);
                (crate::operands(value), false)
            }
            Term::Join { handler, body, .. } => {
                pending.push(*handler);
                pending.push(*body);
                continue;
            }
            Term::Jump { args, .. } => (args.clone(), false),
            Term::If {
                test,
                then,
                otherwise,
            } => {
                pending.push(*then);
                pending.push(*otherwise);
                (vec![*test], false)
            }
            Term::Return(x) => (vec![*x], false),
            Term::TailCall { callee, args, .. } => {
                if is_self(callee) && args.len() != argc {
                    return false;
                }
                (args.clone(), true)
            }
            Term::TailCallDirect { args, .. } => (args.clone(), true),
        };
        if operands.iter().any(is_self) {
            return false;
        }
    }
    true
}

/// Where the copied body's results go.
#[derive(Clone, Copy)]
enum Into {
    /// It replaces a tail call: its returns are the caller's.
    Tail,
    /// It replaces a call for a value: its returns jump here.
    Value(JoinId),
}

/// One step of rebuilding the caller.
enum Task {
    /// A term of the caller.
    Own(TermId, Hole),
    /// A term of the copied body.
    Copied(TermId, Hole),
}

/// Contifies one closure of function `n`, if one can be. Returns whether
/// one was.
pub(crate) fn contify(module: &mut Module, n: usize) -> bool {
    let Some(c) = find(module, &module.functions[n]) else {
        return false;
    };
    if c.function.0 as usize == n {
        return false;
    }
    let unspecified = match module
        .constants
        .iter()
        .position(|k| matches!(k, Constant::Unspecified))
    {
        Some(k) => ConstantId(k as u32),
        None => {
            module.constants.push(Constant::Unspecified);
            ConstantId(module.constants.len() as u32 - 1)
        }
    };
    let f = &module.functions[n];
    let g = &module.functions[c.function.0 as usize];
    // g's variables and join points follow f's.
    let var_offset = f.vars;
    let join_offset = f.joins;
    let mut vars = f.vars + g.vars;
    let mut joins = f.joins + g.joins;
    let loop_join = JoinId(joins);
    joins += 1;
    let selves: Vec<Var> = c.ties.iter().map(|&(_, i)| g.free[i as usize]).collect();
    let map = |x: Operand| -> Operand {
        match x {
            Operand::Const(_) => x,
            Operand::Var(v) => match g.free.iter().position(|w| *w == v) {
                Some(i) => c.free[i],
                None => Operand::Var(Var(v.0 + var_offset)),
            },
        }
    };
    let mut into = Into::Tail;
    let mut b = Builder::default();
    let mut tasks = vec![Task::Own(f.body, (HOLE, Field::Root))];
    while let Some(task) = tasks.pop() {
        match task {
            Task::Own(id, hole) => match f.term(id) {
                _ if id == c.made_at => {
                    let Term::Let { body, .. } = f.term(id) else {
                        unreachable!("a closure is made by a let");
                    };
                    tasks.push(Task::Own(*body, hole));
                }
                Term::Let { var, body, .. } if c.ties.iter().any(|&(t, _)| t == id) => {
                    let at = b.place(
                        hole,
                        Term::Let {
                            var: *var,
                            value: Value::Operand(Operand::Const(unspecified)),
                            body: HOLE,
                            position: None,
                        },
                    );
                    tasks.push(Task::Own(*body, (at, Field::LetBody)));
                }
                _ if id == c.call => {
                    let (args, continuation) = match f.term(id) {
                        Term::TailCall { args, .. } => (args.clone(), None),
                        Term::Let {
                            var,
                            value: Value::Call { args, .. },
                            body,
                            ..
                        } => (args.clone(), Some((*var, *body))),
                        _ => unreachable!("the call is a call"),
                    };
                    let mut hole = hole;
                    if let Some((result, k)) = continuation {
                        let join = JoinId(joins);
                        joins += 1;
                        into = Into::Value(join);
                        let at = b.place(
                            hole,
                            Term::Join {
                                join,
                                params: vec![result],
                                recursive: false,
                                handler: HOLE,
                                body: HOLE,
                            },
                        );
                        tasks.push(Task::Own(k, (at, Field::JoinHandler)));
                        hole = (at, Field::JoinBody);
                    }
                    let params = g.params.iter().map(|p| Var(p.0 + var_offset)).collect();
                    let at = b.place(
                        hole,
                        Term::Join {
                            join: loop_join,
                            params,
                            recursive: true,
                            handler: HOLE,
                            body: HOLE,
                        },
                    );
                    b.place(
                        (at, Field::JoinBody),
                        Term::Jump {
                            join: loop_join,
                            args,
                        },
                    );
                    tasks.push(Task::Copied(g.body, (at, Field::JoinHandler)));
                }
                term => {
                    let (copy, children) = shell(term);
                    let at = b.place(hole, copy);
                    for (child, field) in children {
                        tasks.push(Task::Own(child, (at, field)));
                    }
                }
            },
            Task::Copied(id, hole) => match g.term(id) {
                Term::Let {
                    var,
                    value,
                    body,
                    position,
                } => {
                    let at = b.place(
                        hole,
                        Term::Let {
                            var: Var(var.0 + var_offset),
                            value: map_value(value, &map),
                            body: HOLE,
                            position: *position,
                        },
                    );
                    tasks.push(Task::Copied(*body, (at, Field::LetBody)));
                }
                Term::Join {
                    join,
                    params,
                    recursive,
                    handler,
                    body,
                } => {
                    let at = b.place(
                        hole,
                        Term::Join {
                            join: JoinId(join.0 + join_offset),
                            params: params.iter().map(|p| Var(p.0 + var_offset)).collect(),
                            recursive: *recursive,
                            handler: HOLE,
                            body: HOLE,
                        },
                    );
                    tasks.push(Task::Copied(*handler, (at, Field::JoinHandler)));
                    tasks.push(Task::Copied(*body, (at, Field::JoinBody)));
                }
                Term::Jump { join, args } => {
                    b.place(
                        hole,
                        Term::Jump {
                            join: JoinId(join.0 + join_offset),
                            args: args.iter().map(|&x| map(x)).collect(),
                        },
                    );
                }
                Term::If {
                    test,
                    then,
                    otherwise,
                } => {
                    let at = b.place(
                        hole,
                        Term::If {
                            test: map(*test),
                            then: HOLE,
                            otherwise: HOLE,
                        },
                    );
                    tasks.push(Task::Copied(*then, (at, Field::Then)));
                    tasks.push(Task::Copied(*otherwise, (at, Field::Otherwise)));
                }
                Term::Return(x) => {
                    let x = map(*x);
                    b.place(hole, result(into, x));
                }
                Term::TailCall {
                    callee,
                    args,
                    position,
                } => {
                    let args: Vec<Operand> = args.iter().map(|&x| map(x)).collect();
                    let is_self = matches!(callee, Operand::Var(v) if selves.contains(v));
                    if is_self {
                        b.place(
                            hole,
                            Term::Jump {
                                join: loop_join,
                                args,
                            },
                        );
                        continue;
                    }
                    let callee = map(*callee);
                    match into {
                        Into::Tail => {
                            b.place(
                                hole,
                                Term::TailCall {
                                    callee,
                                    args,
                                    position: *position,
                                },
                            );
                        }
                        Into::Value(join) => {
                            let t = Var(vars);
                            vars += 1;
                            let call = Value::Call { callee, args };
                            call_then_jump(&mut b, hole, t, call, *position, join);
                        }
                    }
                }
                Term::TailCallDirect {
                    function,
                    args,
                    position,
                } => {
                    let args: Vec<Operand> = args.iter().map(|&x| map(x)).collect();
                    match into {
                        Into::Tail => {
                            b.place(
                                hole,
                                Term::TailCallDirect {
                                    function: *function,
                                    args,
                                    position: *position,
                                },
                            );
                        }
                        Into::Value(join) => {
                            let t = Var(vars);
                            vars += 1;
                            let call = Value::CallDirect {
                                function: *function,
                                args,
                            };
                            call_then_jump(&mut b, hole, t, call, *position, join);
                        }
                    }
                }
            },
        }
    }
    let root = b.root.expect("a body is always placed");
    let f = &mut module.functions[n];
    f.terms = b.terms;
    f.body = root;
    f.vars = vars;
    f.joins = joins;
    true
}

/// A copied body's result: returned, or jumped with to `into`'s join.
fn result(into: Into, x: Operand) -> Term {
    match into {
        Into::Tail => Term::Return(x),
        Into::Value(join) => Term::Jump {
            join,
            args: vec![x],
        },
    }
}

fn call_then_jump(
    b: &mut Builder,
    hole: Hole,
    t: Var,
    call: Value,
    position: Option<Position>,
    join: JoinId,
) {
    let at = b.place(
        hole,
        Term::Let {
            var: t,
            value: call,
            body: HOLE,
            position,
        },
    );
    b.place(
        (at, Field::LetBody),
        Term::Jump {
            join,
            args: vec![Operand::Var(t)],
        },
    );
}

/// `term` with its children left as holes, and the children with the
/// fields they fill, the one to build first last.
fn shell(term: &Term) -> (Term, Vec<(TermId, Field)>) {
    match term {
        Term::Let {
            var,
            value,
            body,
            position,
        } => (
            Term::Let {
                var: *var,
                value: value.clone(),
                body: HOLE,
                position: *position,
            },
            vec![(*body, Field::LetBody)],
        ),
        Term::Join {
            join,
            params,
            recursive,
            handler,
            body,
        } => (
            Term::Join {
                join: *join,
                params: params.clone(),
                recursive: *recursive,
                handler: HOLE,
                body: HOLE,
            },
            vec![(*handler, Field::JoinHandler), (*body, Field::JoinBody)],
        ),
        Term::If {
            test,
            then,
            otherwise,
        } => (
            Term::If {
                test: *test,
                then: HOLE,
                otherwise: HOLE,
            },
            vec![(*otherwise, Field::Otherwise), (*then, Field::Then)],
        ),
        other => (other.clone(), Vec::new()),
    }
}

fn map_value(value: &Value, map: &dyn Fn(Operand) -> Operand) -> Value {
    let all = |xs: &[Operand]| xs.iter().map(|&x| map(x)).collect::<Vec<_>>();
    match value {
        Value::Operand(x) => Value::Operand(map(*x)),
        Value::Primitive { op, args } => Value::Primitive {
            op: *op,
            args: all(args),
        },
        Value::Call { callee, args } => Value::Call {
            callee: map(*callee),
            args: all(args),
        },
        Value::CallDirect { function, args } => Value::CallDirect {
            function: *function,
            args: all(args),
        },
        Value::Closure { function, free } => Value::Closure {
            function: *function,
            free: all(free),
        },
        Value::Global(g) => Value::Global(*g),
        Value::SetGlobal(g, x) => Value::SetGlobal(*g, map(*x)),
        Value::Box(x) => Value::Box(map(*x)),
        Value::Unbox(x) => Value::Unbox(map(*x)),
        Value::SetBox(a, b) => Value::SetBox(map(*a), map(*b)),
        Value::SetFree {
            closure,
            index,
            value,
        } => Value::SetFree {
            closure: map(*closure),
            index: *index,
            value: map(*value),
        },
    }
}
