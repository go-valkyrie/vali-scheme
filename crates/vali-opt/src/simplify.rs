//! Simplification: constant folding, copy propagation, `if`s on
//! constants, and removing what is unused.
//!
//! One round counts each variable's uses and each join point's jumps in
//! what the function's body reaches, then rebuilds the body top-down:
//!
//! - a `let` whose value folds to a constant, or is just an operand, is
//!   dropped, and its variable replaced by that operand where it is used;
//! - a `let` whose variable is unused is dropped if its value may only
//!   allocate or read (ADR 0017 #4): raising, writing and capturing stay,
//!   but reading a known procedure's global cannot raise;
//! - an `if` on a constant keeps only the arm it takes;
//! - a join point nothing jumps to keeps only its body, and one jumped to
//!   once, not a loop, goes: its handler takes the place of the jump, with
//!   the jump's arguments for its parameters. The jump is inside the join
//!   point's scope, so whatever the handler uses is in scope there too.
//!   Only a jump in the same loop as the join point is replaced: moving
//!   code into a loop would keep what it uses live through the loop.
//!
//! What a dropped term made unused is found by the next round.

use vali_dist::{
    Constant, ConstantId, Effects, FunId, Function, JoinId, Module, Operand, Primitive, Term,
    TermId, Value, Var,
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
        // `car`, `cdr` and `vector-ref` of a constant are not folded: a
        // quoted pair or vector can be mutated (`set-car!`, `vector-set!`),
        // so its contents at compile time need not be its contents when the
        // read runs. Its type and a vector's length cannot change, so
        // `pair?` and `vector-length` fold. Constants are immutable now
        // (ADR 0020), so these folds could return, once every tier checks
        // writes; and no pass may ever fold an allocation (`cons`, `list`,
        // `vector`, `make-vector`) into a constant, which would make mutable
        // data immutable. `allocations_are_not_folded_into_constants` tests
        // it.
        Primitive::VectorLength => match constant(pool, 0)? {
            Constant::Vector(ref items) => Constant::Integer(i64::try_from(items.len()).ok()?),
            _ => return None,
        },
        _ => return None,
    };
    Some(Operand::Const(pool.add(folded)))
}

/// Whether a `let` of `value` can go when its variable is unused. `known`
/// says which globals hold known procedures, whose reads cannot raise.
fn removable(value: &Value, known: &[Option<FunId>]) -> bool {
    if let Value::Global(g) = value
        && known[g.0 as usize].is_some()
    {
        return true;
    }
    let effects = value.effects();
    !effects.contains(Effects::WRITE)
        && !effects.contains(Effects::RAISE)
        && !effects.contains(Effects::CAPTURE)
}

use crate::operands as operands_of;

/// How often each variable is used, and each join point jumped to, in
/// what `f`'s body reaches; and for each join point, whether its jumps
/// are all in the loop it is in.
fn count(f: &Function) -> (Vec<u32>, Vec<u32>, Vec<bool>) {
    let mut uses = vec![0u32; f.vars as usize];
    let mut jumps = vec![0u32; f.joins as usize];
    // How many loops' handlers each join point, and each jump, is in.
    let mut depth = vec![0u32; f.joins as usize];
    let mut level = vec![true; f.joins as usize];
    let use_all = |xs: &[Operand], uses: &mut Vec<u32>| {
        for x in xs {
            if let Operand::Var(v) = x {
                uses[v.0 as usize] += 1;
            }
        }
    };
    let mut pending = vec![(f.body, 0u32)];
    while let Some((id, d)) = pending.pop() {
        match f.term(id) {
            Term::Let { value, body, .. } => {
                use_all(&operands_of(value), &mut uses);
                pending.push((*body, d));
            }
            Term::Join {
                join,
                recursive,
                handler,
                body,
                ..
            } => {
                depth[join.0 as usize] = d;
                pending.push((*handler, d + u32::from(*recursive)));
                pending.push((*body, d));
            }
            Term::Jump { join, args } => {
                jumps[join.0 as usize] += 1;
                if depth[join.0 as usize] != d {
                    level[join.0 as usize] = false;
                }
                use_all(args, &mut uses);
            }
            Term::If {
                test,
                then,
                otherwise,
            } => {
                use_all(&[*test], &mut uses);
                pending.push((*then, d));
                pending.push((*otherwise, d));
            }
            Term::Return(x) => use_all(&[*x], &mut uses),
            Term::TailCall { callee, args, .. } => {
                use_all(&[*callee], &mut uses);
                use_all(args, &mut uses);
            }
            Term::TailCallDirect { args, .. } => use_all(args, &mut uses),
        }
    }
    (uses, jumps, level)
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
        Value::CallDirect { function, args } => Value::CallDirect {
            function: *function,
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

/// One round over function `n` of `module`, whose known procedures'
/// globals are `known`. Returns whether it changed.
pub(crate) fn simplify(module: &mut Module, n: usize, known: &[Option<FunId>]) -> bool {
    let f = &module.functions[n];
    let (uses, jumps, level) = count(f);
    let mut subst: Vec<Option<Operand>> = vec![None; f.vars as usize];
    let mut pool = Pool {
        base: &module.constants,
        added: Vec::new(),
    };
    let mut b = Builder::default();
    let mut changed = false;
    // The join points jumped to once, whose handlers replace the jumps.
    let mut moved: Vec<Option<(&[Var], TermId)>> = vec![None; f.joins as usize];
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
                    } else if uses[var.0 as usize] == 0 && removable(&value, known) {
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
                    } else if !*recursive && jumps[join.0 as usize] == 1 && level[join.0 as usize] {
                        moved[join.0 as usize] = Some((params, *handler));
                        changed = true;
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
                    let args: Vec<Operand> = args.iter().map(|&x| sub(x)).collect();
                    if let Some((params, handler)) = moved[join.0 as usize] {
                        for (p, a) in params.iter().zip(args) {
                            subst[p.0 as usize] = Some(a);
                        }
                        id = handler;
                        continue;
                    }
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
                Term::TailCallDirect {
                    function,
                    args,
                    position,
                } => {
                    let args = args.iter().map(|&x| sub(x)).collect();
                    b.place(
                        hole,
                        Term::TailCallDirect {
                            function: *function,
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
