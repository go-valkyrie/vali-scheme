//! Adversarial tests of the IR to run-format path (ADR 0017 #13): a module
//! that passes `vali_dist::validate` must convert without panicking, convert
//! to bytecode that passes `vali_bytecode::validate`, and run to the result an
//! independent IR interpreter gives, under every memory strategy.
//!
//! Two kinds of test:
//!
//! - Targeted cases, each a module built by hand around one assumption the
//!   converter or the module builder makes. Those that show a known bug are
//!   ignored with the bug as the reason, so they run with `--ignored` and
//!   pass once it is fixed.
//! - `random_valid_ir`: random valid, terminating modules (lets, ifs, joins,
//!   loops, calls and tail calls, closures, `set-free`, boxes, vectors,
//!   globals and up to 250 live values), each compared with the reference
//!   interpreter below. `IR_SEEDS` and `IR_FIRST` choose the seeds,
//!   `IR_SEED` with `one_seed` runs one, and `IR_PRINT` and `IR_TRACE` show
//!   the module and progress.
//!
//! Slow or process-killing cases are ignored too; run them alone, in
//! release.

#![allow(missing_debug_implementations)]

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::time::Instant;

use vali_dist::{
    self as ir, Arity, Constant, ConstantId, FunId, Function, Global, GlobalId, GlobalLink, JoinId,
    Module, Operand, Primitive, Term, TermId, Value, Var,
};
use vali_runtime::{
    Cx, DeferredHeadered, DeferredSideCounts, HeapConfig, NoCollection, PreciseHeadered,
    PreciseSideCounts, Strategy,
};
use vali_test::fuzz::{Random, XorShift};

// ---------------------------------------------------------------------------
// Building IR by hand.

#[derive(Default)]
struct FB {
    terms: Vec<Term>,
    vars: u32,
    joins: u32,
}

impl FB {
    fn var(&mut self) -> Var {
        self.vars += 1;
        Var(self.vars - 1)
    }
    fn join(&mut self) -> JoinId {
        self.joins += 1;
        JoinId(self.joins - 1)
    }
    fn push(&mut self, t: Term) -> TermId {
        self.terms.push(t);
        TermId(self.terms.len() as u32 - 1)
    }
    fn finish(
        self,
        name: &str,
        arity: Arity,
        params: Vec<Var>,
        free: Vec<Var>,
        body: TermId,
    ) -> Function {
        Function {
            name: Some(name.into()),
            arity,
            params,
            free,
            vars: self.vars,
            joins: self.joins,
            terms: self.terms,
            body,
            inlined: Vec::new(),
        }
    }
}

fn let_(fb: &mut FB, var: Var, value: Value, body: TermId) -> TermId {
    fb.push(Term::Let {
        var,
        value,
        body,
        position: None,
    })
}

fn module(constants: Vec<Constant>, globals: Vec<Global>, functions: Vec<Function>) -> Module {
    Module {
        name: "adversarial".into(),
        constants,
        globals,
        exports: Vec::new(),
        functions,
        body: FunId(0),
        sources: Vec::new(),
    }
}

fn v(x: Var) -> Operand {
    Operand::Var(x)
}
fn c(n: u32) -> Operand {
    Operand::Const(ConstantId(n))
}

/// Validates, converts (catching panics), validates the bytecode.
fn convert_checked(m: &Module) -> Result<vali_bytecode::Module, String> {
    ir::validate(m).map_err(|e| format!("IR invalid: {e}"))?;
    let out = catch_unwind(AssertUnwindSafe(|| vali_bytecode::convert(m)))
        .map_err(|p| format!("CONVERT PANICKED: {}", panic_message(&p)))?
        .map_err(|e| format!("convert error: {e}"))?;
    vali_bytecode::validate(&out).map_err(|e| format!("BYTECODE INVALID: {e}"))?;
    Ok(out)
}

fn dis(m: &vali_bytecode::Module) -> String {
    m.functions
        .iter()
        .enumerate()
        .map(|(i, f)| {
            format!(
                "function {i} ({} slots):\n{}",
                f.slots,
                vali_bytecode::disassemble(f)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic".into())
}

fn run_with<S: Strategy>(m: &vali_bytecode::Module) -> Result<String, String> {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut cx = Cx::<S>::new(&HeapConfig {
            max_bytes: Some(256 << 20),
            ..Default::default()
        });
        cx.set_fuel_limit(Some(5_000_000));
        cx.set_max_stack_bytes(8 << 20);
        let id = cx.load(m.clone()).map_err(|e| format!("load: {e}"))?;
        let out = match cx.run(id) {
            Ok(h) => cx.write_string(&h).map_err(|e| format!("write: {e}")),
            Err(e) => Err(format!("error: {e}")),
        };
        cx.collect_garbage();
        out
    }));
    match r {
        Ok(x) => x,
        Err(p) => Err(format!(
            "VM PANICKED under {}: {}",
            S::NAME,
            panic_message(&p)
        )),
    }
}

fn run_all(m: &vali_bytecode::Module) -> Vec<(&'static str, Result<String, String>)> {
    vec![
        (NoCollection::NAME, run_with::<NoCollection>(m)),
        (DeferredHeadered::NAME, run_with::<DeferredHeadered>(m)),
        (DeferredSideCounts::NAME, run_with::<DeferredSideCounts>(m)),
        (PreciseHeadered::NAME, run_with::<PreciseHeadered>(m)),
        (PreciseSideCounts::NAME, run_with::<PreciseSideCounts>(m)),
    ]
}

// ---------------------------------------------------------------------------
// Targeted cases.

/// A closure over 256 free variables converts and runs, and every slot
/// holds the value it was given. f0 binds 256 distinct integers (n for slot
/// n-1), makes a closure of f1 over them in order, then calls it. f1 returns
/// the sum of n * (value in slot n-1), which depends on every slot and on
/// their order. Guards against the `Closure` instruction's encoder
/// panicking, the converter rejecting the count, or the wide form capturing
/// the wrong values.
#[test]
fn closure_with_256_free_variables() {
    const N: u32 = 256;
    let mut fb = FB::default();
    let xs: Vec<Var> = (0..N).map(|_| fb.var()).collect();
    let k = fb.var();
    let res = fb.var();
    let ret = fb.push(Term::Return(v(res)));
    let call = let_(
        &mut fb,
        res,
        Value::Call {
            callee: v(k),
            args: vec![],
        },
        ret,
    );
    let mut body = let_(
        &mut fb,
        k,
        Value::Closure {
            function: FunId(1),
            free: xs.iter().map(|&x| v(x)).collect(),
        },
        call,
    );
    for (i, &x) in xs.iter().enumerate().rev() {
        body = let_(&mut fb, x, Value::Operand(c(i as u32)), body);
    }
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);

    let mut fb = FB::default();
    let free: Vec<Var> = (0..N).map(|_| fb.var()).collect();
    let terms: Vec<Var> = (0..N).map(|_| fb.var()).collect();
    let sums: Vec<Var> = (0..N).map(|_| fb.var()).collect();
    // sums[0] = terms[0]; sums[i] = sums[i-1] + terms[i].
    let mut body = fb.push(Term::Return(v(sums[N as usize - 1])));
    for i in (1..N as usize).rev() {
        body = let_(
            &mut fb,
            sums[i],
            Value::Primitive {
                op: Primitive::Add,
                args: vec![v(sums[i - 1]), v(terms[i])],
            },
            body,
        );
    }
    body = let_(&mut fb, sums[0], Value::Operand(v(terms[0])), body);
    for i in (0..N as usize).rev() {
        body = let_(
            &mut fb,
            terms[i],
            Value::Primitive {
                op: Primitive::Mul,
                args: vec![c(i as u32), v(free[i])],
            },
            body,
        );
    }
    let f1 = fb.finish("wide", Arity::exactly(0), vec![], free.clone(), body);

    let constants = (1..=N as i64).map(Constant::Integer).collect();
    let m = module(constants, vec![], vec![f0, f1]);
    let out = convert_checked(&m).unwrap();
    let expected: i64 = (1..=N as i64).map(|n| n * n).sum();
    assert_eq!(expected, 5_625_216);
    for (strategy, r) in run_all(&out) {
        assert_eq!(r.unwrap(), expected.to_string(), "{strategy}");
    }
}

/// 0.0 and -0.0 are different constants. Guards against the module
/// builder's pool merging them, which would turn -0.0 into 0.0.
#[test]
fn negative_zero_survives() {
    let mut fb = FB::default();
    let a = fb.var();
    let b = fb.var();
    let r = fb.var();
    let ret = fb.push(Term::Return(v(r)));
    let t = let_(
        &mut fb,
        r,
        Value::Primitive {
            op: Primitive::Cons,
            args: vec![v(a), v(b)],
        },
        ret,
    );
    let t = let_(&mut fb, b, Value::Operand(c(1)), t);
    let body = let_(&mut fb, a, Value::Operand(c(0)), t);
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
    let m = module(
        vec![Constant::Flonum(0.0), Constant::Flonum(-0.0)],
        vec![],
        vec![f0],
    );
    let out = convert_checked(&m).unwrap();
    let results = run_all(&out);
    eprintln!("negative_zero_survives: {results:?}");
    for (_, r) in results {
        assert_eq!(r.unwrap(), "(0.0 . -0.0)");
    }
}

/// A module returning constant 0, with `constants`.
fn constants_module(constants: Vec<Constant>) -> Module {
    let mut fb = FB::default();
    let body = fb.push(Term::Return(c(0)));
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
    module(constants, vec![], vec![f0])
}

/// 65,536 constants leave no room for the unspecified constant the
/// converter adds, so the validator refuses them: an error, not a panic,
/// and no quadratic search to find it.
#[test]
fn full_constant_pool() {
    let m = constants_module((0..65_536).map(Constant::Integer).collect());
    let t = Instant::now();
    let r = convert_checked(&m);
    let e = r.expect_err("there is no room for the unspecified value");
    assert!(e.starts_with("IR invalid"), "{e}");
    assert!(t.elapsed().as_secs() < 5, "took {:?}", t.elapsed());

    // One fewer fits.
    let m = constants_module((0..65_535).map(Constant::Integer).collect());
    convert_checked(&m).unwrap();
}

/// The time the converter's dedup takes grows linearly with the pool.
#[test]
fn constant_pool_dedup_time() {
    let mut times = Vec::new();
    for n in [16_000u32, 64_000] {
        let constants = (0..n)
            .map(|i| Constant::String(format!("s{i}").into()))
            .collect();
        let m = constants_module(constants);
        let t = Instant::now();
        convert_checked(&m).unwrap();
        times.push(t.elapsed());
    }
    assert!(times[1].as_secs() < 5, "{times:?}");
}

/// A quoted list a million long: the IR's constants drop iteratively, the
/// run format's do not.
#[test]
fn deep_constant_list() {
    let mut list = Constant::EmptyList;
    for i in 0..1_000_000 {
        list = Constant::Pair(Box::new(Constant::Integer(i)), Box::new(list));
    }
    let mut fb = FB::default();
    let body = fb.push(Term::Return(c(0)));
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
    let m = module(vec![list], vec![], vec![f0]);
    ir::validate(&m).unwrap();
    eprintln!("validated; converting");
    let out = vali_bytecode::convert(&m).unwrap();
    eprintln!("converted; dropping");
    drop(out);
    eprintln!("dropped");
}

