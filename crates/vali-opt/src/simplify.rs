//! Simplification: constant folding, copy propagation, `if`s on
//! constants, and removing what is unused.
//!
//! One round counts each variable's uses and each join point's jumps in
//! what the function's body reaches, then rebuilds the body top-down:
//!
//! - a `let` whose value folds to a constant, or is just an operand, is
//!   dropped, and its variable replaced by that operand where it is used;
//! - a `let` whose variable is unused is dropped if its value may only
//!   allocate or read (ADR 0017 #4): raising, writing and capturing stay;
//! - an `if` on a constant keeps only the arm it takes;
//! - a join point nothing jumps to keeps only its body.
//!
//! What a dropped term made unused is found by the next round.

use vali_dist::{
    Constant, ConstantId, Effects, Function, JoinId, Module, Operand, Primitive, Term, TermId,
    Value,
};

use crate::build::{Builder, Field, HOLE, Hole};

/// The module's constants, and those this round adds.
struct Pool<'a> {
    base: &'a [Constant],
    added: Vec<Constant>,
}

impl Pool<'_> {
    fn get(&self, id: ConstantId) -> &Constant {
        let n = id.0 as usize;
        match self.base.get(n) {
            Some(c) => c,
            None => &self.added[n - self.base.len()],
        }
    }

    /// The id of `c`: one already in the pool if it is a simple value there,
    /// or a new one.
    fn add(&mut self, c: Constant) -> ConstantId {
        let simple = matches!(
            c,
            Constant::Integer(_) | Constant::Boolean(_) | Constant::EmptyList
        );
        if simple
            && let Some(n) = self
                .base
                .iter()
                .chain(&self.added)
                .position(|existing| *existing == c)
        {
            return ConstantId(n as u32);
        }
        self.added.push(c);
        ConstantId((self.base.len() + self.added.len() - 1) as u32)
    }

    fn integer(&self, x: Operand) -> Option<i64> {
        match x {
            Operand::Const(c) => match self.get(c) {
                Constant::Integer(n) => Some(*n),
                _ => None,
            },
            Operand::Var(_) => None,
        }
    }
}

/// `value` computed now, if its operands are constants and it cannot
/// raise on them.
fn fold(value: &Value, pool: &mut Pool<'_>) -> Option<Operand> {
    let Value::Primitive { op, args } = value else {
        return None;
    };
    let ints = || -> Option<(i64, i64)> { Some((pool.integer(args[0])?, pool.integer(args[1])?)) };
    let constant = |pool: &Pool<'_>, i: usize| match args[i] {
        Operand::Const(c) => Some(pool.get(c).clone()),
        Operand::Var(_) => None,
    };
    let folded = match op {
        Primitive::Add => Constant::Integer(ints().and_then(|(a, b)| a.checked_add(b))?),
        Primitive::Sub => Constant::Integer(ints().and_then(|(a, b)| a.checked_sub(b))?),
        Primitive::Mul => Constant::Integer(ints().and_then(|(a, b)| a.checked_mul(b))?),
        Primitive::Lt => Constant::Boolean(ints().map(|(a, b)| a < b)?),
        Primitive::Le => Constant::Boolean(ints().map(|(a, b)| a <= b)?),
        Primitive::NumEq => Constant::Boolean(ints().map(|(a, b)| a == b)?),
        // The same constant is the same object.
        Primitive::Eq => match (args[0], args[1]) {
            (Operand::Const(a), Operand::Const(b)) if a == b => Constant::Boolean(true),
            _ => return None,
        },
        Primitive::Not => Constant::Boolean(matches!(constant(pool, 0)?, Constant::Boolean(false))),
        Primitive::IsNull => Constant::Boolean(matches!(constant(pool, 0)?, Constant::EmptyList)),
        Primitive::IsPair => Constant::Boolean(matches!(constant(pool, 0)?, Constant::Pair(..))),
        Primitive::Car | Primitive::Cdr => match constant(pool, 0)? {
            Constant::Pair(ref car, ref cdr) => {
                let part = if *op == Primitive::Car { car } else { cdr };
                return Some(Operand::Const(pool.add((**part).clone())));
            }
            _ => return None,
        },
        Primitive::VectorLength => match constant(pool, 0)? {
            Constant::Vector(ref items) => Constant::Integer(i64::try_from(items.len()).ok()?),
            _ => return None,
        },
        _ => return None,
    };
    Some(Operand::Const(pool.add(folded)))
}

