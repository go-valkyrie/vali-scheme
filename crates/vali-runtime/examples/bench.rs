//! First tier-0 benchmarks, on hand-assembled bytecode, under no
//! collection. Run with `cargo run --release -p vali-runtime --example
//! bench`. These are the M1 baselines that counting strategies are
//! measured against (ADR 0003).

use std::time::Instant;

use vali_bytecode::{Arity, Constant, FunctionBuilder, Instruction as I, ModuleBuilder};
use vali_runtime::{Cx, Error, Handle, HeapConfig, ModuleId, NoCollection, Value};

type Builder = fn(&mut ModuleBuilder) -> Vec<(&'static str, u32)>;

/// Loads and runs a module defining each returned function as a variable.
fn load(cx: &mut Cx<NoCollection>, name: &str, build: Builder) -> ModuleId {
    let mut m = ModuleBuilder::new(name);
    let definitions = build(&mut m);
    let unspecified = m.constant(Constant::Unspecified);
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 2, 0);
    for (name, function) in definitions {
        let variable = m.variable(name);
        body.emit(I::Closure {
            dst: 1,
            function,
            free: vec![],
        })
        .emit(I::SetVariable { src: 1, variable });
    }
    body.emit(I::LoadConstant {
        dst: 1,
        constant: unspecified,
    })
    .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    let id = cx.load(m.finish(body)).unwrap();
    cx.run(id).unwrap();
    id
}

/// `(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))`
fn fib(m: &mut ModuleBuilder) -> Vec<(&'static str, u32)> {
    let fib = m.variable("fib");
    let mut f = FunctionBuilder::new(Some("fib"), Arity::exactly(1), 11, 0);
    let recur = f.label();
    f.branch(
        I::IfLtImm {
            a: 1,
            imm: 2,
            offset: 0,
        },
        recur,
    );
    f.emit(I::Return { src: 1 });
    f.bind(recur);
    f.emit(I::GetVariable {
        dst: 5,
        variable: fib,
    })
    .emit(I::SubImm {
        dst: 6,
        a: 1,
        imm: 1,
    })
    .emit(I::Call { base: 2, argc: 1 })
    .emit(I::GetVariable {
        dst: 9,
        variable: fib,
    })
    .emit(I::SubImm {
        dst: 10,
        a: 1,
        imm: 2,
    })
    .emit(I::Call { base: 6, argc: 1 })
    .emit(I::Add { dst: 1, a: 5, b: 9 })
    .emit(I::Return { src: 1 });
    vec![("fib", m.function(f.finish().unwrap()))]
}

/// `(define (tak x y z) (if (not (< y x)) z
///    (tak (tak (- x 1) y z) (tak (- y 1) z x) (tak (- z 1) x y))))`
fn tak(m: &mut ModuleBuilder) -> Vec<(&'static str, u32)> {
    let tak = m.variable("tak");
    let mut f = FunctionBuilder::new(Some("tak"), Arity::exactly(3), 23, 0);
    let return_z = f.label();
    f.branch(
        I::IfLt {
            a: 2,
            b: 1,
            offset: 0,
        },
        return_z,
    );
    // Each call's result lands below the next call's base, so it survives.
    let calls: [(u16, [u16; 3], u16); 3] =
        [(4, [1, 2, 3], 1), (8, [2, 3, 1], 2), (12, [3, 1, 2], 3)];
    for (base, args, decremented) in calls {
        f.emit(I::GetVariable {
            dst: base + 3,
            variable: tak,
        });
        for (i, &arg) in args.iter().enumerate() {
            let dst = base + 4 + i as u16;
            if arg == decremented && i == 0 {
                f.emit(I::SubImm {
                    dst,
                    a: arg,
                    imm: 1,
                });
            } else {
                f.emit(I::Mov { dst, src: arg });
            }
        }
        f.emit(I::Call { base, argc: 3 });
    }
    f.emit(I::GetVariable {
        dst: 19,
        variable: tak,
    })
    .emit(I::Mov { dst: 20, src: 7 })
    .emit(I::Mov { dst: 21, src: 11 })
    .emit(I::Mov { dst: 22, src: 15 })
    .emit(I::TailCall { proc: 19, argc: 3 });
    f.bind(return_z);
    f.emit(I::Return { src: 3 });
    vec![("tak", m.function(f.finish().unwrap()))]
}

