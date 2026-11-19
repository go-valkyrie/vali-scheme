//! The converter's limits are enforced by `vali_dist::validate` (ADR 0017
//! #13): IR just over each limit is refused, IR at it converts, and so a
//! conversion failure can only be a converter bug.

use vali_dist::{
    self as ir, Arity, Constant, ConstantId, FunId, Function, Global, GlobalLink, Module, Operand,
    Primitive, Term, TermId, Value, Var,
};

#[derive(Default)]
struct Fb {
    terms: Vec<Term>,
    vars: u32,
}

impl Fb {
    fn var(&mut self) -> Var {
        self.vars += 1;
        Var(self.vars - 1)
    }

    fn push(&mut self, term: Term) -> TermId {
        self.terms.push(term);
        TermId(self.terms.len() as u32 - 1)
    }

    fn bind(&mut self, var: Var, value: Value, body: TermId) -> TermId {
        self.push(Term::Let {
            var,
            value,
            body,
            position: None,
        })
    }

    fn finish(self, arity: Arity, params: Vec<Var>, free: Vec<Var>, body: TermId) -> Function {
        Function {
            name: None,
            arity,
            params,
            free,
            vars: self.vars,
            joins: 0,
            terms: self.terms,
            body,
            inlined: Vec::new(),
        }
    }
}

fn module(constants: Vec<Constant>, globals: Vec<Global>, functions: Vec<Function>) -> Module {
    Module {
        name: "limits".into(),
        constants,
        globals,
        exports: Vec::new(),
        functions,
        body: FunId(0),
        sources: Vec::new(),
    }
}

fn c(n: u32) -> Operand {
    Operand::Const(ConstantId(n))
}

fn returns_const() -> Function {
    let mut fb = Fb::default();
    let body = fb.push(Term::Return(c(0)));
    fb.finish(Arity::exactly(0), vec![], vec![], body)
}

/// A body of `n` variables bound in a row.
fn variables(n: usize) -> Module {
    let mut fb = Fb::default();
    let vars: Vec<Var> = (0..n).map(|_| fb.var()).collect();
    let mut t = fb.push(Term::Return(Operand::Var(vars[n - 1])));
    for &x in vars.iter().rev() {
        t = fb.bind(
            x,
            Value::Primitive {
                op: Primitive::Add,
                args: vec![c(0), c(0)],
            },
            t,
        );
    }
    module(
        vec![Constant::Integer(1)],
        vec![],
        vec![fb.finish(Arity::exactly(0), vec![], vec![], t)],
    )
}

/// A body of `n` variables all live at once: each is read again by a sum
/// of them all.
fn live_variables(n: usize) -> Module {
    let mut fb = Fb::default();
    let xs: Vec<Var> = (0..n).map(|_| fb.var()).collect();
    let sums: Vec<Var> = (0..n).map(|_| fb.var()).collect();
    let mut t = fb.push(Term::Return(Operand::Var(sums[n - 1])));
    for i in (1..n).rev() {
        t = fb.bind(
            sums[i],
            Value::Primitive {
                op: Primitive::Add,
                args: vec![Operand::Var(sums[i - 1]), Operand::Var(xs[i])],
            },
            t,
        );
    }
    t = fb.bind(sums[0], Value::Operand(Operand::Var(xs[0])), t);
    for &x in xs.iter().rev() {
        t = fb.bind(
            x,
            Value::Primitive {
                op: Primitive::Add,
                args: vec![c(0), c(0)],
            },
            t,
        );
    }
    module(
        vec![Constant::Integer(1)],
        vec![],
        vec![fb.finish(Arity::exactly(0), vec![], vec![], t)],
    )
}

/// A body calling a closure with `n` arguments.
fn call_arguments(n: usize) -> Module {
    let mut fb = Fb::default();
    let k = fb.var();
    let r = fb.var();
    let ret = fb.push(Term::Return(Operand::Var(r)));
    let call = fb.bind(
        r,
        Value::Call {
            callee: Operand::Var(k),
            args: vec![c(0); n],
        },
        ret,
    );
    let body = fb.bind(
        k,
        Value::Closure {
            function: FunId(1),
            free: vec![],
        },
        call,
    );
    module(
        vec![Constant::Integer(1)],
        vec![],
        vec![
            fb.finish(Arity::exactly(0), vec![], vec![], body),
            returns_const(),
        ],
    )
}

/// A body tail-calling a closure with `n` arguments.
fn tail_call_arguments(n: usize) -> Module {
    let mut fb = Fb::default();
    let k = fb.var();
    let call = fb.push(Term::TailCall {
        callee: Operand::Var(k),
        args: vec![c(0); n],
        position: None,
    });
    let body = fb.bind(
        k,
        Value::Closure {
            function: FunId(1),
            free: vec![],
        },
        call,
    );
    module(
        vec![Constant::Integer(1)],
        vec![],
        vec![
            fb.finish(Arity::exactly(0), vec![], vec![], body),
            returns_const(),
        ],
    )
}

/// A body making a closure of a function with `n` free variables.
fn free_variables(n: usize) -> Module {
    let mut fb = Fb::default();
    let k = fb.var();
    let ret = fb.push(Term::Return(Operand::Var(k)));
    let body = fb.bind(
        k,
        Value::Closure {
            function: FunId(1),
            free: vec![c(0); n],
        },
        ret,
    );
    let mut gb = Fb::default();
    let free: Vec<Var> = (0..n).map(|_| gb.var()).collect();
    let inner = gb.push(Term::Return(c(0)));
    module(
        vec![Constant::Integer(1)],
        vec![],
        vec![
            fb.finish(Arity::exactly(0), vec![], vec![], body),
            gb.finish(Arity::exactly(0), vec![], free, inner),
        ],
    )
}