/// Two equal quoted lists: the builder's dedup compares them recursively.
#[test]
fn deep_constant_list_dedup() {
    let make = || {
        let mut list = Constant::EmptyList;
        for i in 0..300_000 {
            list = Constant::Pair(Box::new(Constant::Integer(i)), Box::new(list));
        }
        list
    };
    let mut fb = FB::default();
    let body = fb.push(Term::Return(c(0)));
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
    let m = module(vec![make(), make()], vec![], vec![f0]);
    eprintln!("built; converting");
    let out = vali_bytecode::convert(&m).unwrap();
    std::mem::forget(out);
    eprintln!("converted");
}

/// A call with 300 arguments, and a tail call with 300.
#[test]
fn very_wide_calls() {
    for tail in [false, true] {
        let mut fb = FB::default();
        let f = fb.var();
        let r = fb.var();
        let args = vec![c(0); 300];
        let t = if tail {
            fb.push(Term::TailCall {
                callee: v(f),
                args,
                position: None,
            })
        } else {
            let ret = fb.push(Term::Return(v(r)));
            let_(&mut fb, r, Value::Call { callee: v(f), args }, ret)
        };
        let body = let_(&mut fb, f, Value::Global(GlobalId(0)), t);
        let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
        let m = module(
            vec![Constant::Integer(1)],
            vec![Global {
                name: "list".into(),
                link: GlobalLink::Primitive,
            }],
            vec![f0],
        );
        let r = convert_checked(&m);
        eprintln!(
            "call with 300 args (tail {tail}): {:?}",
            r.as_ref().map(|_| "ok").map_err(|e| e.clone())
        );
        assert!(!matches!(&r, Err(e) if e.contains("PANICKED") || e.contains("INVALID")));
    }
}

/// Calls with tens of thousands of arguments, and as many variables live
/// at once, convert in time that grows with the size, not its square.
#[test]
fn enormous_calls_convert_in_linear_time() {
    for (n, vars, tail) in [
        (60_000usize, 0usize, false),
        (60_000, 0, true),
        (10_000, 10_000, false),
        (10_000, 10_000, true),
        (30_000, 30_000, false),
        (30_000, 30_000, true),
        (60_000, 60_000, false),
    ] {
        let mut fb = FB::default();
        let f = fb.var();
        let r = if tail { f } else { fb.var() };
        let xs: Vec<Var> = (0..vars).map(|_| fb.var()).collect();
        // The variables, last first, then constants.
        let args: Vec<Operand> = xs
            .iter()
            .rev()
            .map(|&x| v(x))
            .chain(std::iter::repeat_n(c(0), n))
            .collect();
        let mut t = if tail {
            fb.push(Term::TailCall {
                callee: v(f),
                args,
                position: None,
            })
        } else {
            let ret = fb.push(Term::Return(v(r)));
            let_(&mut fb, r, Value::Call { callee: v(f), args }, ret)
        };
        t = let_(&mut fb, f, Value::Global(GlobalId(0)), t);
        for &x in xs.iter().rev() {
            t = let_(&mut fb, x, Value::Operand(c(0)), t);
        }
        let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], t);
        let m = module(
            vec![Constant::Integer(1)],
            vec![Global {
                name: "list".into(),
                link: GlobalLink::Primitive,
            }],
            vec![f0],
        );
        let time = Instant::now();
        let r = convert_checked(&m);
        eprintln!(
            "{n} constants + {vars} variables as arguments (tail {tail}): {:?} in {:?}",
            r.as_ref().map(|_| "ok").map_err(|e| e.clone()),
            time.elapsed()
        );
        assert!(!matches!(&r, Err(e) if e.contains("PANICKED") || e.contains("INVALID")));
        assert!(time.elapsed().as_secs() < 10, "took {:?}", time.elapsed());
    }
}

/// Modules with very many functions, globals, sources, exports, inlined
/// copies, join-point parameters and free variables convert in time that
/// grows with their size.
#[test]
fn very_many_of_everything_converts_in_linear_time() {
    let time = Instant::now();
    let n = 60_000usize;
    // Many functions, each closed over by the body, with a position in its
    // own source and many globals and exports.
    let mut functions = Vec::new();
    let mut fb = FB::default();
    let mut body = fb.push(Term::Return(c(0)));
    for i in 0..n {
        let x = fb.var();
        body = fb.push(Term::Let {
            var: x,
            value: Value::Closure {
                function: FunId(i as u32 + 1),
                free: vec![],
            },
            body,
            position: Some(ir::Position {
                source: i as u16,
                line: 1,
                column: 1,
                inlined: 0,
            }),
        });
        let x = fb.var();
        body = let_(&mut fb, x, Value::SetGlobal(GlobalId(i as u32), c(0)), body);
    }
    functions.push(fb.finish("body", Arity::exactly(0), vec![], vec![], body));
    for i in 0..n {
        let mut fb = FB::default();
        let ret = fb.push(Term::Return(c(0)));
        functions.push(fb.finish(&format!("f{i}"), Arity::exactly(0), vec![], vec![], ret));
    }
    let mut m = module(vec![Constant::Integer(1)], vec![], functions);
    m.globals = (0..n)
        .map(|i| Global {
            name: format!("g{i}").into(),
            link: GlobalLink::Defined,
        })
        .collect();
    m.exports = (0..n)
        .map(|i| (format!("g{i}").into(), GlobalId(i as u32)))
        .collect();
    m.sources = (0..n).map(|i| format!("s{i}").into()).collect();
    convert_checked(&m).unwrap();
    eprintln!(
        "{n} functions, globals, exports, sources: {:?}",
        time.elapsed()
    );

    // A join point with many parameters, jumped to with as many arguments;
    // a closure with many free variables.
    let time = Instant::now();
    let mut fb = FB::default();
    let params: Vec<Var> = (0..n).map(|_| fb.var()).collect();
    let j = fb.join();
    let handler = fb.push(Term::Return(v(params[0])));
    let jump = fb.push(Term::Jump {
        join: j,
        args: vec![c(0); n],
    });
    let body = fb.push(Term::Join {
        join: j,
        params,
        recursive: false,
        handler,
        body: jump,
    });
    let g = fb.finish("g", Arity::exactly(0), vec![], vec![], body);
    let mut fb = FB::default();
    let free: Vec<Var> = (0..n).map(|_| fb.var()).collect();
    let ret = fb.push(Term::Return(v(free[n - 1])));
    let h = fb.finish("h", Arity::exactly(0), vec![], free, ret);
    let mut fb = FB::default();
    let x = fb.var();
    let ret = fb.push(Term::Return(v(x)));
    let body = let_(
        &mut fb,
        x,
        Value::Closure {
            function: FunId(2),
            free: vec![c(0); n],
        },
        ret,
    );
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
    let m = module(vec![Constant::Integer(1)], vec![], vec![f0, g, h]);
    let r = convert_checked(&m);
    eprintln!(
        "{n} join parameters, {n} free: {:?} in {:?}",
        r.as_ref().map(|_| "ok").map_err(|e| e.clone()),
        time.elapsed()
    );
    assert!(!matches!(&r, Err(e) if e.contains("PANICKED") || e.contains("INVALID")));
    assert!(time.elapsed().as_secs() < 10, "took {:?}", time.elapsed());
}

/// Slot allocation takes the lowest free slot without scanning the busy
/// ones: 60,000 long-lived variables and 250,000 short-lived ones took
/// 13 s that way, and a join point with 60,000 parameters over a second.
#[test]
fn slot_allocation_is_not_a_scan_of_the_slots() {
    let time = Instant::now();
    let (long, short) = (60_000usize, 250_000usize);
    let mut fb = FB::default();
    let xs: Vec<Var> = (0..long).map(|_| fb.var()).collect();
    let ys: Vec<Var> = (0..short).map(|_| fb.var()).collect();
    // Each long-lived variable is read at the end, so all are live at once.
    let last = *xs.last().unwrap();
    let mut t = fb.push(Term::Return(v(xs[0])));
    for &x in xs.iter().skip(1) {
        let r = fb.var();
        t = let_(
            &mut fb,
            r,
            Value::Primitive {
                op: Primitive::Eq,
                args: vec![v(x), v(last)],
            },
            t,
        );
    }
    for &y in &ys {
        t = let_(&mut fb, y, Value::Operand(c(0)), t);
    }
    for &x in xs.iter().rev() {
        t = let_(&mut fb, x, Value::Operand(c(0)), t);
    }
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], t);
    let m = module(vec![Constant::Integer(1)], vec![], vec![f0]);
    let r = convert_checked(&m);
    eprintln!(
        "{long} long-lived, {short} short-lived: {:?} in {:?}",
        r.as_ref().map(|_| "ok").map_err(|e| e.clone()),
        time.elapsed()
    );
    assert!(!matches!(&r, Err(e) if e.contains("PANICKED") || e.contains("INVALID")));
    assert!(time.elapsed().as_secs() < 10, "took {:?}", time.elapsed());

    // A join point with 60,000 parameters, never jumped to.
    let time = Instant::now();
    let mut fb = FB::default();
    let params: Vec<Var> = (0..60_000).map(|_| fb.var()).collect();
    let j = fb.join();
    let handler = fb.push(Term::Return(v(params[0])));
    let body = fb.push(Term::Return(c(0)));
    let top = fb.push(Term::Join {
        join: j,
        params,
        recursive: false,
        handler,
        body,
    });
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], top);
    let m = module(vec![Constant::Integer(1)], vec![], vec![f0]);
    let r = convert_checked(&m);
    assert!(!matches!(&r, Err(e) if e.contains("PANICKED") || e.contains("INVALID")));
    assert!(time.elapsed().as_secs() < 10, "took {:?}", time.elapsed());
}