/// Whether a `let` of `value` can go when its variable is unused.
fn removable(value: &Value) -> bool {
    let effects = value.effects();
    !effects.contains(Effects::WRITE)
        && !effects.contains(Effects::RAISE)
        && !effects.contains(Effects::CAPTURE)
}

fn operands_of(value: &Value) -> Vec<Operand> {
    match value {
        Value::Operand(x) | Value::Box(x) | Value::Unbox(x) | Value::SetGlobal(_, x) => vec![*x],
        Value::Primitive { args, .. } => args.clone(),
        Value::Call { callee, args } => std::iter::once(*callee)
            .chain(args.iter().copied())
            .collect(),
        Value::Closure { free, .. } => free.clone(),
        Value::Global(_) => Vec::new(),
        Value::SetBox(a, b) => vec![*a, *b],
        Value::SetFree { closure, value, .. } => vec![*closure, *value],
    }
}

/// How often each variable is used, and each join point jumped to, in
/// what `f`'s body reaches.
fn count(f: &Function) -> (Vec<u32>, Vec<u32>) {
    let mut uses = vec![0u32; f.vars as usize];
    let mut jumps = vec![0u32; f.joins as usize];
    let use_all = |xs: &[Operand], uses: &mut Vec<u32>| {
        for x in xs {
            if let Operand::Var(v) = x {
                uses[v.0 as usize] += 1;
            }
        }
    };
    let mut pending = vec![f.body];
    while let Some(id) = pending.pop() {
        match f.term(id) {
            Term::Let { value, body, .. } => {
                use_all(&operands_of(value), &mut uses);
                pending.push(*body);
            }
            Term::Join { handler, body, .. } => {
                pending.push(*handler);
                pending.push(*body);
            }
            Term::Jump { join, args } => {
                jumps[join.0 as usize] += 1;
                use_all(args, &mut uses);
            }
            Term::If {
                test,
                then,
                otherwise,
            } => {
                use_all(&[*test], &mut uses);
                pending.push(*then);
                pending.push(*otherwise);
            }
            Term::Return(x) => use_all(&[*x], &mut uses),
            Term::TailCall { callee, args, .. } => {
                use_all(&[*callee], &mut uses);
                use_all(args, &mut uses);
            }
        }
    }
    (uses, jumps)
}

/// `value` with each variable replaced as `subst` says.
fn substitute_value(value: &Value, sub: &dyn Fn(Operand) -> Operand) -> Value {
    let all = |xs: &[Operand]| xs.iter().map(|&x| sub(x)).collect::<Vec<_>>();
    match value {
        Value::Operand(x) => Value::Operand(sub(*x)),
        Value::Primitive { op, args } => Value::Primitive {
            op: *op,
            args: all(args),
        },
        Value::Call { callee, args } => Value::Call {
            callee: sub(*callee),
            args: all(args),
        },
        Value::Closure { function, free } => Value::Closure {
            function: *function,
            free: all(free),
        },
        Value::Global(g) => Value::Global(*g),
        Value::SetGlobal(g, x) => Value::SetGlobal(*g, sub(*x)),
        Value::Box(x) => Value::Box(sub(*x)),
        Value::Unbox(x) => Value::Unbox(sub(*x)),
        Value::SetBox(a, b) => Value::SetBox(sub(*a), sub(*b)),
        Value::SetFree {
            closure,
            index,
            value,
        } => Value::SetFree {
            closure: sub(*closure),
            index: *index,
            value: sub(*value),
        },
    }
}

