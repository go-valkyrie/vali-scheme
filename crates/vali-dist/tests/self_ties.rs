//! The validator's rules for a closure's self-ties (ADR 0017 #13): for
//! `let k = closure g(..)`, every `set-free k i k` is an ancestor of every
//! other use of `k`, `set-free` names only closures made in its own
//! function, and a self-tied slot is written once.

use vali_dist::{
    Arity, Constant, ConstantId, FunId, Function, JoinId, Module, Operand, Primitive, Term, TermId,
    Value, Var, validate,
};

fn v(x: Var) -> Operand {
    Operand::Var(x)
}

fn zero() -> Operand {
    Operand::Const(ConstantId(0))
}

/// A function's arena, built bottom-up, with a variable and join counter.
#[derive(Default)]
struct B {
    terms: Vec<Term>,
    vars: u32,
}

impl B {
    fn var(&mut self) -> Var {
        self.vars += 1;
        Var(self.vars - 1)
    }

    fn push(&mut self, term: Term) -> TermId {
        self.terms.push(term);
        TermId(self.terms.len() as u32 - 1)
    }

    fn let_(&mut self, var: Var, value: Value, body: TermId) -> TermId {
        self.push(Term::Let {
            var,
            value,
            body,
            position: None,
        })
    }

    fn ret(&mut self, x: Operand) -> TermId {
        self.push(Term::Return(x))
    }

    /// `let k = closure f1(0, 0) in body`.
    fn closure(&mut self, k: Var, body: TermId) -> TermId {
        self.closure_of(k, [zero(), zero()], body)
    }

    fn closure_of(&mut self, k: Var, free: [Operand; 2], body: TermId) -> TermId {
        self.let_(
            k,
            Value::Closure {
                function: FunId(1),
                free: free.to_vec(),
            },
            body,
        )
    }

    /// `let _ = set-free k i value in body`.
    fn set(&mut self, k: Var, index: u32, value: Operand, body: TermId) -> TermId {
        let u = self.var();
        self.let_(
            u,
            Value::SetFree {
                closure: v(k),
                index,
                value,
            },
            body,
        )
    }

    /// `let _ = set-free k i k in body`.
    fn tie(&mut self, k: Var, index: u32, body: TermId) -> TermId {
        self.set(k, index, v(k), body)
    }

    /// `let r = call k() in body`.
    fn call(&mut self, k: Var, body: impl FnOnce(&mut B, Var) -> TermId) -> TermId {
        let r = self.var();
        let rest = body(self, r);
        self.let_(
            r,
            Value::Call {
                callee: v(k),
                args: vec![],
            },
            rest,
        )
    }

    fn function(self, arity: Arity, params: Vec<Var>, free: Vec<Var>, body: TermId) -> Function {
        Function {
            name: None,
            arity,
            params,
            free,
            vars: self.vars,
            joins: 1,
            terms: self.terms,
            body,
            inlined: vec![],
        }
    }
}

/// A module of `f0`, which `build` makes, and `f1`, which takes two free
/// variables and returns 0.
fn module(build: impl FnOnce(&mut B) -> TermId) -> Module {
    let mut b = B::default();
    let body = build(&mut b);
    let f0 = b.function(Arity::exactly(0), vec![], vec![], body);

    let mut b = B::default();
    let free = vec![b.var(), b.var()];
    let body = b.ret(zero());
    let f1 = b.function(Arity::exactly(0), vec![], free, body);

    Module {
        name: "self-ties".into(),
        constants: vec![Constant::Integer(0)],
        globals: vec![],
        exports: vec![],
        functions: vec![f0, f1],
        body: FunId(0),
        sources: vec![],
    }
}

fn accepted(build: impl FnOnce(&mut B) -> TermId) {
    let m = module(build);
    validate(&m).unwrap_or_else(|e| panic!("{e}\n{m}"));
}

/// Refused with a message naming the function and containing `part`.
fn refused(build: impl FnOnce(&mut B) -> TermId, part: &str) {
    let m = module(build);
    let error = validate(&m).expect_err(&format!("accepted:\n{m}"));
    let message = error.to_string();
    assert!(message.starts_with("function f0: "), "{message}");
    assert!(message.contains(part), "{message}");
}

const UNTIED: &str = "not yet tied to itself";