/// The record primitives that `define-record-type` expands into, which a
/// library's artifact may link and so call directly with anything: each
/// raises an error for an argument of the wrong type or range, under every
/// strategy, and reads none of them unchecked.
#[test]
fn record_primitives_reject_bad_arguments() {
    const NAMES: [&str; 7] = [
        "%make-record-type",
        "%record-constructor-indices",
        "%make-record",
        "%record-ref",
        "%record-set!",
        "%record-field-index",
        "%record?",
    ];
    let prim = |name: &str| NAMES.iter().position(|n| *n == name).unwrap() as u32;
    let sym = |s: &str| Constant::Symbol(s.into());
    let list = |items: Vec<Constant>| {
        items
            .into_iter()
            .rev()
            .fold(Constant::EmptyList, |rest, c| {
                Constant::Pair(Box::new(c), Box::new(rest))
            })
    };
    let ints = |ns: &[i64]| Constant::Vector(ns.iter().map(|&n| Constant::Integer(n)).collect());
    let constants = vec![
        sym("point"),                                           // 0
        list(vec![sym("x"), sym("y")]),                         // 1
        Constant::Integer(5),                                   // 2
        Constant::Integer(0),                                   // 3
        Constant::Integer(2),                                   // 4
        Constant::Integer(-1),                                  // 5
        sym("x"),                                               // 6
        sym("z"),                                               // 7
        Constant::Flonum(1.5),                                  // 8
        Constant::Boolean(false),                               // 9
        ints(&[7, 9]),                                          // 10
        ints(&[-1, 0]),                                         // 11
        Constant::Vector(vec![sym("x"), Constant::Integer(0)]), // 12
        ints(&[0]),                                             // 13
        Constant::Integer(1),                                   // 14
        list(vec![sym("z")]),                                   // 15
    ];
    let (point, fields, five, zero, two, neg, x, z, flo, no, bad, negs, syms, short, one, zs) = (
        c(0),
        c(1),
        c(2),
        c(3),
        c(4),
        c(5),
        c(6),
        c(7),
        c(8),
        c(9),
        c(10),
        c(11),
        c(12),
        c(13),
        c(14),
        c(15),
    );
    // A type, another, the constructor indices, and a record of the first.
    type Case = Box<dyn Fn(Operand, Operand, Operand, Operand) -> (&'static str, Vec<Operand>)>;
    let cases: Vec<(&str, Case)> = vec![
        (
            "ref of a non-type",
            Box::new(move |_, _, _, _| ("%record-ref", vec![five, five, zero])),
        ),
        (
            "ref with a non-type",
            Box::new(move |_, _, _, p| ("%record-ref", vec![p, five, zero])),
        ),
        (
            "ref with a symbol",
            Box::new(move |_, _, _, p| ("%record-ref", vec![p, point, zero])),
        ),
        (
            "set with a non-type",
            Box::new(move |_, _, _, p| ("%record-set!", vec![p, five, zero, one])),
        ),
        (
            "set of a non-record",
            Box::new(move |_, _, _, _| ("%record-set!", vec![five, five, zero, one])),
        ),
        (
            "field index of a non-type",
            Box::new(move |_, _, _, _| ("%record-field-index", vec![five, x])),
        ),
        (
            "indices of a non-type",
            Box::new(move |_, _, _, _| ("%record-constructor-indices", vec![five, fields])),
        ),
        (
            "make with a non-type",
            Box::new(move |_, _, i, _| ("%make-record", vec![five, i, one, two])),
        ),
        (
            "make a type of a non-symbol",
            Box::new(move |_, _, _, _| ("%make-record-type", vec![five, fields])),
        ),
        (
            "ref of a non-record",
            Box::new(move |r, _, _, _| ("%record-ref", vec![five, r, zero])),
        ),
        (
            "ref with the wrong type",
            Box::new(move |_, o, _, p| ("%record-ref", vec![p, o, zero])),
        ),
        (
            "set of a non-record",
            Box::new(move |r, _, _, _| ("%record-set!", vec![five, r, zero, one])),
        ),
        (
            "set with the wrong type",
            Box::new(move |_, o, _, p| ("%record-set!", vec![p, o, zero, one])),
        ),
        (
            "ref past the fields",
            Box::new(move |r, _, _, p| ("%record-ref", vec![p, r, two])),
        ),
        (
            "ref at a negative index",
            Box::new(move |r, _, _, p| ("%record-ref", vec![p, r, neg])),
        ),
        (
            "ref at a symbol",
            Box::new(move |r, _, _, p| ("%record-ref", vec![p, r, x])),
        ),
        (
            "ref at a flonum",
            Box::new(move |r, _, _, p| ("%record-ref", vec![p, r, flo])),
        ),
        (
            "set past the fields",
            Box::new(move |r, _, _, p| ("%record-set!", vec![p, r, two, one])),
        ),
        (
            "set at false",
            Box::new(move |r, _, _, p| ("%record-set!", vec![p, r, no, one])),
        ),
        (
            "a field that is not there",
            Box::new(move |r, _, _, _| ("%record-field-index", vec![r, z])),
        ),
        (
            "indices of a missing field",
            Box::new(move |r, _, _, _| ("%record-constructor-indices", vec![r, zs])),
        ),
        (
            "make with indices that are not a vector",
            Box::new(move |r, _, _, _| ("%make-record", vec![r, five, one, two])),
        ),
        (
            "make with indices out of range",
            Box::new(move |r, _, _, _| ("%make-record", vec![r, bad, one, two])),
        ),
        (
            "make with negative indices",
            Box::new(move |r, _, _, _| ("%make-record", vec![r, negs, one, two])),
        ),
        (
            "make with symbol indices",
            Box::new(move |r, _, _, _| ("%make-record", vec![r, syms, one, two])),
        ),
        (
            "make with too few indices",
            Box::new(move |r, _, _, _| ("%make-record", vec![r, short, one, two])),
        ),
    ];
    let build = |name: &str, bad_call: Option<&Case>| {
        let mut fb = FB::default();
        let (rtd, other, idx, p, result) = (fb.var(), fb.var(), fb.var(), fb.var(), fb.var());
        let (v_rtd, v_other, v_idx, v_p) = (v(rtd), v(other), v(idx), v(p));
        let (last_name, last_args) = match bad_call {
            Some(case) => case(v_rtd, v_other, v_idx, v_p),
            None => ("%record-ref", vec![v_p, v_rtd, zero]),
        };
        let mut steps: Vec<(Var, &str, Vec<Operand>)> = vec![
            (rtd, "%make-record-type", vec![point, fields]),
            (other, "%make-record-type", vec![point, fields]),
            (idx, "%record-constructor-indices", vec![v_rtd, fields]),
            (p, "%make-record", vec![v_rtd, v_idx, one, two]),
        ];
        steps.push((result, last_name, last_args));
        let mut t = fb.push(Term::Return(v(result)));
        for (var, prim_name, args) in steps.into_iter().rev() {
            let callee = fb.var();
            let call = let_(
                &mut fb,
                var,
                Value::Call {
                    callee: v(callee),
                    args,
                },
                t,
            );
            t = let_(
                &mut fb,
                callee,
                Value::Global(GlobalId(prim(prim_name))),
                call,
            );
        }
        let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], t);
        let globals = NAMES
            .iter()
            .map(|n| Global {
                name: (*n).into(),
                link: GlobalLink::Primitive,
            })
            .collect();
        let m = module(constants.clone(), globals, vec![f0]);
        (name.to_owned(), convert_checked(&m).unwrap())
    };
    // Called properly, the primitives work.
    let (name, ok) = build("control", None);
    for (strategy, r) in run_all(&ok) {
        assert_eq!(r.as_deref(), Ok("1"), "{name} under {strategy}");
    }
    for (name, case) in &cases {
        let (name, bytecode) = build(name, Some(case));
        for (strategy, r) in run_all(&bytecode) {
            let e = r.expect_err(&format!("{name} under {strategy} should raise"));
            assert!(e.starts_with("error:"), "{name} under {strategy}: {e}");
        }
    }
}

/// The widest calls that fit: every argument a distinct live variable.
#[test]
fn widest_calls_that_fit() {
    for tail in [false, true] {
        for n in [240usize, 248, 250, 251, 252, 253, 254, 255] {
            let mut fb = FB::default();
            let f = fb.var();
            let r = fb.var();
            let xs: Vec<Var> = (0..n).map(|_| fb.var()).collect();
            let args: Vec<Operand> = xs.iter().map(|&x| v(x)).collect();
            let mut t = if tail {
                fb.push(Term::TailCall {
                    callee: v(f),
                    args,
                    position: None,
                })
            } else {
                let ret = fb.push(Term::Return(v(r)));
                let_(&mut fb, r, Value::Call { callee: v(f), args }, ret)
            };
            for (i, &x) in xs.iter().enumerate().rev() {
                t = let_(
                    &mut fb,
                    x,
                    Value::Primitive {
                        op: Primitive::Add,
                        args: vec![c(0), c(i as u32 % 2)],
                    },
                    t,
                );
            }
            let body = let_(&mut fb, f, Value::Global(GlobalId(0)), t);
            let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
            let m = module(
                vec![Constant::Integer(1000), Constant::Integer(2000)],
                vec![Global {
                    name: "+".into(),
                    link: GlobalLink::Primitive,
                }],
                vec![f0],
            );
            let r = convert_checked(&m);
            let expected = (0..n)
                .map(|i| 1000 + 1000 + if i % 2 == 1 { 1000 } else { 0 })
                .sum::<usize>()
                - (0..n).filter(|i| i % 2 == 1).count() * 1000;
            let _ = expected;
            match r {
                Ok(bc) => {
                    let results = run_all(&bc);
                    let first = results[0].1.clone();
                    eprintln!("tail {tail} n {n}: converted, ran {first:?}");
                    for (s, x) in &results {
                        assert_eq!(x, &first, "{s}");
                    }
                }
                Err(e) => {
                    eprintln!("tail {tail} n {n}: {e}");
                    assert!(!e.contains("PANICKED") && !e.contains("INVALID"), "{e}");
                }
            }
        }
    }
}

/// 300 values live at once.
#[test]
fn too_many_live_values() {
    let mut fb = FB::default();
    let xs: Vec<Var> = (0..300).map(|_| fb.var()).collect();
    let acc: Vec<Var> = (0..300).map(|_| fb.var()).collect();
    let mut t = fb.push(Term::Return(v(acc[299])));
    for i in (1..300).rev() {
        t = let_(
            &mut fb,
            acc[i],
            Value::Primitive {
                op: Primitive::Add,
                args: vec![v(acc[i - 1]), v(xs[i])],
            },
            t,
        );
    }
    t = let_(&mut fb, acc[0], Value::Operand(v(xs[0])), t);
    for (i, &x) in xs.iter().enumerate().rev() {
        t = let_(
            &mut fb,
            x,
            Value::Primitive {
                op: Primitive::Add,
                args: vec![c(0), c(i as u32 % 3)],
            },
            t,
        );
    }
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], t);
    let m = module(
        vec![
            Constant::Integer(1000),
            Constant::Integer(2),
            Constant::Integer(3),
        ],
        vec![],
        vec![f0],
    );
    let r = convert_checked(&m);
    eprintln!("300 live: {:?}", r.as_ref().map(|_| "ok"));
    assert!(!matches!(&r, Err(e) if e.contains("PANICKED") || e.contains("INVALID")));
}

/// A function with 300 parameters (200 required, 100 optional).
#[test]
fn too_many_parameters() {
    let mut fb = FB::default();
    let k = fb.var();
    let ret = fb.push(Term::Return(v(k)));
    let body = let_(
        &mut fb,
        k,
        Value::Closure {
            function: FunId(1),
            free: vec![],
        },
        ret,
    );
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
    let mut fb = FB::default();
    let ps: Vec<Var> = (0..300).map(|_| fb.var()).collect();
    let body = fb.push(Term::Return(v(ps[299])));
    let f1 = fb.finish(
        "many",
        Arity {
            required: 200,
            optional: 100,
            rest: false,
        },
        ps,
        vec![],
        body,
    );
    let m = module(vec![], vec![], vec![f0, f1]);
    let r = convert_checked(&m);
    eprintln!("300 params: {:?}", r.as_ref().map(|_| "ok"));
    assert!(!matches!(&r, Err(e) if e.contains("PANICKED") || e.contains("INVALID")));
}

/// `set-free` with index 300: the IR's docs say conversion checks the index
/// at run time; the converter rejects the module instead.
#[test]
fn set_free_index_out_of_u8() {
    let mut fb = FB::default();
    let k = fb.var();
    let u = fb.var();
    let ret = fb.push(Term::Return(c(0)));
    // Never runs: guarded by #f.
    let set = let_(
        &mut fb,
        u,
        Value::SetFree {
            closure: v(k),
            index: 300,
            value: c(0),
        },
        ret,
    );
    let skip = fb.push(Term::Return(c(0)));
    let iff = fb.push(Term::If {
        test: c(1),
        then: set,
        otherwise: skip,
    });
    let body = let_(
        &mut fb,
        k,
        Value::Closure {
            function: FunId(0),
            free: vec![],
        },
        iff,
    );
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
    let m = module(
        vec![Constant::Integer(7), Constant::Boolean(false)],
        vec![],
        vec![f0],
    );
    let r = convert_checked(&m);
    eprintln!("set-free 300: {:?}", r.as_ref().map(|_| "ok"));
}