/// The allocation churn of `tests/regressions/allocation-without-global-lock.scm`:
/// a loop allocating a pair, a two-element vector and a closure per
/// iteration, keeping one closure's result in a thousand.
fn churn(m: &mut ModuleBuilder) -> Vec<(&'static str, u32)> {
    let loop_var = m.variable("churn-loop");
    let length = m.variable("length");
    let remainder = m.variable("remainder");

    let mut thunk = FunctionBuilder::new(Some("thunk"), Arity::exactly(0), 2, 1);
    thunk
        .emit(I::Free { dst: 1, index: 0 })
        .emit(I::Return { src: 1 });
    let thunk = m.function(thunk.finish().unwrap());

    // (churn-loop n i keep)
    let mut f = FunctionBuilder::new(Some("churn-loop"), Arity::exactly(3), 14, 0);
    let body = f.label();
    let skip = f.label();
    f.branch(
        I::IfNumEq {
            a: 2,
            b: 1,
            offset: 0,
        },
        body,
    );
    f.emit(I::GetVariable {
        dst: 4,
        variable: length,
    })
    .emit(I::Mov { dst: 5, src: 3 })
    .emit(I::TailCall { proc: 4, argc: 1 });
    f.bind(body);
    f.emit(I::Cons {
        dst: 4,
        car: 2,
        cdr: 2,
    })
    .emit(I::LoadInt { dst: 5, value: 2 })
    .emit(I::MakeVector {
        dst: 5,
        len: 5,
        fill: 4,
    })
    .emit(I::Closure {
        dst: 6,
        function: thunk,
        free: vec![5],
    })
    .emit(I::GetVariable {
        dst: 10,
        variable: remainder,
    })
    .emit(I::Mov { dst: 11, src: 2 })
    .emit(I::LoadInt {
        dst: 12,
        value: 1000,
    })
    .emit(I::Call { base: 7, argc: 2 })
    .emit(I::LoadInt { dst: 11, value: 0 })
    .emit(I::Mov { dst: 13, src: 3 });
    f.branch(
        I::IfNumEq {
            a: 10,
            b: 11,
            offset: 0,
        },
        skip,
    );
    f.emit(I::Mov { dst: 10, src: 6 })
        .emit(I::Call { base: 7, argc: 0 })
        .emit(I::Cons {
            dst: 13,
            car: 10,
            cdr: 3,
        });
    f.bind(skip);
    f.emit(I::GetVariable {
        dst: 4,
        variable: loop_var,
    })
    .emit(I::Mov { dst: 5, src: 1 })
    .emit(I::AddImm {
        dst: 6,
        a: 2,
        imm: 1,
    })
    .emit(I::Mov { dst: 7, src: 13 })
    .emit(I::TailCall { proc: 4, argc: 3 });
    let looping = m.function(f.finish().unwrap());

    // (churn n) = (churn-loop n 0 '())
    let empty = m.constant(Constant::EmptyList);
    let mut churn = FunctionBuilder::new(Some("churn"), Arity::exactly(1), 6, 0);
    churn
        .emit(I::GetVariable {
            dst: 2,
            variable: loop_var,
        })
        .emit(I::Mov { dst: 3, src: 1 })
        .emit(I::LoadInt { dst: 4, value: 0 })
        .emit(I::LoadConstant {
            dst: 5,
            constant: empty,
        })
        .emit(I::TailCall { proc: 2, argc: 3 });
    vec![
        ("churn-loop", looping),
        ("churn", m.function(churn.finish().unwrap())),
    ]
}

/// `(define (iota n acc) (if (= n 0) acc (iota (- n 1) (cons n acc))))` and
/// `(define (sum xs acc) (if (null? xs) acc (sum (cdr xs) (+ acc (car xs)))))`.
fn lists(m: &mut ModuleBuilder) -> Vec<(&'static str, u32)> {
    let iota_var = m.variable("iota");
    let sum_var = m.variable("sum");
    let mut iota = FunctionBuilder::new(Some("iota"), Arity::exactly(2), 7, 0);
    let more = iota.label();
    iota.emit(I::LoadInt { dst: 3, value: 0 });
    iota.branch(
        I::IfNumEq {
            a: 1,
            b: 3,
            offset: 0,
        },
        more,
    );
    iota.emit(I::Return { src: 2 });
    iota.bind(more);
    iota.emit(I::GetVariable {
        dst: 4,
        variable: iota_var,
    })
    .emit(I::SubImm {
        dst: 5,
        a: 1,
        imm: 1,
    })
    .emit(I::Cons {
        dst: 6,
        car: 1,
        cdr: 2,
    })
    .emit(I::TailCall { proc: 4, argc: 2 });

    let mut sum = FunctionBuilder::new(Some("sum"), Arity::exactly(2), 7, 0);
    let more = sum.label();
    sum.branch(I::IfNull { a: 1, offset: 0 }, more);
    sum.emit(I::Return { src: 2 });
    sum.bind(more);
    sum.emit(I::GetVariable {
        dst: 4,
        variable: sum_var,
    })
    .emit(I::Cdr { dst: 5, pair: 1 })
    .emit(I::Car { dst: 6, pair: 1 })
    .emit(I::Add { dst: 6, a: 2, b: 6 })
    .emit(I::TailCall { proc: 4, argc: 2 });
    vec![
        ("iota", m.function(iota.finish().unwrap())),
        ("sum", m.function(sum.finish().unwrap())),
    ]
}