#[test]
fn a_tied_closure_is_called_after() {
    accepted(|b| {
        let (k, r) = (b.var(), b.var());
        let done = b.ret(v(r));
        let call = b.let_(
            r,
            Value::Call {
                callee: v(k),
                args: vec![],
            },
            done,
        );
        let tied = b.tie(k, 0, call);
        b.closure(k, tied)
    });
}

#[test]
fn a_tail_call_after_the_tie_is_a_named_let() {
    accepted(|b| {
        let k = b.var();
        let call = b.push(Term::TailCall {
            callee: v(k),
            args: vec![],
            position: None,
        });
        let tied = b.tie(k, 0, call);
        b.closure(k, tied)
    });
}

#[test]
fn a_call_before_the_tie_is_refused() {
    refused(
        |b| {
            let k = b.var();
            let done = b.ret(zero());
            let tied = b.tie(k, 0, done);
            let called = b.call(k, |_, _| tied);
            b.closure(k, called)
        },
        UNTIED,
    );
}

#[test]
fn an_argument_before_the_tie_is_refused() {
    refused(
        |b| {
            let (k, p) = (b.var(), b.var());
            let done = b.ret(zero());
            let tied = b.tie(k, 0, done);
            let pair = b.let_(
                p,
                Value::Primitive {
                    op: Primitive::Cons,
                    args: vec![v(k), zero()],
                },
                tied,
            );
            b.closure(k, pair)
        },
        UNTIED,
    );
}

#[test]
fn a_return_before_the_tie_is_refused() {
    // Tied in one branch, returned in the other.
    refused(
        |b| {
            let (k, t) = (b.var(), b.var());
            let done = b.ret(zero());
            let then = b.tie(k, 0, done);
            let otherwise = b.ret(v(k));
            let branch = b.push(Term::If {
                test: v(t),
                then,
                otherwise,
            });
            let test = b.let_(
                t,
                Value::Primitive {
                    op: Primitive::Lt,
                    args: vec![zero(), zero()],
                },
                branch,
            );
            b.closure(k, test)
        },
        UNTIED,
    );
}

#[test]
fn another_closures_free_operand_before_the_tie_is_refused() {
    refused(
        |b| {
            let (k, m) = (b.var(), b.var());
            let done = b.ret(zero());
            let tied = b.tie(k, 0, done);
            let held = b.closure_of(m, [v(k), zero()], tied);
            b.closure(k, held)
        },
        UNTIED,
    );
}

#[test]
fn a_set_free_value_before_the_tie_is_refused() {
    refused(
        |b| {
            let (k, m) = (b.var(), b.var());
            let done = b.ret(zero());
            let tied = b.tie(k, 0, done);
            let patched = b.set(m, 1, v(k), tied);
            let made = b.closure(k, patched);
            b.closure(m, made)
        },
        UNTIED,
    );
}

#[test]
fn a_set_free_on_the_closure_before_its_tie_is_refused() {
    // Writing another slot is a use too: nothing may name `k` before it
    // is tied.
    refused(
        |b| {
            let k = b.var();
            let done = b.ret(zero());
            let tied = b.tie(k, 0, done);
            let patched = b.set(k, 1, zero(), tied);
            b.closure(k, patched)
        },
        UNTIED,
    );
}

#[test]
fn a_tie_in_the_other_branch_is_refused() {
    refused(
        |b| {
            let (k, t) = (b.var(), b.var());
            let done = b.ret(zero());
            let then = b.tie(k, 0, done);
            let after = b.ret(zero());
            let otherwise = b.call(k, |_, _| after);
            let branch = b.push(Term::If {
                test: v(t),
                then,
                otherwise,
            });
            let test = b.let_(
                t,
                Value::Primitive {
                    op: Primitive::Lt,
                    args: vec![zero(), zero()],
                },
                branch,
            );
            b.closure(k, test)
        },
        UNTIED,
    );
}

/// `join j() = handler in body`, where `body` jumps to `j`.
fn join(b: &mut B, handler: TermId, body: impl FnOnce(&mut B, TermId) -> TermId) -> TermId {
    let jump = b.push(Term::Jump {
        join: JoinId(0),
        args: vec![],
    });
    let body = body(b, jump);
    b.push(Term::Join {
        join: JoinId(0),
        params: vec![],
        recursive: false,
        handler,
        body,
    })
}