/// Non-canonical exact ratios with bignum parts pass the IR validator.
#[test]
fn bignum_ratio_not_in_lowest_terms() {
    let big = |limbs: Vec<u64>| Constant::Bignum {
        negative: false,
        limbs,
    };
    for (name, k) in [
        (
            "2^65/2",
            Constant::Ratio(Box::new(big(vec![0, 2])), Box::new(Constant::Integer(2))),
        ),
        (
            "0/2^64",
            Constant::Ratio(Box::new(Constant::Integer(0)), Box::new(big(vec![0, 1]))),
        ),
        (
            "2^64/2^65",
            Constant::Ratio(Box::new(big(vec![0, 1])), Box::new(big(vec![0, 2]))),
        ),
    ] {
        let mut fb = FB::default();
        let body = fb.push(Term::Return(c(0)));
        let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
        let m = module(vec![k], vec![], vec![f0]);
        let r = convert_checked(&m).map(|bc| run_all(&bc));
        eprintln!("{name}: {r:?}");
    }
}

/// Two distinct IR globals with the same name and link: the module
/// builder merges them into one cell.
#[test]
fn duplicate_defined_globals_alias() {
    let mut fb = FB::default();
    let (a, b, r) = (fb.var(), fb.var(), fb.var());
    let ret = fb.push(Term::Return(v(r)));
    let t = let_(&mut fb, r, Value::Global(GlobalId(0)), ret);
    let t = let_(&mut fb, b, Value::SetGlobal(GlobalId(1), c(1)), t);
    let body = let_(&mut fb, a, Value::SetGlobal(GlobalId(0), c(0)), t);
    let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
    let m = module(
        vec![Constant::Integer(1), Constant::Integer(2)],
        vec![
            Global {
                name: "x".into(),
                link: GlobalLink::Defined,
            },
            Global {
                name: "x".into(),
                link: GlobalLink::Defined,
            },
        ],
        vec![f0],
    );
    let e = ir::validate(&m).unwrap_err();
    assert!(e.message.contains("twice"), "{e}");
    // The same name with another link is another variable.
    let mut m = m;
    m.globals[1].link = GlobalLink::Repl;
    ir::validate(&m).unwrap();
}

/// Nesting 1,000,000 deep: lets, ifs, joins.
#[test]
fn deep_nesting() {
    // Deep enough to overflow a test thread's stack under any recursive
    // implementation; the full million only where the build is fast.
    let n = if cfg!(debug_assertions) {
        50_000
    } else {
        1_000_000
    };
    // Lets: v_i = v_{i-1} + 1.
    {
        let mut fb = FB::default();
        let xs: Vec<Var> = (0..n).map(|_| fb.var()).collect();
        let mut t = fb.push(Term::Return(v(xs[n - 1])));
        for i in (1..n).rev() {
            t = let_(
                &mut fb,
                xs[i],
                Value::Primitive {
                    op: Primitive::Add,
                    args: vec![v(xs[i - 1]), c(0)],
                },
                t,
            );
        }
        t = let_(&mut fb, xs[0], Value::Operand(c(0)), t);
        let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], t);
        let m = module(vec![Constant::Integer(1)], vec![], vec![f0]);
        let time = Instant::now();
        let r = convert_checked(&m).map(|bc| run_with::<PreciseHeadered>(&bc));
        eprintln!("{n} lets: {r:?} in {:?}", time.elapsed());
    }
    // Ifs nested in `then`, each testing a fresh comparison.
    {
        let mut fb = FB::default();
        let x = fb.var();
        let mut t = fb.push(Term::Return(v(x)));
        for _ in 0..n {
            let b = fb.var();
            let other = fb.push(Term::Return(c(0)));
            let iff = fb.push(Term::If {
                test: v(b),
                then: t,
                otherwise: other,
            });
            t = let_(
                &mut fb,
                b,
                Value::Primitive {
                    op: Primitive::Lt,
                    args: vec![c(0), v(x)],
                },
                iff,
            );
        }
        t = let_(&mut fb, x, Value::Operand(c(1)), t);
        let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], t);
        let m = module(
            vec![Constant::Integer(1), Constant::Integer(5)],
            vec![],
            vec![f0],
        );
        let time = Instant::now();
        let r = convert_checked(&m).map(|bc| run_with::<PreciseHeadered>(&bc));
        eprintln!("{n} ifs: {r:?} in {:?}", time.elapsed());
    }
    // Joins nested in bodies: join j_i(p) = jump j_{i-1}(p) ... j_0(p) = return p.
    {
        let n = if cfg!(debug_assertions) {
            10_000
        } else {
            200_000
        };
        let mut fb = FB::default();
        let js: Vec<JoinId> = (0..n).map(|_| fb.join()).collect();
        let mut handlers = Vec::new();
        let mut params = Vec::new();
        for i in 0..n {
            let p = fb.var();
            params.push(p);
            let h = if i == 0 {
                fb.push(Term::Return(v(p)))
            } else {
                fb.push(Term::Jump {
                    join: js[i - 1],
                    args: vec![v(p)],
                })
            };
            handlers.push(h);
        }
        let mut t = fb.push(Term::Jump {
            join: js[n - 1],
            args: vec![c(0)],
        });
        for i in (0..n).rev() {
            t = fb.push(Term::Join {
                join: js[i],
                params: vec![params[i]],
                recursive: false,
                handler: handlers[i],
                body: t,
            });
        }
        // Wrap so the outermost is j_0.
        let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], t);
        let m = module(vec![Constant::Integer(42)], vec![], vec![f0]);
        let time = Instant::now();
        let r = convert_checked(&m).map(|bc| run_with::<PreciseHeadered>(&bc));
        eprintln!("{n} joins: {r:?} in {:?}", time.elapsed());
    }
}

/// Many recursive joins and as many uses of an outer variable: the time
/// grows with their product if each use is checked against each loop
/// (80,000 took 2.8 s in a release build).
#[test]
fn quadratic_loop_regions() {
    for n in [10_000usize, 200_000] {
        let mut fb = FB::default();
        // rec join j_i() = return x  in  rec join j_{i+1} ... in  return x
        let x = fb.var();
        let mut t = fb.push(Term::Return(v(x)));
        for _ in 0..n {
            let j = fb.join();
            let h = fb.push(Term::Return(v(x)));
            t = fb.push(Term::Join {
                join: j,
                params: vec![],
                recursive: true,
                handler: h,
                body: t,
            });
        }
        t = let_(&mut fb, x, Value::Operand(c(0)), t);
        let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], t);
        let m = module(vec![Constant::Integer(1)], vec![], vec![f0]);
        let time = Instant::now();
        let r = convert_checked(&m);
        eprintln!(
            "{n} loops + {n} uses: {:?} in {:?}",
            r.as_ref().map(|_| "ok"),
            time.elapsed()
        );
        r.unwrap();
        assert!(time.elapsed().as_secs() < 10, "took {:?}", time.elapsed());
    }
}

/// Many fused `if`s in a function with many free variables: the time
/// grows with their product if each `if` scans the free list (80,000 took
/// 2.6 s in a release build, and the function was then rejected).
#[test]
fn quadratic_free_scan() {
    for (n, ifs) in [
        (10_000usize, 10_000usize),
        (60_000, 300_000),
        (80_000, 10_000),
    ] {
        let mut fb = FB::default();
        let free: Vec<Var> = (0..n).map(|_| fb.var()).collect();
        let mut t = fb.push(Term::Return(c(0)));
        for _ in 0..ifs {
            let b = fb.var();
            let other = fb.push(Term::Return(c(0)));
            let iff = fb.push(Term::If {
                test: v(b),
                then: t,
                otherwise: other,
            });
            t = let_(
                &mut fb,
                b,
                Value::Primitive {
                    op: Primitive::Eq,
                    args: vec![c(0), c(0)],
                },
                iff,
            );
        }
        let f1 = fb.finish("f", Arity::exactly(0), vec![], free, t);
        let mut fb = FB::default();
        let body = fb.push(Term::Return(c(0)));
        let f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
        let m = module(vec![Constant::Integer(1)], vec![], vec![f0, f1]);
        let time = Instant::now();
        let r = convert_checked(&m);
        eprintln!(
            "{n} fused ifs + {n} free: {:?} in {:?}",
            r.as_ref().map(|_| "ok").map_err(|e| e.clone()),
            time.elapsed()
        );
        assert!(time.elapsed().as_secs() < 10, "took {:?}", time.elapsed());
        if n > 65_535 {
            assert!(r.is_err(), "more free variables than a closure holds");
        }
    }
}

/// A function that declares 2^32 - 1 variables or join points but has one
/// term: the validator must reject it before allocating for the count, and
/// so must the converter, which validates first.
#[test]
fn huge_declared_counts() {
    for which in ["vars", "joins"] {
        let mut fb = FB::default();
        let body = fb.push(Term::Return(c(0)));
        let mut f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
        if which == "vars" {
            f0.vars = u32::MAX;
        } else {
            f0.joins = u32::MAX;
        }
        let m = module(vec![Constant::Integer(1)], vec![], vec![f0]);
        let time = Instant::now();
        let e = ir::validate(&m).unwrap_err();
        assert!(e.message.contains("declares"), "{e}");
        vali_bytecode::convert(&m).unwrap_err();
        assert!(time.elapsed().as_secs() < 2, "took {:?}", time.elapsed());
    }
}

/// A count too large for the bindings a function has is an error even when
/// small: it is exactly the bindings' number or fewer.
#[test]
fn declared_counts_must_match_bindings() {
    let mut fb = FB::default();
    let x = fb.var();
    let ret = fb.push(Term::Return(v(x)));
    let body = let_(&mut fb, x, Value::Operand(c(0)), ret);
    let mut f0 = fb.finish("body", Arity::exactly(0), vec![], vec![], body);
    let m = module(vec![Constant::Integer(1)], vec![], vec![f0.clone()]);
    ir::validate(&m).unwrap();
    f0.vars = 2;
    let m = module(vec![Constant::Integer(1)], vec![], vec![f0.clone()]);
    assert!(ir::validate(&m).is_err());
    f0.vars = 1;
    f0.joins = 1;
    let m = module(vec![Constant::Integer(1)], vec![], vec![f0]);
    assert!(ir::validate(&m).is_err());
}

// ---------------------------------------------------------------------------
// A reference interpreter for the IR.

#[derive(Clone)]
enum V {
    Int(i64),
    Bool(bool),
    Nil,
    Unspec,
    Str(Rc<str>),
    Pair(Rc<RefCell<(V, V)>>),
    Ipair(Rc<(V, V)>),
    Vector(Rc<RefCell<Vec<V>>>),
    Box(Rc<RefCell<V>>),
    Clo(Rc<Clo>),
    Prim,
}

struct Clo {
    fun: u32,
    free: RefCell<Vec<V>>,
}

#[derive(Debug)]
enum Stop {
    Overflow,
    Timeout,
    Error(String),
}