/// A body of `n` `if`s on a constant, which bind nothing: a lot of code
/// for no slots.
fn terms(n: usize) -> Module {
    let mut fb = Fb::default();
    let mut t = fb.push(Term::Return(c(0)));
    for _ in 0..n {
        let leaf = fb.push(Term::Return(c(0)));
        t = fb.push(Term::If {
            test: c(0),
            then: leaf,
            otherwise: t,
        });
    }
    module(
        vec![Constant::Integer(1)],
        vec![],
        vec![fb.finish(Arity::exactly(0), vec![], vec![], t)],
    )
}

/// A body of one `set-free` at `index`, never run.
fn set_free(index: u32) -> Module {
    let mut fb = Fb::default();
    let k = fb.var();
    let u = fb.var();
    let ret = fb.push(Term::Return(c(0)));
    let set = fb.bind(
        u,
        Value::SetFree {
            closure: Operand::Var(k),
            index,
            value: c(0),
        },
        ret,
    );
    let body = fb.bind(
        k,
        Value::Closure {
            function: FunId(1),
            free: vec![],
        },
        set,
    );
    module(
        vec![Constant::Integer(1)],
        vec![],
        vec![
            fb.finish(Arity::exactly(0), vec![], vec![], body),
            returns_const(),
        ],
    )
}

fn converts(m: &Module) {
    ir::validate(m).expect("validates");
    let converted = vali_bytecode::convert(m).expect("converts");
    vali_bytecode::validate(&converted).expect("valid run format");
}

fn refused(m: &Module, why: &str) {
    let error = ir::validate(m).expect_err("is refused").to_string();
    assert!(error.contains(why), "{error}");
}

/// The most `n` in `lo..hi` for which `make(n)` validates, given that
/// `lo` does and `hi` does not; it converts, and one more is refused.
fn boundary(make: impl Fn(usize) -> Module, lo: usize, hi: usize, why: &str) -> usize {
    let (mut lo, mut hi) = (lo, hi);
    assert!(ir::validate(&make(lo)).is_ok());
    assert!(ir::validate(&make(hi)).is_err());
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if ir::validate(&make(mid)).is_ok() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    converts(&make(lo));
    refused(&make(lo + 1), why);
    lo
}

#[test]
fn constants() {
    let make = |n: usize| {
        module(
            (0..n as i64).map(Constant::Integer).collect(),
            vec![],
            vec![returns_const()],
        )
    };
    converts(&make(ir::MAX_CONSTANTS));
    refused(&make(ir::MAX_CONSTANTS + 1), "constants");
}

#[test]
fn globals_and_sources() {
    let make = |n: usize| {
        module(
            vec![Constant::Integer(1)],
            (0..n)
                .map(|i| Global {
                    name: format!("g{i}").into(),
                    link: GlobalLink::Defined,
                })
                .collect(),
            vec![returns_const()],
        )
    };
    converts(&make(ir::MAX_POOL));
    refused(&make(ir::MAX_POOL + 1), "globals");
    let mut m = make(0);
    m.sources = (0..ir::MAX_POOL).map(|i| format!("s{i}").into()).collect();
    converts(&m);
    m.sources.push("one more".into());
    refused(&m, "sources");
}

#[test]
fn slots_by_live_variables() {
    let most = boundary(live_variables, 60_000, 70_000, "slots");
    let converted = vali_bytecode::convert(&live_variables(most)).unwrap();
    assert!(usize::from(converted.functions[0].slots) <= ir::MAX_SLOT + 1);
}

/// Variables that are not live together take few slots, however many.
#[test]
fn many_variables_not_live_together() {
    converts(&variables(200_000));
}

#[test]
fn slots_by_call_arguments() {
    boundary(call_arguments, 1_000, 70_000, "slots");
    boundary(tail_call_arguments, 1_000, 70_000, "slots");
}

#[test]
fn free_variables_and_slots() {
    boundary(free_variables, 1_000, 70_000, "slots");
}

#[test]
fn call_arguments_above_a_u16() {
    refused(&call_arguments(65_536), "arguments");
    refused(&tail_call_arguments(65_536), "arguments");
}

#[test]
fn free_variables_above_a_u16() {
    let mut m = free_variables(1);
    m.functions[1].free = (0..65_536).map(Var).collect();
    m.functions[1].vars = 65_536;
    refused(&m, "free variables");
}

#[test]
fn set_free_index() {
    converts(&set_free(65_535));
    refused(&set_free(65_536), "set-free");
}

#[test]
fn code_words() {
    boundary(terms, 100_000, 1_000_000, "words");
}

#[test]
fn parameters_are_limited_by_the_arity() {
    // An arity binds at most 511 parameters, so there are never too many.
    let mut fb = Fb::default();
    let params: Vec<Var> = (0..511).map(|_| fb.var()).collect();
    let body = fb.push(Term::Return(c(0)));
    let mut m = module(
        vec![Constant::Integer(1)],
        vec![],
        vec![
            returns_const(),
            fb.finish(
                Arity {
                    required: 255,
                    optional: 255,
                    rest: true,
                },
                params,
                vec![],
                body,
            ),
        ],
    );
    converts(&m);
    m.functions[1].vars += 1;
    assert!(ir::validate(&m).is_err());
}

#[test]
fn the_jump_range_is_the_bound() {
    assert_eq!(
        i64::from(*vali_bytecode::JUMP_RANGE.end()),
        ir::MAX_CODE_WORDS as i64
    );
}