#[test]
fn a_tie_in_a_join_body_does_not_cover_its_handler() {
    refused(
        |b| {
            let k = b.var();
            let after = b.ret(zero());
            let handler = b.call(k, |_, _| after);
            let joined = join(b, handler, |b, jump| b.tie(k, 0, jump));
            b.closure(k, joined)
        },
        UNTIED,
    );
}

#[test]
fn a_tie_before_a_join_covers_its_handler_and_body() {
    accepted(|b| {
        let k = b.var();
        let after = b.ret(zero());
        let handler = b.call(k, |_, _| after);
        let joined = join(b, handler, |b, jump| b.call(k, |_, _| jump));
        let tied = b.tie(k, 0, joined);
        b.closure(k, tied)
    });
}

#[test]
fn a_tie_in_a_join_body_covers_what_follows_it_there() {
    accepted(|b| {
        let k = b.var();
        let handler = b.ret(zero());
        let joined = join(b, handler, |b, jump| {
            let call = b.call(k, |_, _| jump);
            b.tie(k, 0, call)
        });
        b.closure(k, joined)
    });
}

/// `module` with an `f2` of one parameter and one free variable, whose
/// body is `build(parameter, free variable)`.
fn with_f2(build: impl FnOnce(&mut B, Var, Var) -> TermId) -> Module {
    let mut m = module(|b| b.ret(zero()));
    let mut b = B::default();
    let (p, x) = (b.var(), b.var());
    let body = build(&mut b, p, x);
    m.functions
        .push(b.function(Arity::exactly(1), vec![p], vec![x], body));
    m
}

#[test]
fn set_free_on_a_parameter_is_refused() {
    let m = with_f2(|b, p, _| {
        let done = b.ret(zero());
        b.set(p, 0, zero(), done)
    });
    assert_eq!(
        validate(&m).unwrap_err().to_string(),
        "function f2: set-free of v0, which is not a closure made in this function"
    );
}

#[test]
fn set_free_on_a_free_variable_is_refused() {
    let m = with_f2(|b, _, x| {
        let done = b.ret(zero());
        b.set(x, 0, zero(), done)
    });
    assert_eq!(
        validate(&m).unwrap_err().to_string(),
        "function f2: set-free of v1, which is not a closure made in this function"
    );
}

#[test]
fn set_free_on_a_copy_of_a_closure_is_refused() {
    // The rule names the variable the closure let binds, not a copy.
    refused(
        |b| {
            let (k, a) = (b.var(), b.var());
            let done = b.ret(zero());
            let tied = b.set(a, 0, zero(), done);
            let copy = b.let_(a, Value::Operand(v(k)), tied);
            b.closure(k, copy)
        },
        "set-free of v1, which is not a closure made in this function",
    );
}

#[test]
fn set_free_on_a_constant_is_refused() {
    refused(
        |b| {
            let u = b.var();
            let done = b.ret(zero());
            b.let_(
                u,
                Value::SetFree {
                    closure: zero(),
                    index: 0,
                    value: zero(),
                },
                done,
            )
        },
        "set-free of a constant",
    );
}

#[test]
fn a_second_write_to_a_self_tied_slot_is_refused() {
    refused(
        |b| {
            let k = b.var();
            let done = b.ret(zero());
            let again = b.set(k, 0, zero(), done);
            let tied = b.tie(k, 0, again);
            b.closure(k, tied)
        },
        "free variable 0 of v0, which is tied to itself, is written again",
    );
}

#[test]
fn a_write_before_a_self_tie_of_the_slot_is_refused_too() {
    refused(
        |b| {
            let k = b.var();
            let done = b.ret(zero());
            let tied = b.tie(k, 0, done);
            // The write names `k` before its tie, and the slot twice.
            let first = b.set(k, 0, zero(), tied);
            b.closure(k, first)
        },
        "is written again",
    );
}

#[test]
fn tying_a_slot_twice_is_refused() {
    refused(
        |b| {
            let k = b.var();
            let done = b.ret(zero());
            let again = b.tie(k, 0, done);
            let tied = b.tie(k, 0, again);
            b.closure(k, tied)
        },
        "is written again",
    );
}

#[test]
fn another_slot_may_be_written_after_the_tie() {
    accepted(|b| {
        let k = b.var();
        let done = b.ret(v(k));
        let patched = b.set(k, 1, zero(), done);
        let tied = b.tie(k, 0, patched);
        b.closure(k, tied)
    });
}