fn eq(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Int(x), V::Int(y)) => x == y,
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Nil, V::Nil) | (V::Unspec, V::Unspec) | (V::Prim, V::Prim) => true,
        (V::Str(x), V::Str(y)) => Rc::ptr_eq(x, y),
        (V::Pair(x), V::Pair(y)) => Rc::ptr_eq(x, y),
        (V::Ipair(x), V::Ipair(y)) => Rc::ptr_eq(x, y),
        (V::Vector(x), V::Vector(y)) => Rc::ptr_eq(x, y),
        (V::Box(x), V::Box(y)) => Rc::ptr_eq(x, y),
        (V::Clo(x), V::Clo(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

fn write(v: &V, out: &mut String) {
    match v {
        V::Int(n) => out.push_str(&n.to_string()),
        V::Bool(true) => out.push_str("#t"),
        V::Bool(false) => out.push_str("#f"),
        V::Nil => out.push_str("()"),
        V::Unspec => out.push_str("#<unspecified>"),
        V::Str(s) => {
            out.push('"');
            out.push_str(s);
            out.push('"');
        }
        V::Pair(p) => {
            out.push('(');
            let mut cur = p.clone();
            loop {
                let (car, cdr) = cur.borrow().clone();
                write(&car, out);
                match cdr {
                    V::Nil => break,
                    V::Pair(next) => {
                        out.push(' ');
                        cur = next;
                    }
                    other => {
                        out.push_str(" . ");
                        write(&other, out);
                        break;
                    }
                }
            }
            out.push(')');
        }
        V::Ipair(p) => {
            out.push_str("#<ilist ");
            let mut cur = p.clone();
            loop {
                write(&cur.0, out);
                match &cur.1 {
                    V::Nil => break,
                    V::Ipair(next) => {
                        out.push(' ');
                        cur = next.clone();
                    }
                    other => {
                        out.push_str(" . ");
                        write(other, out);
                        break;
                    }
                }
            }
            out.push('>');
        }
        V::Vector(items) => {
            out.push_str("#(");
            for (i, x) in items.borrow().iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                write(x, out);
            }
            out.push(')');
        }
        V::Box(_) => out.push_str("#<box>"),
        V::Clo(_) | V::Prim => out.push_str("#<procedure>"),
    }
}

/// Replaces `#<procedure anything>` with `#<procedure>`.
fn normalise(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("#<procedure") {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let end = after.find('>').unwrap();
        out.push_str("#<procedure>");
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

const LIMIT: i64 = 1 << 40;

/// A join point the interpreter has entered the scope of: its parameters
/// and its body.
type JoinScope = Option<(Vec<Var>, TermId)>;

struct Interp<'m> {
    m: &'m Module,
    consts: Vec<V>,
    globals: Vec<Option<V>>,
    steps: u64,
    joins: Vec<Vec<JoinScope>>,
}

impl<'m> Interp<'m> {
    fn new(m: &'m Module) -> Self {
        let consts = m
            .constants
            .iter()
            .map(|k| match k {
                Constant::Integer(n) => V::Int(*n),
                Constant::Boolean(b) => V::Bool(*b),
                Constant::EmptyList => V::Nil,
                Constant::Unspecified => V::Unspec,
                Constant::String(s) => V::Str(Rc::from(&**s)),
                Constant::Pair(a, b) => {
                    // Only (int . int) / small lists of ints are generated.
                    fn conv(k: &Constant) -> V {
                        match k {
                            Constant::Integer(n) => V::Int(*n),
                            Constant::EmptyList => V::Nil,
                            Constant::Pair(a, b) => {
                                V::Pair(Rc::new(RefCell::new((conv(a), conv(b)))))
                            }
                            _ => unimplemented!(),
                        }
                    }
                    V::Pair(Rc::new(RefCell::new((conv(a), conv(b)))))
                }
                _ => unimplemented!("constant {k:?}"),
            })
            .collect();
        let joins = m
            .functions
            .iter()
            .map(|f| {
                let mut js = vec![None; f.joins as usize];
                for t in &f.terms {
                    if let Term::Join {
                        join,
                        params,
                        handler,
                        ..
                    } = t
                    {
                        js[join.0 as usize] = Some((params.clone(), *handler));
                    }
                }
                js
            })
            .collect();
        Interp {
            m,
            consts,
            globals: vec![None; m.globals.len()],
            steps: 0,
            joins,
        }
    }

    fn int(&self, n: i64) -> Result<V, Stop> {
        if n.abs() >= LIMIT {
            Err(Stop::Overflow)
        } else {
            Ok(V::Int(n))
        }
    }

    fn apply(&mut self, mut callee: V, mut args: Vec<V>) -> Result<V, Stop> {
        'call: loop {
            let clo = match &callee {
                V::Clo(c) => c.clone(),
                V::Prim => {
                    // "+" only.
                    let mut sum = 0i64;
                    for a in &args {
                        match a {
                            V::Int(n) => sum += n,
                            _ => return Err(Stop::Error("+ of non-int".into())),
                        }
                    }
                    return self.int(sum);
                }
                _ => return Err(Stop::Error("not a procedure".into())),
            };
            let f = &self.m.functions[clo.fun as usize];
            let mut env: Vec<Option<V>> = vec![None; f.vars as usize];
            let fixed = f.arity.required as usize + f.arity.optional as usize;
            if args.len() < f.arity.required as usize || (!f.arity.rest && args.len() > fixed) {
                return Err(Stop::Error("arity".into()));
            }
            for i in 0..fixed {
                env[f.params[i].0 as usize] =
                    Some(args.get(i).cloned().ok_or(Stop::Error("default".into()))?);
            }
            if f.arity.rest {
                let mut list = V::Nil;
                for a in args.iter().skip(fixed).rev() {
                    list = V::Pair(Rc::new(RefCell::new((a.clone(), list))));
                }
                env[f.params[fixed].0 as usize] = Some(list);
            }
            for (i, fv) in f.free.iter().enumerate() {
                env[fv.0 as usize] = Some(clo.free.borrow()[i].clone());
            }
            let mut t = f.body;
            loop {
                self.steps += 1;
                if self.steps > 2_000_000 {
                    return Err(Stop::Timeout);
                }
                match f.term(t) {
                    Term::Let {
                        var, value, body, ..
                    } => {
                        let x = self.value(&env, value)?;
                        env[var.0 as usize] = Some(x);
                        t = *body;
                    }
                    Term::Join { body, .. } => t = *body,
                    Term::Jump { join, args } => {
                        let vals: Vec<V> = args.iter().map(|a| self.get(&env, a)).collect();
                        let (params, handler) = self.joins[clo.fun as usize][join.0 as usize]
                            .clone()
                            .unwrap();
                        for (p, x) in params.iter().zip(vals) {
                            env[p.0 as usize] = Some(x);
                        }
                        t = handler;
                    }
                    Term::If {
                        test,
                        then,
                        otherwise,
                    } => {
                        t = if matches!(self.get(&env, test), V::Bool(false)) {
                            *otherwise
                        } else {
                            *then
                        };
                    }
                    Term::Return(x) => return Ok(self.get(&env, x)),
                    Term::TailCall {
                        callee: c, args: a, ..
                    } => {
                        let nc = self.get(&env, c);
                        let na = a.iter().map(|x| self.get(&env, x)).collect();
                        callee = nc;
                        args = na;
                        continue 'call;
                    }
                    Term::TailCallDirect {
                        function, args: a, ..
                    } => {
                        args = a.iter().map(|x| self.get(&env, x)).collect();
                        callee = direct(*function);
                        continue 'call;
                    }
                }
            }
        }
    }

    fn get(&self, env: &[Option<V>], x: &Operand) -> V {
        match x {
            Operand::Var(v) => env[v.0 as usize].clone().expect("bound"),
            Operand::Const(k) => self.consts[k.0 as usize].clone(),
        }
    }

    fn value(&mut self, env: &[Option<V>], value: &Value) -> Result<V, Stop> {
        let g = |x: &Operand| self.get(env, x);
        let int = |x: V| match x {
            V::Int(n) => Ok(n),
            _ => Err(Stop::Error("not an int".into())),
        };
        Ok(match value {
            Value::Operand(x) => g(x),
            Value::Primitive { op, args } => {
                let a: Vec<V> = args.iter().map(&g).collect();
                match op {
                    Primitive::Add => self.int(int(a[0].clone())? + int(a[1].clone())?)?,
                    Primitive::Sub => self.int(int(a[0].clone())? - int(a[1].clone())?)?,
                    Primitive::Mul => {
                        let r = int(a[0].clone())?
                            .checked_mul(int(a[1].clone())?)
                            .ok_or(Stop::Overflow)?;
                        self.int(r)?
                    }
                    Primitive::Lt => V::Bool(int(a[0].clone())? < int(a[1].clone())?),
                    Primitive::Le => V::Bool(int(a[0].clone())? <= int(a[1].clone())?),
                    Primitive::NumEq => V::Bool(int(a[0].clone())? == int(a[1].clone())?),
                    Primitive::Eq => V::Bool(eq(&a[0], &a[1])),
                    Primitive::Not => V::Bool(matches!(a[0], V::Bool(false))),
                    Primitive::Cons => V::Pair(Rc::new(RefCell::new((a[0].clone(), a[1].clone())))),
                    Primitive::Ipair => V::Ipair(Rc::new((a[0].clone(), a[1].clone()))),
                    Primitive::Icar | Primitive::Icdr => match &a[0] {
                        V::Ipair(p) => {
                            if *op == Primitive::Icar {
                                p.0.clone()
                            } else {
                                p.1.clone()
                            }
                        }
                        _ => return Err(Stop::Error("icar".into())),
                    },
                    Primitive::IsIpair => V::Bool(matches!(a[0], V::Ipair(_))),
                    Primitive::Car | Primitive::Cdr => match &a[0] {
                        V::Pair(p) => {
                            let p = p.borrow();
                            if *op == Primitive::Car {
                                p.0.clone()
                            } else {
                                p.1.clone()
                            }
                        }
                        _ => return Err(Stop::Error("car".into())),
                    },
                    Primitive::SetCar | Primitive::SetCdr => match &a[0] {
                        V::Pair(p) => {
                            let mut p = p.borrow_mut();
                            if *op == Primitive::SetCar {
                                p.0 = a[1].clone()
                            } else {
                                p.1 = a[1].clone()
                            }
                            V::Unspec
                        }
                        _ => return Err(Stop::Error("set-car".into())),
                    },
                    Primitive::IsNull => V::Bool(matches!(a[0], V::Nil)),
                    Primitive::IsPair => V::Bool(matches!(a[0], V::Pair(_))),
                    Primitive::MakeVector => {
                        let n = int(a[0].clone())?;
                        V::Vector(Rc::new(RefCell::new(vec![a[1].clone(); n as usize])))
                    }
                    Primitive::VectorRef => match &a[0] {
                        V::Vector(items) => items.borrow()[int(a[1].clone())? as usize].clone(),
                        _ => return Err(Stop::Error("vector-ref".into())),
                    },
                    Primitive::VectorSet => match &a[0] {
                        V::Vector(items) => {
                            items.borrow_mut()[int(a[1].clone())? as usize] = a[2].clone();
                            V::Unspec
                        }
                        _ => return Err(Stop::Error("vector-set".into())),
                    },
                    Primitive::VectorLength => match &a[0] {
                        V::Vector(items) => V::Int(items.borrow().len() as i64),
                        _ => return Err(Stop::Error("vector-length".into())),
                    },
                }
            }
            Value::Call { callee, args } => {
                let c = g(callee);
                let a = args.iter().map(&g).collect();
                self.apply(c, a)?
            }
            Value::CallDirect { function, args } => {
                let a = args.iter().map(&g).collect();
                self.apply(direct(*function), a)?
            }
            Value::Closure { function, free } => V::Clo(Rc::new(Clo {
                fun: function.0,
                free: RefCell::new(free.iter().map(&g).collect()),
            })),
            Value::Global(gid) => match &self.m.globals[gid.0 as usize].link {
                GlobalLink::Primitive => V::Prim,
                _ => self.globals[gid.0 as usize]
                    .clone()
                    .ok_or(Stop::Error("unbound".into()))?,
            },
            Value::SetGlobal(gid, x) => {
                self.globals[gid.0 as usize] = Some(g(x));
                V::Unspec
            }
            Value::Box(x) => V::Box(Rc::new(RefCell::new(g(x)))),
            Value::Unbox(x) => match g(x) {
                V::Box(b) => b.borrow().clone(),
                _ => return Err(Stop::Error("unbox".into())),
            },
            Value::SetBox(b, x) => match g(b) {
                V::Box(b) => {
                    *b.borrow_mut() = g(x);
                    V::Unspec
                }
                _ => return Err(Stop::Error("set-box".into())),
            },
            Value::SetFree {
                closure,
                index,
                value,
            } => match g(closure) {
                V::Clo(c) => {
                    let mut free = c.free.borrow_mut();
                    if (*index as usize) >= free.len() {
                        return Err(Stop::Error("set-free index".into()));
                    }
                    free[*index as usize] = g(value);
                    V::Unspec
                }
                _ => return Err(Stop::Error("set-free".into())),
            },
        })
    }
}

fn interpret(m: &Module) -> Result<String, Stop> {
    let mut i = Interp::new(m);
    let body = V::Clo(Rc::new(Clo {
        fun: m.body.0,
        free: RefCell::new(vec![]),
    }));
    let r = i.apply(body, vec![])?;
    let mut s = String::new();
    write(&r, &mut s);
    Ok(s)
}

// ---------------------------------------------------------------------------
// A generator of valid, well-typed, terminating IR modules.

#[derive(Clone, Copy, PartialEq, Debug)]
enum K {
    Int,
    Bool,
    Pair,
    Ipair,
    Vector(u8),
    BoxInt,
    Clo(u32),
    Prim,
    Any,
}

#[derive(Clone)]
struct Sig {
    required: u8,
    optional: u8,
    rest: bool,
    free: usize,
}

#[derive(Clone)]
struct JoinCtx {
    id: JoinId,
    kinds: Vec<K>,
    /// For a loop: the operand its counter must be passed.
    first: Option<Operand>,
}

#[derive(Default, Clone)]
struct Ctx {
    vars: Vec<(Var, K)>,
    joins: Vec<JoinCtx>,
}

struct Gen<'r, R: Random> {
    r: &'r mut R,
    sigs: Vec<Sig>,
    consts: Vec<(Constant, K)>,
    fb: FB,
    fun: u32,
    /// How big to make things.
    budget: i64,
    pressure: bool,
    /// Variables every leaf uses, so they are live throughout.
    prelude: Vec<Var>,
    /// Variables a `let` of this function binds to a closure, the only
    /// ones `set-free` may name.
    made: Vec<Var>,
}