/// One round over function `n` of `module`. Returns whether it changed.
pub(crate) fn simplify(module: &mut Module, n: usize) -> bool {
    let f = &module.functions[n];
    let (uses, jumps) = count(f);
    let mut subst: Vec<Option<Operand>> = vec![None; f.vars as usize];
    let mut pool = Pool {
        base: &module.constants,
        added: Vec::new(),
    };
    let mut b = Builder::default();
    let mut changed = false;
    let mut pending: Vec<(TermId, Hole)> = vec![(f.body, (HOLE, Field::Root))];
    while let Some((mut id, mut hole)) = pending.pop() {
        loop {
            let sub = |x: Operand| match x {
                Operand::Var(v) => subst[v.0 as usize].unwrap_or(x),
                Operand::Const(_) => x,
            };
            match f.term(id) {
                Term::Let {
                    var,
                    value,
                    body,
                    position,
                } => {
                    let value = substitute_value(value, &sub);
                    let replaced = match &value {
                        Value::Operand(x) => Some(*x),
                        _ => fold(&value, &mut pool),
                    };
                    if let Some(x) = replaced {
                        subst[var.0 as usize] = Some(x);
                        changed = true;
                    } else if uses[var.0 as usize] == 0 && removable(&value) {
                        changed = true;
                    } else {
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
                    }
                    id = *body;
                }
                Term::Join {
                    join,
                    params,
                    recursive,
                    handler,
                    body,
                } => {
                    if jumps[join.0 as usize] == 0 {
                        changed = true;
                    } else if let (false, 1, Term::Jump { join: to, args }) =
                        (*recursive, jumps[join.0 as usize], f.term(*body))
                        && to == join
                    {
                        // `join j(p) = h in jump j(a)`: just `h`, with `a` for `p`.
                        let args: Vec<Operand> = args.iter().map(|&a| sub(a)).collect();
                        for (p, a) in params.iter().zip(args) {
                            subst[p.0 as usize] = Some(a);
                        }
                        changed = true;
                        id = *handler;
                        continue;
                    } else {
                        let at = b.place(
                            hole,
                            Term::Join {
                                join: *join,
                                params: params.clone(),
                                recursive: *recursive,
                                handler: HOLE,
                                body: HOLE,
                            },
                        );
                        pending.push((*handler, (at, Field::JoinHandler)));
                        hole = (at, Field::JoinBody);
                    }
                    id = *body;
                }
                Term::If {
                    test,
                    then,
                    otherwise,
                } => match sub(*test) {
                    Operand::Const(c) => {
                        changed = true;
                        let taken = !matches!(pool.get(c), Constant::Boolean(false));
                        id = if taken { *then } else { *otherwise };
                    }
                    test => {
                        let at = b.place(
                            hole,
                            Term::If {
                                test,
                                then: HOLE,
                                otherwise: HOLE,
                            },
                        );
                        pending.push((*otherwise, (at, Field::Otherwise)));
                        hole = (at, Field::Then);
                        id = *then;
                    }
                },
                Term::Jump { join, args } => {
                    let args = args.iter().map(|&x| sub(x)).collect();
                    b.place(hole, jump(*join, args));
                    break;
                }
                Term::Return(x) => {
                    b.place(hole, Term::Return(sub(*x)));
                    break;
                }
                Term::TailCall {
                    callee,
                    args,
                    position,
                } => {
                    let args = args.iter().map(|&x| sub(x)).collect();
                    b.place(
                        hole,
                        Term::TailCall {
                            callee: sub(*callee),
                            args,
                            position: *position,
                        },
                    );
                    break;
                }
            }
        }
    }
    let added = pool.added;
    let root = b.root.expect("a body is always placed");
    let f = &mut module.functions[n];
    f.terms = b.terms;
    f.body = root;
    module.constants.extend(added);
    changed
}

fn jump(join: JoinId, args: Vec<Operand>) -> Term {
    Term::Jump { join, args }
}