#[test]
fn a_closure_with_two_ties_is_used_after_both() {
    accepted(|b| {
        let k = b.var();
        let done = b.ret(v(k));
        let second = b.tie(k, 1, done);
        let first = b.tie(k, 0, second);
        b.closure(k, first)
    });
    refused(
        |b| {
            let k = b.var();
            let done = b.ret(zero());
            let second = b.tie(k, 1, done);
            let between = b.call(k, |_, _| second);
            let first = b.tie(k, 0, between);
            b.closure(k, first)
        },
        UNTIED,
    );
}

#[test]
fn two_ties_in_different_branches_cover_no_use() {
    refused(
        |b| {
            let (k, t) = (b.var(), b.var());
            let done = b.ret(zero());
            let then = b.tie(k, 0, done);
            let done = b.ret(v(k));
            let otherwise = b.tie(k, 1, done);
            let branch = b.push(Term::If {
                test: v(t),
                then,
                otherwise,
            });
            let test = b.let_(
                t,
                Value::Primitive {
                    op: Primitive::Lt,
                    args: vec![zero(), zero()],
                },
                branch,
            );
            b.closure(k, test)
        },
        UNTIED,
    );
}

#[test]
fn mutual_recursion_is_tied_in_order() {
    // What `letrec` lowers to: each closure ties itself, then is patched
    // into the earlier ones.
    accepted(|b| {
        let (f, g) = (b.var(), b.var());
        let done = b.ret(v(f));
        let patched = b.set(f, 1, v(g), done);
        let g_tied = b.tie(g, 0, patched);
        let g_made = b.closure_of(g, [v(f), zero()], g_tied);
        let f_tied = b.tie(f, 0, g_made);
        b.closure(f, f_tied)
    });
}

#[test]
fn mutual_recursion_patched_before_its_own_tie_is_refused() {
    // The order `letrec` used to emit: `f` patched with `g` first.
    refused(
        |b| {
            let (f, g) = (b.var(), b.var());
            let done = b.ret(v(f));
            let g_tied = b.tie(g, 0, done);
            let patched = b.set(f, 1, v(g), g_tied);
            let g_made = b.closure_of(g, [v(f), zero()], patched);
            let f_tied = b.tie(f, 0, g_made);
            b.closure(f, f_tied)
        },
        "v1 is used at",
    );
}

#[test]
fn a_letrec_star_patch_that_runs_a_closure_early_is_legal() {
    // `(letrec* ((f (lambda () (g))) (x (f)) (g (lambda () ...))) ...)`:
    // `f` is called before it is patched with `g`, which only a run can
    // tell, so the validator accepts it.
    accepted(|b| {
        let (f, g) = (b.var(), b.var());
        let done = b.ret(v(g));
        let patched = b.set(f, 1, v(g), done);
        let g_tied = b.tie(g, 0, patched);
        let g_made = b.closure(g, g_tied);
        let called = b.call(f, |_, _| g_made);
        let f_tied = b.tie(f, 0, called);
        b.closure(f, f_tied)
    });
}

/// How long validating a closure with `n` self-ties at distinct indices
/// and `n` later uses takes, at best of three runs.
fn tie_scaling_time(n: u32) -> std::time::Duration {
    let m = module(|b| {
        let k = b.var();
        let mut body = b.ret(v(k));
        for _ in 0..n {
            let a = b.var();
            body = b.let_(a, Value::Operand(v(k)), body);
        }
        for i in 0..n {
            body = b.tie(k, i, body);
        }
        b.closure(k, body)
    });
    (0..3)
        .map(|_| {
            let start = std::time::Instant::now();
            validate(&m).unwrap();
            start.elapsed()
        })
        .min()
        .unwrap()
}

/// The check is linear in the terms: four times the ties and uses costs
/// about four times as much, where a rescan of the ties for each use would
/// cost sixteen times. The bound is generous, between the two.
#[test]
fn many_ties_and_uses_validate_in_linear_time() {
    let small = tie_scaling_time(4_000);
    let large = tie_scaling_time(16_000);
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(1e-6);
    eprintln!("4000 ties and uses: {small:?}, 16000: {large:?}, ratio {ratio:.1}");
    assert!(ratio < 10.0, "4x the input took {ratio:.1}x as long");
}