const G_DEFINED: u32 = 0;
const G_PLUS: u32 = 1;

impl<R: Random> Gen<'_, R> {
    fn below(&mut self, n: u64) -> u64 {
        self.r.below(n)
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }

    fn int_const(&mut self) -> Operand {
        let ints: Vec<usize> = self
            .consts
            .iter()
            .enumerate()
            .filter(|(_, (_, k))| *k == K::Int)
            .map(|(i, _)| i)
            .collect();
        c(ints[self.below(ints.len() as u64) as usize] as u32)
    }

    fn small_int_const(&mut self, max: i64) -> Operand {
        let ints: Vec<usize> = self
            .consts
            .iter()
            .enumerate()
            .filter(|(_, (k, kk))| {
                *kk == K::Int && matches!(k, Constant::Integer(n) if (0..=max).contains(n))
            })
            .map(|(i, _)| i)
            .collect();
        c(ints[self.below(ints.len() as u64) as usize] as u32)
    }

    fn const_of(&self, n: i64) -> Operand {
        let i = self
            .consts
            .iter()
            .position(|(k, _)| matches!(k, Constant::Integer(m) if *m == n))
            .unwrap();
        c(i as u32)
    }

    fn var_of(&mut self, ctx: &Ctx, pred: impl Fn(K) -> bool) -> Option<(Var, K)> {
        let vs: Vec<(Var, K)> = ctx.vars.iter().copied().filter(|(_, k)| pred(*k)).collect();
        if vs.is_empty() {
            None
        } else {
            // Bias to recent variables, and sometimes old ones.
            let i = if self.chance(60) {
                vs.len() - 1 - (self.below(vs.len().min(4) as u64) as usize)
            } else {
                self.below(vs.len() as u64) as usize
            };
            Some(vs[i])
        }
    }

    fn int(&mut self, ctx: &Ctx) -> Operand {
        if self.chance(75)
            && let Some((x, _)) = self.var_of(ctx, |k| k == K::Int)
        {
            return v(x);
        }
        self.int_const()
    }

    fn any(&mut self, ctx: &Ctx) -> (Operand, K) {
        if self.chance(80) && !ctx.vars.is_empty() {
            let (x, k) = self.var_of(ctx, |_| true).unwrap();
            return (v(x), k);
        }
        let i = self.below(self.consts.len() as u64) as usize;
        (c(i as u32), self.consts[i].1)
    }

    fn of_kind(&mut self, ctx: &Ctx, k: K) -> Operand {
        match k {
            K::Int => self.int(ctx),
            K::Any => self.any(ctx).0,
            _ => unreachable!(),
        }
    }

    fn leaf(&mut self, ctx: &Ctx) -> TermId {
        if self.prelude.is_empty() || self.below(100) < 15 {
            return self.plain_leaf(ctx);
        }
        // Use every prelude variable here, so all stay live everywhere.
        let prelude = self.prelude.clone();
        let sums: Vec<Var> = prelude.iter().map(|_| self.fb.var()).collect();
        let mut ctx2 = ctx.clone();
        ctx2.vars.push((sums[sums.len() - 1], K::Int));
        let mut t = if self.chance(50) {
            self.fb.push(Term::Return(v(sums[sums.len() - 1])))
        } else {
            self.plain_leaf(&ctx2)
        };
        for i in (0..prelude.len()).rev() {
            let prev = if i == 0 {
                self.const_of(0)
            } else {
                v(sums[i - 1])
            };
            // Alternate the operand order so both sides are exercised.
            let args = if i % 2 == 0 {
                vec![prev, v(prelude[i])]
            } else {
                vec![v(prelude[i]), prev]
            };
            t = let_(
                &mut self.fb,
                sums[i],
                Value::Primitive {
                    op: Primitive::Add,
                    args,
                },
                t,
            );
        }
        t
    }

    fn plain_leaf(&mut self, ctx: &Ctx) -> TermId {
        loop {
            match self.below(4) {
                0 | 1 if !ctx.joins.is_empty() => {
                    let j = ctx.joins[self.below(ctx.joins.len() as u64) as usize].clone();
                    let mut args = Vec::new();
                    for (i, &k) in j.kinds.iter().enumerate() {
                        if i == 0
                            && let Some(first) = j.first
                        {
                            args.push(first);
                        } else {
                            args.push(self.of_kind(ctx, k));
                        }
                    }
                    return self.fb.push(Term::Jump { join: j.id, args });
                }
                2 => {
                    if let Some((callee, k)) =
                        self.var_of(ctx, |k| matches!(k, K::Clo(_) | K::Prim))
                    {
                        let args = self.call_args(ctx, k);
                        return self.fb.push(Term::TailCall {
                            callee: v(callee),
                            args,
                            position: None,
                        });
                    }
                }
                _ => {
                    let (x, _) = self.any(ctx);
                    return self.fb.push(Term::Return(x));
                }
            }
        }
    }

    fn call_args(&mut self, ctx: &Ctx, k: K) -> Vec<Operand> {
        match k {
            K::Prim => {
                let n = self.below(4) as usize;
                (0..n).map(|_| self.int(ctx)).collect()
            }
            K::Clo(j) => {
                let sig = self.sigs[j as usize].clone();
                let mut n = sig.required as usize + sig.optional as usize;
                if sig.rest {
                    n += if self.pressure && self.chance(30) {
                        self.below(250) as usize
                    } else {
                        self.below(4) as usize
                    };
                }
                (0..n)
                    .map(|i| {
                        if i < sig.required as usize + sig.optional as usize {
                            self.int(ctx)
                        } else {
                            self.any(ctx).0
                        }
                    })
                    .collect()
            }
            _ => unreachable!(),
        }
    }

    /// A value for a `let`, and its kind.
    fn value(&mut self, ctx: &Ctx) -> (Value, K) {
        loop {
            let choice = self.below(26);
            let prim = |op, args| Value::Primitive { op, args };
            match choice {
                0 => {
                    let (x, k) = self.any(ctx);
                    return (Value::Operand(x), k);
                }
                1..=3 => {
                    let op = [Primitive::Add, Primitive::Sub, Primitive::Add][choice as usize - 1];
                    let a = self.int(ctx);
                    let b = self.int(ctx);
                    return (prim(op, vec![a, b]), K::Int);
                }
                4 => {
                    let a = self.int(ctx);
                    let b = self.small_int_const(3);
                    return (prim(Primitive::Mul, vec![a, b]), K::Int);
                }
                5 | 6 => {
                    let op =
                        [Primitive::Lt, Primitive::Le, Primitive::NumEq][self.below(3) as usize];
                    let a = self.int(ctx);
                    let b = self.int(ctx);
                    return (prim(op, vec![a, b]), K::Bool);
                }
                7 => {
                    let a = self.any(ctx).0;
                    let b = self.any(ctx).0;
                    return (prim(Primitive::Eq, vec![a, b]), K::Bool);
                }
                8 => {
                    let a = self.any(ctx).0;
                    let op = [Primitive::Not, Primitive::IsNull, Primitive::IsPair]
                        [self.below(3) as usize];
                    return (prim(op, vec![a]), K::Bool);
                }
                9 => {
                    let a = self.any(ctx).0;
                    let b = self.any(ctx).0;
                    return (prim(Primitive::Cons, vec![a, b]), K::Pair);
                }
                10 => {
                    let a = self.any(ctx).0;
                    let b = self.any(ctx).0;
                    return (prim(Primitive::Ipair, vec![a, b]), K::Ipair);
                }
                24 => {
                    if let Some((p, _)) = self.var_of(ctx, |k| k == K::Ipair) {
                        let op = if self.chance(50) {
                            Primitive::Icar
                        } else {
                            Primitive::Icdr
                        };
                        return (prim(op, vec![v(p)]), K::Any);
                    }
                }
                25 => {
                    let a = self.any(ctx).0;
                    return (prim(Primitive::IsIpair, vec![a]), K::Bool);
                }
                11 => {
                    if let Some((p, _)) = self.var_of(ctx, |k| k == K::Pair) {
                        let op = if self.chance(50) {
                            Primitive::Car
                        } else {
                            Primitive::Cdr
                        };
                        return (prim(op, vec![v(p)]), K::Any);
                    }
                }
                12 => {
                    if let Some((p, _)) = self.var_of(ctx, |k| k == K::Pair) {
                        let op = if self.chance(50) {
                            Primitive::SetCar
                        } else {
                            Primitive::SetCdr
                        };
                        let x = self.int(ctx);
                        return (prim(op, vec![v(p), x]), K::Any);
                    }
                }
                13 => {
                    let n = self.small_int_const(3);
                    let len = match n {
                        Operand::Const(k) => match self.consts[k.0 as usize].0 {
                            Constant::Integer(n) => n as u8,
                            _ => unreachable!(),
                        },
                        _ => unreachable!(),
                    };
                    let fill = self.any(ctx).0;
                    return (prim(Primitive::MakeVector, vec![n, fill]), K::Vector(len));
                }
                14 => {
                    if let Some((x, K::Vector(n))) =
                        self.var_of(ctx, |k| matches!(k, K::Vector(n) if n > 0))
                    {
                        let i = self.small_int_const(n as i64 - 1);
                        if self.chance(50) {
                            return (prim(Primitive::VectorRef, vec![v(x), i]), K::Any);
                        }
                        // An int: anything else could make a cycle.
                        let y = self.int(ctx);
                        return (prim(Primitive::VectorSet, vec![v(x), i, y]), K::Any);
                    }
                    if let Some((x, _)) = self.var_of(ctx, |k| matches!(k, K::Vector(_))) {
                        return (prim(Primitive::VectorLength, vec![v(x)]), K::Int);
                    }
                }
                15 => {
                    let x = self.int(ctx);
                    return (Value::Box(x), K::BoxInt);
                }
                16 => {
                    if let Some((b, _)) = self.var_of(ctx, |k| k == K::BoxInt) {
                        if self.chance(50) {
                            return (Value::Unbox(v(b)), K::Int);
                        }
                        let x = self.int(ctx);
                        return (Value::SetBox(v(b), x), K::Any);
                    }
                }
                17 => {
                    let n = self.sigs.len() as u32;
                    if self.fun + 1 < n {
                        let j = self.fun + 1 + self.below((n - self.fun - 1) as u64) as u32;
                        let free = (0..self.sigs[j as usize].free)
                            .map(|_| self.int(ctx))
                            .collect();
                        return (
                            Value::Closure {
                                function: FunId(j),
                                free,
                            },
                            K::Clo(j),
                        );
                    }
                }
                18..=20 => {
                    if let Some((f, k)) = self.var_of(ctx, |k| matches!(k, K::Clo(_) | K::Prim)) {
                        let args = self.call_args(ctx, k);
                        let rk = if k == K::Prim { K::Int } else { K::Any };
                        return (Value::Call { callee: v(f), args }, rk);
                    }
                }
                21 => {
                    if self.chance(50) {
                        return (Value::Global(GlobalId(G_DEFINED)), K::Int);
                    }
                    if self.chance(50) {
                        return (Value::Global(GlobalId(G_PLUS)), K::Prim);
                    }
                    let x = self.int(ctx);
                    return (Value::SetGlobal(GlobalId(G_DEFINED), x), K::Any);
                }
                22 => {
                    if let Some((f, K::Clo(j))) = self.var_of(ctx, |k| matches!(k, K::Clo(_)))
                        && self.made.contains(&f)
                    {
                        let n = self.sigs[j as usize].free;
                        if n > 0 {
                            let index = self.below(n as u64) as u32;
                            let x = self.int(ctx);
                            return (
                                Value::SetFree {
                                    closure: v(f),
                                    index,
                                    value: x,
                                },
                                K::Any,
                            );
                        }
                    }
                }
                _ => {
                    let a = self.int(ctx);
                    let b = self.int(ctx);
                    return (prim(Primitive::Add, vec![a, b]), K::Int);
                }
            }
        }
    }

    fn term(&mut self, ctx: &mut Ctx, depth: u32) -> TermId {
        self.budget -= 1;
        if depth > 40 || self.budget <= 0 {
            return self.leaf(ctx);
        }
        let choice = self.below(100);
        if choice < 45 {
            let (value, k) = self.value(ctx);
            let x = self.fb.var();
            if matches!(value, Value::Closure { .. }) {
                self.made.push(x);
            }
            ctx.vars.push((x, k));
            let body = self.term(ctx, depth + 1);
            ctx.vars.pop();
            let_(&mut self.fb, x, value, body)
        } else if choice < 58 {
            // A comparison and an `if`, usually fusible.
            let x = self.fb.var();
            let op = [
                Primitive::Lt,
                Primitive::Le,
                Primitive::NumEq,
                Primitive::Eq,
                Primitive::IsNull,
                Primitive::IsPair,
                Primitive::IsIpair,
            ][self.below(7) as usize];
            let args = match op {
                Primitive::IsNull | Primitive::IsPair | Primitive::IsIpair => vec![self.any(ctx).0],
                Primitive::Eq => vec![self.any(ctx).0, self.any(ctx).0],
                _ => vec![self.int(ctx), self.int(ctx)],
            };
            let reuse = self.chance(15);
            ctx.vars.push((x, K::Bool));
            if !reuse {
                ctx.vars.pop();
            }
            let then = self.term(ctx, depth + 1);
            let otherwise = self.term(ctx, depth + 1);
            if reuse {
                ctx.vars.pop();
            }
            let iff = self.fb.push(Term::If {
                test: v(x),
                then,
                otherwise,
            });
            let_(&mut self.fb, x, Value::Primitive { op, args }, iff)
        } else if choice < 64 {
            let (test, _) = self.any(ctx);
            let then = self.term(ctx, depth + 1);
            let otherwise = self.term(ctx, depth + 1);
            self.fb.push(Term::If {
                test,
                then,
                otherwise,
            })
        } else if choice < 74 {
            // A join point.
            let j = self.fb.join();
            let n = self.below(4) as usize;
            let kinds: Vec<K> = (0..n)
                .map(|_| if self.chance(60) { K::Int } else { K::Any })
                .collect();
            let params: Vec<Var> = (0..n).map(|_| self.fb.var()).collect();
            let saved = ctx.vars.len();
            for (p, k) in params.iter().zip(&kinds) {
                ctx.vars.push((*p, *k));
            }
            let handler = self.term(ctx, depth + 1);
            ctx.vars.truncate(saved);
            ctx.joins.push(JoinCtx {
                id: j,
                kinds,
                first: None,
            });
            let body = self.term(ctx, depth + 1);
            ctx.joins.pop();
            self.fb.push(Term::Join {
                join: j,
                params,
                recursive: false,
                handler,
                body,
            })
        } else if choice < 84 {
            // A loop: rec join j(i, ...) = if i < K then (let i1 = i + 1 in ...) else ...
            let j = self.fb.join();
            let n = self.below(3) as usize;
            let mut kinds = vec![K::Int];
            kinds.extend((0..n).map(|_| if self.chance(60) { K::Int } else { K::Any }));
            let params: Vec<Var> = kinds.iter().map(|_| self.fb.var()).collect();
            let saved = ctx.vars.len();
            for (p, k) in params.iter().zip(&kinds) {
                ctx.vars.push((*p, *k));
            }
            let t = self.fb.var();
            let i1 = self.fb.var();
            ctx.vars.push((i1, K::Int));
            ctx.joins.push(JoinCtx {
                id: j,
                kinds: kinds.clone(),
                first: Some(v(i1)),
            });
            let then_body = self.term(ctx, depth + 1);
            ctx.joins.pop();
            ctx.vars.pop();
            let one = self.const_of(1);
            let then = let_(
                &mut self.fb,
                i1,
                Value::Primitive {
                    op: Primitive::Add,
                    args: vec![v(params[0]), one],
                },
                then_body,
            );
            let otherwise = self.term(ctx, depth + 1);
            let iff = self.fb.push(Term::If {
                test: v(t),
                then,
                otherwise,
            });
            let bound = self.small_int_const(3);
            let handler = let_(
                &mut self.fb,
                t,
                Value::Primitive {
                    op: Primitive::Lt,
                    args: vec![v(params[0]), bound],
                },
                iff,
            );
            ctx.vars.truncate(saved);
            let start = self.small_int_const(1);
            ctx.joins.push(JoinCtx {
                id: j,
                kinds,
                first: Some(start),
            });
            let body = self.term(ctx, depth + 1);
            ctx.joins.pop();
            self.fb.push(Term::Join {
                join: j,
                params,
                recursive: true,
                handler,
                body,
            })
        } else if choice < 88 && self.pressure {
            // Many values live at once, consumed together.
            let wide = self.chance(20);
            let n = 5 + self.below(if wide { 250 } else { 60 }) as usize;
            let xs: Vec<Var> = (0..n).map(|_| self.fb.var()).collect();
            let mut values = Vec::new();
            for _ in 0..n {
                let a = self.int(ctx);
                let b = self.int_const();
                values.push(Value::Primitive {
                    op: Primitive::Add,
                    args: vec![a, b],
                });
            }
            let saved = ctx.vars.len();
            // Consume: acc_0 = cons(x_0, '()), acc_i = cons(x_i, acc_{i-1}) in reverse.
            let accs: Vec<Var> = (0..n).map(|_| self.fb.var()).collect();
            for &a in &accs {
                let _ = a;
            }
            ctx.vars.push((accs[n - 1], K::Pair));
            let rest = self.term(ctx, depth + 1);
            ctx.vars.truncate(saved);
            let mut t = rest;
            let nil = self
                .consts
                .iter()
                .position(|(k, _)| matches!(k, Constant::EmptyList))
                .unwrap() as u32;
            for i in (0..n).rev() {
                let prev = if i == 0 { c(nil) } else { v(accs[i - 1]) };
                // Consume in the opposite order to definition so all stay live.
                t = let_(
                    &mut self.fb,
                    accs[i],
                    Value::Primitive {
                        op: Primitive::Cons,
                        args: vec![v(xs[n - 1 - i]), prev],
                    },
                    t,
                );
            }
            for i in (0..n).rev() {
                t = let_(&mut self.fb, xs[i], values[i].clone(), t);
            }
            t
        } else {
            self.leaf(ctx)
        }
    }
}