fn length(cx: &mut Cx<NoCollection>, args: &[Value]) -> Result<Value, Error> {
    let mut n = 0;
    let mut list = args[0];
    while list.is_pair() {
        n += 1;
        // SAFETY: `list` is a live pair.
        list = unsafe { cx.cdr(list) };
    }
    Ok(Value::fixnum(n).unwrap())
}

fn remainder(_cx: &mut Cx<NoCollection>, args: &[Value]) -> Result<Value, Error> {
    match (args[0].as_fixnum(), args[1].as_fixnum()) {
        (Some(a), Some(b)) if b != 0 => Ok(Value::fixnum(a % b).unwrap()),
        _ => Err(Error::scheme(
            "remainder: expected integers, the second non-zero",
        )),
    }
}

/// Runs `run` once to warm up, then five times, and prints the best time
/// and the time per Scheme call.
fn time(
    cx: &mut Cx<NoCollection>,
    label: &str,
    expected: i64,
    mut run: impl FnMut(&mut Cx<NoCollection>) -> Option<i64>,
) {
    let before = cx.fuel_used();
    assert_eq!(run(cx), Some(expected), "{label}");
    let calls = (cx.fuel_used() - before) as f64;
    let mut best = f64::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        let result = run(cx);
        best = best.min(start.elapsed().as_secs_f64());
        assert_eq!(result, Some(expected), "{label}");
    }
    println!(
        "{label:20} {:8.2} ms  {:10} calls  {:5.1} ns per call",
        best * 1e3,
        calls,
        best * 1e9 / calls
    );
}

fn main() {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    // SAFETY: both return only fixnums and errors, and keep nothing.
    unsafe { cx.define_native("length", Arity::exactly(1), length) }.unwrap();
    unsafe { cx.define_native("remainder", Arity::exactly(2), remainder) }.unwrap();
    let n = |n: i64| Handle::fixnum(n).unwrap();

    let module = load(&mut cx, "fib", fib);
    let fib = cx.variable(module, "fib").unwrap();
    time(&mut cx, "fib 30", 832_040, |cx| {
        cx.call(&fib, &[n(30)]).unwrap().as_fixnum()
    });

    let module = load(&mut cx, "tak", tak);
    let tak = cx.variable(module, "tak").unwrap();
    time(&mut cx, "tak 24 16 8", 9, |cx| {
        cx.call(&tak, &[n(24), n(16), n(8)]).unwrap().as_fixnum()
    });

    let module = load(&mut cx, "churn", churn);
    let churn = cx.variable(module, "churn").unwrap();
    time(&mut cx, "churn 1,000,000", 1000, |cx| {
        cx.call(&churn, &[n(1_000_000)]).unwrap().as_fixnum()
    });

    let module = load(&mut cx, "lists", lists);
    let iota = cx.variable(module, "iota").unwrap();
    let sum = cx.variable(module, "sum").unwrap();
    let list = cx
        .call(&iota, &[n(1_000_000), Handle::empty_list()])
        .unwrap();
    time(&mut cx, "sum 1,000,000", 500_000_500_000, |cx| {
        cx.call(&sum, &[list.clone(), n(0)]).unwrap().as_fixnum()
    });
    time(&mut cx, "build 1,000,000", 1, |cx| {
        let list = cx
            .call(&iota, &[n(1_000_000), Handle::empty_list()])
            .unwrap();
        Some(i64::from(list.is_pair()))
    });
    println!("heap: {} MiB", cx.bytes_used() / (1024 * 1024));
}