fn random_module(r: &mut impl Random) -> Module {
    let pressure = r.below(3) == 0;
    let nfun = 1 + r.below(5) as usize;
    let mut sigs = vec![Sig {
        required: 0,
        optional: 0,
        rest: false,
        free: 0,
    }];
    for _ in 1..nfun {
        let rest = r.below(5) == 0;
        sigs.push(Sig {
            required: if r.below(4) == 0 {
                r.below(9) as u8
            } else {
                r.below(4) as u8
            },
            optional: if r.below(6) == 0 { 1 } else { 0 },
            rest,
            free: r.below(4) as usize,
        });
    }
    let mut consts: Vec<(Constant, K)> = vec![
        (Constant::Integer(0), K::Int),
        (Constant::Integer(1), K::Int),
        (Constant::Integer(2), K::Int),
        (Constant::Integer(3), K::Int),
        (Constant::Integer(-1), K::Int),
        (Constant::Integer(127), K::Int),
        (Constant::Integer(128), K::Int),
        (Constant::Integer(-129), K::Int),
        (Constant::Integer(40000), K::Int),
        (Constant::Integer(1 << 33), K::Int),
        (Constant::Boolean(true), K::Bool),
        (Constant::Boolean(false), K::Bool),
        (Constant::EmptyList, K::Any),
        (Constant::Unspecified, K::Any),
        (Constant::String("s".into()), K::Any),
        (
            Constant::Pair(
                Box::new(Constant::Integer(1)),
                Box::new(Constant::EmptyList),
            ),
            K::Any,
        ),
    ];
    // Shuffle a little so the pool order varies.
    if r.below(2) == 0 {
        consts.reverse();
    }
    let globals = vec![
        Global {
            name: "g".into(),
            link: GlobalLink::Defined,
        },
        Global {
            name: "+".into(),
            link: GlobalLink::Primitive,
        },
    ];
    let mut functions = Vec::new();
    let budget = if pressure {
        60
    } else {
        10 + r.below(50) as i64
    };
    let call_heavy = r.below(2) == 0;
    let prelude_max = match r.below(4) {
        0 | 1 => 0,
        2 => 20,
        _ => 250,
    };
    let mut g = Gen {
        r,
        sigs: sigs.clone(),
        consts: consts.clone(),
        fb: FB::default(),
        fun: 0,
        budget,
        pressure,
        prelude: Vec::new(),
        made: Vec::new(),
    };
    for (i, sig) in sigs.iter().enumerate() {
        g.fun = i as u32;
        g.made.clear();
        g.fb = FB::default();
        g.budget = budget;
        let mut ctx = Ctx::default();
        let nparams = sig.required as usize + sig.optional as usize + sig.rest as usize;
        let params: Vec<Var> = (0..nparams).map(|_| g.fb.var()).collect();
        let free: Vec<Var> = (0..sig.free).map(|_| g.fb.var()).collect();
        for (k, p) in params.iter().enumerate() {
            let kind = if sig.rest && k == nparams - 1 {
                K::Any
            } else {
                K::Int
            };
            ctx.vars.push((*p, kind));
        }
        for f in &free {
            ctx.vars.push((*f, K::Int));
        }
        // Preludes: closures of every later function, and values every
        // leaf uses.
        let mut wrap: Vec<(Var, Value)> = Vec::new();
        if call_heavy {
            for (j, later) in sigs.iter().enumerate().skip(i + 1) {
                let free = (0..later.free).map(|_| g.int(&ctx)).collect();
                let k = g.fb.var();
                wrap.push((
                    k,
                    Value::Closure {
                        function: FunId(j as u32),
                        free,
                    },
                ));
                ctx.vars.push((k, K::Clo(j as u32)));
                g.made.push(k);
            }
        }
        g.prelude.clear();
        let n = if prelude_max == 0 {
            0
        } else {
            g.below(prelude_max) as usize
        };
        for _ in 0..n {
            let a = g.int(&ctx);
            let b = g.int_const();
            let x = g.fb.var();
            wrap.push((
                x,
                Value::Primitive {
                    op: Primitive::Add,
                    args: vec![a, b],
                },
            ));
            ctx.vars.push((x, K::Int));
            g.prelude.push(x);
        }
        let mut body = g.term(&mut ctx, 0);
        for (x, value) in wrap.into_iter().rev() {
            body = let_(&mut g.fb, x, value, body);
        }
        if i == 0 {
            // The body sets the global first.
            let u = g.fb.var();
            let one = g.const_of(1);
            body = let_(
                &mut g.fb,
                u,
                Value::SetGlobal(GlobalId(G_DEFINED), one),
                body,
            );
        }
        let fb = std::mem::take(&mut g.fb);
        functions.push(fb.finish(
            &format!("f{i}"),
            Arity {
                required: sig.required,
                optional: sig.optional,
                rest: sig.rest,
            },
            params,
            free,
            body,
        ));
    }
    Module {
        name: "random".into(),
        constants: consts.into_iter().map(|(k, _)| k).collect(),
        globals,
        exports: Vec::new(),
        functions,
        body: FunId(0),
        sources: Vec::new(),
    }
}

/// Runs one seed: Ok(true) if it ran and matched, Ok(false) if skipped.
fn check_seed(seed: u64) -> Result<bool, String> {
    let m = random_module(&mut XorShift::new(seed));
    if let Err(e) = ir::validate(&m) {
        return Err(format!("GENERATOR BUG: {e}\n{m}"));
    }
    let expected = interpret(&m);
    if std::env::var("IR_PRINT").is_ok() {
        eprintln!("{m}\nexpected {expected:?}");
    }
    let bc = match convert_checked(&m) {
        Ok(bc) => bc,
        Err(e) if e.starts_with("convert error") => return Ok(false),
        Err(e) => return Err(format!("{e}\n{m}")),
    };
    let expected = match expected {
        Ok(s) => Some(s),
        Err(Stop::Overflow) | Err(Stop::Timeout) => return Ok(false),
        Err(Stop::Error(e)) => {
            return Err(format!("GENERATOR TYPE BUG: {e}\n{m}"));
        }
    };
    for (s, r) in run_all(&bc) {
        let got = r.map(|x| normalise(&x));
        if got.as_ref().ok() != expected.as_ref() {
            return Err(format!(
                "MISMATCH under {s}: got {got:?}, expected {expected:?}\n{m}\n{}",
                dis(&bc)
            ));
        }
    }
    Ok(true)
}

#[test]
fn random_valid_ir() {
    let n: u64 = std::env::var("IR_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let first: u64 = std::env::var("IR_FIRST")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let mut failures = Vec::new();
    let mut ran = 0;
    let mut skipped = 0;
    let trace = std::env::var("IR_TRACE").is_ok();
    for seed in first..first + n {
        if trace {
            eprintln!("seed {seed}");
        }
        match check_seed(seed) {
            Ok(true) => ran += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                if failures.len() < 3 {
                    eprintln!("seed {seed}: {e}");
                }
                failures.push(seed);
            }
        }
    }
    eprintln!(
        "ran {ran}, skipped {skipped}, failed {}: {:?}",
        failures.len(),
        &failures[..failures.len().min(50)]
    );
    assert!(failures.is_empty());
}

#[test]
#[ignore = "one seed"]
fn one_seed() {
    let seed: u64 = std::env::var("IR_SEED").unwrap().parse().unwrap();
    match check_seed(seed) {
        Ok(x) => eprintln!("ok {x}"),
        Err(e) => panic!("{e}"),
    }
}

/// What a direct call to `function` calls: a closure of it with no free
/// variables.
fn direct(function: FunId) -> V {
    V::Clo(Rc::new(Clo {
        fun: function.0,
        free: RefCell::new(Vec::new()),
    }))
}

// ---------------------------------------------------------------------------
// Contification and the order of a closure's ties and its call
// (ADR 0017 #9, #11).

/// `f1(i) [self]`: loops by tail calling `self` while `i < 3`, then returns
/// `i`. Constants 0, 1, 3 and `#<unspecified>` are `c(0)` to `c(3)`.
fn tie_loop() -> Function {
    let mut fb = FB::default();
    let i = fb.var();
    let me = fb.var();
    let (t, i1) = (fb.var(), fb.var());
    let call = fb.push(Term::TailCall {
        callee: v(me),
        args: vec![v(i1)],
        position: None,
    });
    let then = let_(
        &mut fb,
        i1,
        Value::Primitive {
            op: Primitive::Add,
            args: vec![v(i), c(1)],
        },
        call,
    );
    let otherwise = fb.push(Term::Return(v(i)));
    let iff = fb.push(Term::If {
        test: v(t),
        then,
        otherwise,
    });
    let body = let_(
        &mut fb,
        t,
        Value::Primitive {
            op: Primitive::Lt,
            args: vec![v(i), c(2)],
        },
        iff,
    );
    fb.finish(
        "f1",
        Arity {
            required: 1,
            optional: 0,
            rest: false,
        },
        vec![i],
        vec![me],
        body,
    )
}

/// A module of `f0`, built by `build`, and `tie_loop`.
fn tie_module(build: impl FnOnce(&mut FB) -> TermId) -> Module {
    let consts = vec![
        Constant::Integer(0),
        Constant::Integer(1),
        Constant::Integer(3),
        Constant::Unspecified,
    ];
    let mut fb = FB::default();
    let body = build(&mut fb);
    let f0 = fb.finish(
        "f0",
        Arity {
            required: 0,
            optional: 0,
            rest: false,
        },
        vec![],
        vec![],
        body,
    );
    module(consts, vec![], vec![f0, tie_loop()])
}

fn make_closure(fb: &mut FB, k: Var, body: TermId) -> TermId {
    let_(
        fb,
        k,
        Value::Closure {
            function: FunId(1),
            free: vec![c(3)],
        },
        body,
    )
}

fn tie(fb: &mut FB, u: Var, k: Var, body: TermId) -> TermId {
    let_(
        fb,
        u,
        Value::SetFree {
            closure: v(k),
            index: 0,
            value: v(k),
        },
        body,
    )
}

fn call_k(fb: &mut FB, r: Var, k: Var, body: TermId) -> TermId {
    let_(
        fb,
        r,
        Value::Call {
            callee: v(k),
            args: vec![c(0)],
        },
        body,
    )
}

/// What a module gives, which every memory strategy must agree on.
fn answer_of(m: &Module) -> Result<String, String> {
    let bytecode = convert_checked(m).unwrap();
    let mut runs = run_all(&bytecode).into_iter();
    let (_, first) = runs.next().unwrap();
    for (name, run) in runs {
        assert_eq!(run, first, "{name} disagrees");
    }
    first
}

fn closures_in_f0(m: &Module) -> usize {
    m.functions[0]
        .terms
        .iter()
        .filter(|t| {
            matches!(
                t,
                Term::Let {
                    value: Value::Closure { .. },
                    ..
                }
            )
        })
        .count()
}

/// Runs `contify` on a copy of `m` and checks the result still validates.
fn contified(m: &Module) -> Module {
    let mut out = m.clone();
    vali_opt::contify(&mut out);
    ir::validate(&out).unwrap();
    out
}

/// What the validator says about `m`, which it must refuse.
fn refused(m: &Module) -> String {
    let why = ir::validate(m).unwrap_err().to_string();
    assert!(why.contains("function f0"), "{why}");
    assert!(why.contains("v0"), "{why}");
    // Conversion validates first, so such IR never runs or optimises.
    let converted = convert_checked(m).unwrap_err();
    assert!(converted.contains("tied to itself"), "{converted}");
    why
}

/// A letrec-style closure whose self-tie comes after its call would reach
/// the placeholder: the validator refuses it (ADR 0017 #13).
#[test]
fn validator_refuses_tie_after_call() {
    let m = tie_module(|fb| {
        let (k, r, u) = (fb.var(), fb.var(), fb.var());
        let ret = fb.push(Term::Return(v(r)));
        let t = tie(fb, u, k, ret);
        let t = call_k(fb, r, k, t);
        make_closure(fb, k, t)
    });
    let why = refused(&m);
    assert!(why.contains("not yet tied to itself"), "{why}");
}

/// The tie is in the branch of an `if` the call is not in.
#[test]
fn validator_refuses_tie_in_other_branch() {
    let m = tie_module(|fb| {
        let (k, r, u, t) = (fb.var(), fb.var(), fb.var(), fb.var());
        let ret = fb.push(Term::Return(c(0)));
        let then = tie(fb, u, k, ret);
        let ret = fb.push(Term::Return(v(r)));
        let otherwise = call_k(fb, r, k, ret);
        let iff = fb.push(Term::If {
            test: v(t),
            then,
            otherwise,
        });
        let test = let_(
            fb,
            t,
            Value::Primitive {
                op: Primitive::Lt,
                args: vec![c(1), c(0)],
            },
            iff,
        );
        make_closure(fb, k, test)
    });
    refused(&m);
}

/// The tie is in a join's body and the call in its handler. The tie runs
/// first, but ancestry is conservative across a join: the validator
/// refuses it.
#[test]
fn validator_refuses_tie_in_join_body_call_in_handler() {
    let m = tie_module(|fb| {
        let (k, r, u) = (fb.var(), fb.var(), fb.var());
        let j = fb.join();
        let ret = fb.push(Term::Return(v(r)));
        let handler = call_k(fb, r, k, ret);
        let jump = fb.push(Term::Jump {
            join: j,
            args: vec![],
        });
        let body = tie(fb, u, k, jump);
        let join = fb.push(Term::Join {
            join: j,
            params: vec![],
            recursive: false,
            handler,
            body,
        });
        make_closure(fb, k, join)
    });
    refused(&m);
}

/// A closure tied, then called, which is what a named `let` lowers to,
/// still contifies.
#[test]
fn contify_tie_before_call() {
    let m = tie_module(|fb| {
        let (k, r, u) = (fb.var(), fb.var(), fb.var());
        let ret = fb.push(Term::Return(v(r)));
        let t = call_k(fb, r, k, ret);
        let t = tie(fb, u, k, t);
        make_closure(fb, k, t)
    });
    let before = answer_of(&m);
    assert_eq!(before, Ok("3".into()));
    let after = contified(&m);
    assert_eq!(closures_in_f0(&after), 0, "not contified");
    assert_eq!(answer_of(&after), before);
}
