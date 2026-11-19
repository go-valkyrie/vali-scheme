//! The tier-0 VM, on hand-assembled bytecode.

use vali_bytecode::{Arity, Constant, FunctionBuilder, Instruction as I, Module, ModuleBuilder};
use vali_runtime::{Cx, Error, Handle, HeapConfig, NoCollection, Value};

fn cx() -> Cx<NoCollection> {
    Cx::new(&HeapConfig::default())
}

/// Builds a module whose body defines each `(name, function)` as a
/// variable bound to a closure of it, then returns unspecified.
fn module(
    name: &str,
    build: impl FnOnce(&mut ModuleBuilder) -> Vec<(&'static str, u32)>,
) -> Module {
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
    m.finish(body)
}

/// Loads and runs `module`, returning the procedure bound to `name`.
fn procedure(cx: &mut Cx<NoCollection>, module: Module, name: &str) -> Handle {
    let id = cx.load(module).unwrap();
    cx.run(id).unwrap();
    cx.variable(id, name).unwrap()
}

fn int(n: i64) -> Handle {
    Handle::fixnum(n).unwrap()
}

/// `(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))`
fn fib(m: &mut ModuleBuilder) -> u32 {
    let fib_var = m.variable("fib");
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
        variable: fib_var,
    })
    .emit(I::SubImm {
        dst: 6,
        a: 1,
        imm: 1,
    })
    .emit(I::Call { base: 2, argc: 1 })
    .emit(I::GetVariable {
        dst: 9,
        variable: fib_var,
    })
    .emit(I::SubImm {
        dst: 10,
        a: 1,
        imm: 2,
    })
    .emit(I::Call { base: 6, argc: 1 })
    .emit(I::Add { dst: 1, a: 5, b: 9 })
    .emit(I::Return { src: 1 });
    m.function(f.finish().unwrap())
}

#[test]
fn fib_runs() {
    let mut cx = cx();
    let fib = procedure(&mut cx, module("fib", |m| vec![("fib", fib(m))]), "fib");
    for (n, expected) in [(0, 0), (1, 1), (2, 1), (10, 55), (20, 6765), (25, 75025)] {
        let result = cx.call(&fib, &[int(n)]).unwrap();
        assert_eq!(result.as_fixnum(), Some(expected), "fib {n}");
    }
}

/// `(define (count n acc) (if (= n 0) acc (count (- n 1) (+ acc 1))))`: a
/// loop as a tail call, which must not grow the stack.
#[test]
fn tail_calls_run_in_constant_space() {
    let mut cx = cx();
    let m = module("count", |m| {
        let count = m.variable("count");
        let mut f = FunctionBuilder::new(Some("count"), Arity::exactly(2), 6, 0);
        let recur = f.label();
        f.emit(I::LoadInt { dst: 3, value: 0 });
        f.branch(
            I::IfNumEq {
                a: 1,
                b: 3,
                offset: 0,
            },
            recur,
        );
        f.emit(I::Return { src: 2 });
        f.bind(recur);
        f.emit(I::GetVariable {
            dst: 3,
            variable: count,
        })
        .emit(I::SubImm {
            dst: 4,
            a: 1,
            imm: 1,
        })
        .emit(I::AddImm {
            dst: 5,
            a: 2,
            imm: 1,
        })
        .emit(I::TailCall { proc: 3, argc: 2 });
        vec![("count", m.function(f.finish().unwrap()))]
    });
    let count = procedure(&mut cx, m, "count");
    let result = cx.call(&count, &[int(1_000_000), int(0)]).unwrap();
    assert_eq!(result.as_fixnum(), Some(1_000_000));
}

/// `(define (sum-to n) (if (= n 0) 0 (+ n (sum-to (- n 1)))))`: recursion
/// that is not a tail call, which grows the stack until it overflows.
fn sum_to(m: &mut ModuleBuilder) -> u32 {
    let sum_to = m.variable("sum-to");
    let mut f = FunctionBuilder::new(Some("sum-to"), Arity::exactly(1), 7, 0);
    let recur = f.label();
    f.emit(I::LoadInt { dst: 2, value: 0 });
    f.branch(
        I::IfNumEq {
            a: 1,
            b: 2,
            offset: 0,
        },
        recur,
    );
    f.emit(I::Return { src: 2 });
    f.bind(recur);
    f.emit(I::GetVariable {
        dst: 5,
        variable: sum_to,
    })
    .emit(I::SubImm {
        dst: 6,
        a: 1,
        imm: 1,
    })
    .emit(I::Call { base: 2, argc: 1 })
    .emit(I::Add { dst: 1, a: 1, b: 5 })
    .emit(I::Return { src: 1 });
    m.function(f.finish().unwrap())
}

#[test]
fn deep_recursion_and_overflow() {
    let mut cx = cx();
    let f = procedure(
        &mut cx,
        module("sum", |m| vec![("sum-to", sum_to(m))]),
        "sum-to",
    );
    // 100,000 frames of 10 words chain many 64 KiB segments.
    let result = cx.call(&f, &[int(100_000)]).unwrap();
    assert_eq!(result.as_fixnum(), Some(100_000 * 100_001 / 2));
    cx.set_max_stack_bytes(4 * 1024 * 1024);
    let Error::Scheme(e) = cx.call(&f, &[int(10_000_000)]).unwrap_err() else {
        panic!("expected a Scheme error");
    };
    assert!(e.message.contains("stack overflow"), "{}", e.message);
    // The VM is usable again after an error.
    assert_eq!(cx.call(&f, &[int(3)]).unwrap().as_fixnum(), Some(6));
}

/// `(define (deep n) (if (= n 0) (big) (+ 1 (deep (- n 1)))))`, where `big`
/// has a 250-slot frame and returns 7. At every depth, `deep` tail-calls
/// `big` from a different place in the segment, so sometimes `big`'s frame
/// must move to a new segment, including from a frame that was the first in
/// its segment.
#[test]
fn tail_calls_move_frames_across_segments() {
    let mut cx = cx();
    let m = module("deep", |m| {
        let deep_var = m.variable("deep");
        let big_var = m.variable("big");
        let mut big = FunctionBuilder::new(Some("big"), Arity::exactly(0), 250, 0);
        big.emit(I::LoadInt { dst: 249, value: 7 })
            .emit(I::Return { src: 249 });
        let mut deep = FunctionBuilder::new(Some("deep"), Arity::exactly(1), 8, 0);
        let recur = deep.label();
        deep.emit(I::LoadInt { dst: 2, value: 0 });
        deep.branch(
            I::IfNumEq {
                a: 1,
                b: 2,
                offset: 0,
            },
            recur,
        );
        deep.emit(I::GetVariable {
            dst: 2,
            variable: big_var,
        })
        .emit(I::TailCall { proc: 2, argc: 0 });
        deep.bind(recur);
        deep.emit(I::GetVariable {
            dst: 5,
            variable: deep_var,
        })
        .emit(I::SubImm {
            dst: 6,
            a: 1,
            imm: 1,
        })
        .emit(I::Call { base: 2, argc: 1 })
        .emit(I::AddImm {
            dst: 5,
            a: 5,
            imm: 1,
        })
        .emit(I::Return { src: 5 });
        vec![
            ("big", m.function(big.finish().unwrap())),
            ("deep", m.function(deep.finish().unwrap())),
        ]
    });
    let deep = procedure(&mut cx, m, "deep");
    for n in 0..3000 {
        let result = cx.call(&deep, &[int(n)]).unwrap();
        assert_eq!(result.as_fixnum(), Some(n + 7), "deep {n}");
    }
}

/// `(define (make-adder n) (lambda (x) (+ x n)))`, and
/// `(define (make-counter) (let ((n 0)) (lambda () (set! n (+ n 1)) n)))`.
#[test]
fn closures_and_boxes() {
    let mut cx = cx();
    let m = module("closures", |m| {
        let mut adder = FunctionBuilder::new(Some("adder"), Arity::exactly(1), 3, 1);
        adder
            .emit(I::Free { dst: 2, index: 0 })
            .emit(I::Add { dst: 2, a: 1, b: 2 })
            .emit(I::Return { src: 2 });
        let adder = m.function(adder.finish().unwrap());
        let mut make_adder = FunctionBuilder::new(Some("make-adder"), Arity::exactly(1), 3, 0);
        make_adder
            .emit(I::Closure {
                dst: 2,
                function: adder,
                free: vec![1],
            })
            .emit(I::Return { src: 2 });

        let mut counter = FunctionBuilder::new(Some("counter"), Arity::exactly(0), 3, 1);
        counter
            .emit(I::Free { dst: 1, index: 0 })
            .emit(I::Unbox { dst: 2, src: 1 })
            .emit(I::AddImm {
                dst: 2,
                a: 2,
                imm: 1,
            })
            .emit(I::SetBox { cell: 1, src: 2 })
            .emit(I::Return { src: 2 });
        let counter = m.function(counter.finish().unwrap());
        let mut make_counter = FunctionBuilder::new(Some("make-counter"), Arity::exactly(0), 3, 0);
        make_counter
            .emit(I::LoadInt { dst: 1, value: 0 })
            .emit(I::MakeBox { dst: 1, src: 1 })
            .emit(I::Closure {
                dst: 2,
                function: counter,
                free: vec![1],
            })
            .emit(I::Return { src: 2 });
        vec![
            ("make-adder", m.function(make_adder.finish().unwrap())),
            ("make-counter", m.function(make_counter.finish().unwrap())),
        ]
    });
    let id = cx.load(m).unwrap();
    cx.run(id).unwrap();
    let make_adder = cx.variable(id, "make-adder").unwrap();
    let add10 = cx.call(&make_adder, &[int(10)]).unwrap();
    let add3 = cx.call(&make_adder, &[int(3)]).unwrap();
    assert_eq!(cx.call(&add10, &[int(5)]).unwrap().as_fixnum(), Some(15));
    assert_eq!(cx.call(&add3, &[int(5)]).unwrap().as_fixnum(), Some(8));

    let make_counter = cx.variable(id, "make-counter").unwrap();
    let a = cx.call(&make_counter, &[]).unwrap();
    let b = cx.call(&make_counter, &[]).unwrap();
    for expected in 1..=3 {
        assert_eq!(cx.call(&a, &[]).unwrap().as_fixnum(), Some(expected));
    }
    assert_eq!(cx.call(&b, &[]).unwrap().as_fixnum(), Some(1));
}

/// `(define (f a #!optional b . rest) (cons a (cons b rest)))`.
#[test]
fn optional_and_rest_arguments() {
    let mut cx = cx();
    let m = module("args", |m| {
        let arity = Arity {
            required: 1,
            optional: 1,
            rest: true,
        };
        let mut f = FunctionBuilder::new(Some("f"), arity, 5, 0);
        f.emit(I::Cons {
            dst: 4,
            car: 2,
            cdr: 3,
        })
        .emit(I::Cons {
            dst: 4,
            car: 1,
            cdr: 4,
        })
        .emit(I::Return { src: 4 });
        vec![("f", m.function(f.finish().unwrap()))]
    });
    let f = procedure(&mut cx, m, "f");
    let list = |cx: &mut Cx<NoCollection>, mut h: Handle| {
        let mut out = Vec::new();
        while h.is_pair() {
            let item = h.car(cx).unwrap();
            out.push(cx.get(&item).unwrap());
            h = h.cdr(cx).unwrap();
        }
        out
    };
    let one = cx.call(&f, &[int(1)]).unwrap();
    assert_eq!(
        list(&mut cx, one),
        [Value::fixnum(1).unwrap(), Value::DEFAULT]
    );
    let four = cx.call(&f, &[int(1), int(2), int(3), int(4)]).unwrap();
    let values: Vec<i64> = list(&mut cx, four)
        .iter()
        .map(|v| v.as_fixnum().unwrap())
        .collect();
    assert_eq!(values, [1, 2, 3, 4]);
    let Error::Scheme(e) = cx.call(&f, &[]).unwrap_err() else {
        panic!("expected an arity error");
    };
    assert!(
        e.message.contains("wrong number of arguments"),
        "{}",
        e.message
    );
}

fn remainder(_cx: &mut Cx<NoCollection>, args: &[Value]) -> Result<Value, Error> {
    match (args[0].as_fixnum(), args[1].as_fixnum()) {
        (Some(_), Some(0)) => Err(Error::scheme("remainder: division by zero")),
        (Some(a), Some(b)) => Ok(Value::fixnum(a % b).unwrap()),
        _ => Err(Error::scheme("remainder: expected integers")),
    }
}

/// `(define (rem7 n) (remainder n 7))`, calling a native procedure, and
/// `(define (rem7-tail n) (remainder n 7))` as a tail call.
#[test]
fn native_procedures() {
    let mut cx = cx();
    // SAFETY: these procedures return only their arguments, immediates
    // and errors, and keep nothing.
    unsafe { cx.define_native("remainder", Arity::exactly(2), remainder) }.unwrap();
    let m = module("natives", |m| {
        let remainder = m.variable("remainder");
        let mut call = FunctionBuilder::new(Some("rem7"), Arity::exactly(1), 8, 0);
        call.emit(I::GetVariable {
            dst: 5,
            variable: remainder,
        })
        .emit(I::Mov { dst: 6, src: 1 })
        .emit(I::LoadInt { dst: 7, value: 7 })
        .emit(I::Call { base: 2, argc: 2 })
        .emit(I::AddImm {
            dst: 5,
            a: 5,
            imm: 100,
        })
        .emit(I::Return { src: 5 });
        let mut tail = FunctionBuilder::new(Some("rem7-tail"), Arity::exactly(1), 5, 0);
        tail.emit(I::Mov { dst: 3, src: 1 })
            .emit(I::GetVariable {
                dst: 2,
                variable: remainder,
            })
            .emit(I::LoadInt { dst: 4, value: 7 })
            .emit(I::TailCall { proc: 2, argc: 2 });
        vec![
            ("rem7", m.function(call.finish().unwrap())),
            ("rem7-tail", m.function(tail.finish().unwrap())),
        ]
    });
    let id = cx.load(m).unwrap();
    cx.run(id).unwrap();
    let rem7 = cx.variable(id, "rem7").unwrap();
    assert_eq!(cx.call(&rem7, &[int(23)]).unwrap().as_fixnum(), Some(102));
    let rem7_tail = cx.variable(id, "rem7-tail").unwrap();
    assert_eq!(
        cx.call(&rem7_tail, &[int(23)]).unwrap().as_fixnum(),
        Some(2)
    );
    let remainder = cx.variable(id, "remainder").unwrap();
    assert_eq!(
        cx.call(&remainder, &[int(10), int(4)]).unwrap().as_fixnum(),
        Some(2)
    );
}

/// `(define (inner x) (car x))` and `(define (outer x) (+ 1 (inner x)))`.
#[test]
fn errors_carry_a_backtrace() {
    let mut cx = cx();
    let m = module("errors", |m| {
        let inner_var = m.variable("inner");
        let mut inner = FunctionBuilder::new(Some("inner"), Arity::exactly(1), 2, 0);
        inner
            .emit(I::Car { dst: 1, pair: 1 })
            .emit(I::Return { src: 1 });
        let mut outer = FunctionBuilder::new(Some("outer"), Arity::exactly(1), 6, 0);
        outer
            .emit(I::GetVariable {
                dst: 4,
                variable: inner_var,
            })
            .emit(I::Mov { dst: 5, src: 1 })
            .emit(I::Call { base: 1, argc: 1 })
            .emit(I::AddImm {
                dst: 4,
                a: 4,
                imm: 1,
            })
            .emit(I::Return { src: 4 });
        let missing = m.variable("missing");
        let mut unbound = FunctionBuilder::new(Some("unbound"), Arity::exactly(0), 2, 0);
        unbound
            .emit(I::GetVariable {
                dst: 1,
                variable: missing,
            })
            .emit(I::Return { src: 1 });
        vec![
            ("inner", m.function(inner.finish().unwrap())),
            ("outer", m.function(outer.finish().unwrap())),
            ("unbound", m.function(unbound.finish().unwrap())),
        ]
    });
    let id = cx.load(m).unwrap();
    cx.run(id).unwrap();
    let outer = cx.variable(id, "outer").unwrap();
    let Error::Scheme(e) = cx.call(&outer, &[int(5)]).unwrap_err() else {
        panic!("expected a Scheme error");
    };
    assert_eq!(e.message, "car: expected a pair, got 5");
    let names: Vec<&str> = e.backtrace.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["inner", "outer"]);

    let unbound = cx.variable(id, "unbound").unwrap();
    let Error::Scheme(e) = cx.call(&unbound, &[]).unwrap_err() else {
        panic!("expected a Scheme error");
    };
    assert_eq!(e.message, "unbound variable: missing");

    let Error::Scheme(e) = cx.call(&int(1), &[]).unwrap_err() else {
        panic!("expected a Scheme error");
    };
    assert_eq!(e.message, "not a procedure: 1");
}

/// A module whose body builds `(1 2 3)` from a constant, a vector, and
/// a symbol, and checks them: `(let ((v (make-vector 3 'x))) ...)`.
#[test]
fn constants_pairs_and_vectors() {
    let mut cx = cx();
    let mut m = ModuleBuilder::new("data");
    let list = m.constant(Constant::Pair(
        Box::new(Constant::Integer(1)),
        Box::new(Constant::Pair(
            Box::new(Constant::Symbol("two".into())),
            Box::new(Constant::EmptyList),
        )),
    ));
    let symbol = m.constant(Constant::Symbol("two".into()));
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 8, 0);
    let fail = body.label();
    body.emit(I::LoadConstant {
        dst: 1,
        constant: list,
    })
    .emit(I::Cdr { dst: 2, pair: 1 })
    .emit(I::Car { dst: 2, pair: 2 })
    .emit(I::LoadConstant {
        dst: 3,
        constant: symbol,
    });
    // Symbols in constants are interned: the same object.
    body.branch(
        I::IfEq {
            a: 2,
            b: 3,
            offset: 0,
        },
        fail,
    );
    body.emit(I::LoadInt { dst: 4, value: 3 })
        .emit(I::MakeVector {
            dst: 5,
            len: 4,
            fill: 3,
        })
        .emit(I::LoadInt { dst: 6, value: 2 })
        .emit(I::VectorSet {
            vector: 5,
            index: 6,
            value: 1,
        })
        .emit(I::VectorRef {
            dst: 7,
            vector: 5,
            index: 6,
        })
        .emit(I::Car { dst: 7, pair: 7 })
        .emit(I::VectorLength { dst: 6, vector: 5 })
        .emit(I::Add { dst: 7, a: 7, b: 6 })
        .emit(I::Return { src: 7 });
    body.bind(fail);
    body.emit(I::LoadInt { dst: 1, value: -1 })
        .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    let id = cx.load(m.finish(body)).unwrap();
    assert_eq!(cx.run(id).unwrap().as_fixnum(), Some(4));
}

#[test]
fn flonums_mix_with_fixnums() {
    let mut cx = cx();
    let mut m = ModuleBuilder::new("floats");
    let half = m.constant(Constant::Flonum(0.5));
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 4, 0);
    body.emit(I::LoadConstant {
        dst: 1,
        constant: half,
    })
    .emit(I::LoadInt { dst: 2, value: 3 })
    .emit(I::Mul { dst: 3, a: 1, b: 2 })
    .emit(I::Add { dst: 3, a: 3, b: 2 })
    .emit(I::Return { src: 3 });
    let body = m.function(body.finish().unwrap());
    let id = cx.load(m.finish(body)).unwrap();
    let result = cx.run(id).unwrap();
    assert_eq!(result.to_f64(&mut cx), Ok(4.5));
}

#[test]
fn invalid_bytecode_is_refused() {
    let mut cx = cx();
    let mut m = ModuleBuilder::new("bad");
    let mut body = FunctionBuilder::new(None, Arity::exactly(0), 1, 0);
    body.emit(I::Return { src: 3 });
    let body = m.function(body.finish().unwrap());
    let Error::Scheme(e) = cx.load(m.finish(body)).unwrap_err() else {
        panic!("expected a refusal");
    };
    assert!(
        e.message.starts_with("invalid bytecode in bad"),
        "{}",
        e.message
    );
}

/// `(+ fixnum-max 1)` overflows into a bignum, and `(- bignum 1)` comes
/// back to a fixnum.
#[test]
fn fixnum_overflow_makes_bignums() {
    let mut cx = cx();
    let mut m = ModuleBuilder::new("overflow");
    let max = m.constant(Constant::Integer((1 << 60) - 1));
    let mut body = FunctionBuilder::new(None, Arity::exactly(0), 3, 0);
    body.emit(I::LoadConstant {
        dst: 1,
        constant: max,
    })
    .emit(I::AddImm {
        dst: 2,
        a: 1,
        imm: 1,
    })
    .emit(I::Return { src: 2 });
    let body = m.function(body.finish().unwrap());
    let id = cx.load(m.finish(body)).unwrap();
    let big = cx.run(id).unwrap();
    assert_eq!(big.as_fixnum(), None, "beyond the fixnum range");
    assert_eq!(big.to_integer(&mut cx).unwrap().to_i64(), Some(1 << 60));
}

/// `(define (square-until n limit) (if (< limit n) n (square-until (* n n) limit)))`,
/// run from 3 past 2^200: bignum multiplication and comparison, with a
/// bignum constant as the limit.
#[test]
fn bignum_arithmetic_and_comparison() {
    let mut cx = cx();
    let mut m = ModuleBuilder::new("squares");
    let square_var = m.variable("square-until");
    let mut f = FunctionBuilder::new(Some("square-until"), Arity::exactly(2), 6, 0);
    let again = f.label();
    f.branch(
        I::IfLt {
            a: 2,
            b: 1,
            offset: 0,
        },
        again,
    );
    f.emit(I::Return { src: 1 });
    f.bind(again);
    f.emit(I::GetVariable {
        dst: 3,
        variable: square_var,
    })
    .emit(I::Mul { dst: 4, a: 1, b: 1 })
    .emit(I::Mov { dst: 5, src: 2 })
    .emit(I::TailCall { proc: 3, argc: 2 });
    let f = m.function(f.finish().unwrap());
    let unspecified = m.constant(Constant::Unspecified);
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 2, 0);
    body.emit(I::Closure {
        dst: 1,
        function: f,
        free: vec![],
    })
    .emit(I::SetVariable {
        src: 1,
        variable: square_var,
    })
    .emit(I::LoadConstant {
        dst: 1,
        constant: unspecified,
    })
    .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    let id = cx.load(m.finish(body)).unwrap();
    cx.run(id).unwrap();
    let square_until = cx.variable(id, "square-until").unwrap();

    // 3, 9, 81, 6561, ...: 3^128 is the first square of squares above 2^200,
    // which is about 3^126.
    let limit = vali_num::Integer::from_limbs(false, &[0, 0, 0, 1 << 8]);
    let limit_value = {
        // 2^200, computed by the VM from the fixnum 2^50, so the limit is a
        // bignum on the heap.
        let mut m = ModuleBuilder::new("limit");
        let big = m.constant(Constant::Integer(1 << 50));
        let mut body = FunctionBuilder::new(None, Arity::exactly(0), 3, 0);
        body.emit(I::LoadConstant {
            dst: 1,
            constant: big,
        })
        .emit(I::Mul { dst: 2, a: 1, b: 1 }) // 2^100
        .emit(I::Mul { dst: 2, a: 2, b: 2 }) // 2^200
        .emit(I::Return { src: 2 });
        let body = m.function(body.finish().unwrap());
        let id = cx.load(m.finish(body)).unwrap();
        cx.run(id).unwrap()
    };
    assert_eq!(limit_value.to_integer(&mut cx).unwrap(), limit);
    let result = cx
        .call(&square_until, &[Handle::fixnum(3).unwrap(), limit_value])
        .unwrap()
        .to_integer(&mut cx)
        .unwrap();
    let mut expected = vali_num::Integer::from(3);
    for _ in 0..7 {
        expected = &expected * &expected;
    }
    assert_eq!(result, expected, "3^128");
}

/// A bignum mixed with a flonum gives a flonum.
#[test]
fn bignums_mix_with_flonums() {
    let mut cx = cx();
    let mut m = ModuleBuilder::new("mixed");
    let big = m.constant(Constant::Integer(1 << 50));
    let half = m.constant(Constant::Flonum(0.5));
    let mut body = FunctionBuilder::new(None, Arity::exactly(0), 4, 0);
    body.emit(I::LoadConstant {
        dst: 1,
        constant: big,
    })
    .emit(I::Mul { dst: 1, a: 1, b: 1 }) // 2^100, a bignum
    .emit(I::LoadConstant {
        dst: 2,
        constant: half,
    })
    .emit(I::Mul { dst: 3, a: 1, b: 2 })
    .emit(I::Return { src: 3 });
    let body = m.function(body.finish().unwrap());
    let id = cx.load(m.finish(body)).unwrap();
    let result = cx.run(id).unwrap();
    assert_eq!(result.to_f64(&mut cx), Ok(2f64.powi(99)));
}

fn seven(_cx: &mut Cx<NoCollection>, _args: &[Value]) -> Result<Value, Error> {
    Ok(Value::fixnum(777).unwrap())
}

fn one(_cx: &mut Cx<NoCollection>, _args: &[Value]) -> Result<Value, Error> {
    Ok(Value::fixnum(1).unwrap())
}

/// Asks for a tail call to its argument, then fails anyway.
fn tail_then_fail(cx: &mut Cx<NoCollection>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: the procedure is this procedure's own argument.
    unsafe { cx.tail_call(args[0], Vec::new()) };
    Err(Error::scheme("tail-then-fail: failed"))
}

fn boom(_cx: &mut Cx<NoCollection>, _args: &[Value]) -> Result<Value, Error> {
    panic!("kaboom")
}

/// A tail call asked for by a Rust procedure that then fails is dropped,
/// not made by the next Rust procedure to return.
#[test]
fn failed_procedures_leave_no_tail_call() {
    let mut cx = cx();
    // SAFETY: these procedures return only their arguments, immediates
    // and errors, and keep nothing.
    let seven = unsafe { cx.define_native("seven", Arity::exactly(0), seven) }.unwrap();
    let one = unsafe { cx.define_native("one", Arity::exactly(0), one) }.unwrap();
    let fail =
        unsafe { cx.define_native("tail-then-fail", Arity::exactly(1), tail_then_fail) }.unwrap();
    assert!(cx.call(&fail, &[seven]).is_err());
    assert_eq!(cx.call(&one, &[]).unwrap().as_fixnum(), Some(1));
}

/// A panic in a core primitive poisons the isolate: the host gets a
/// distinct error naming it, and every later call is refused.
#[test]
fn panics_in_primitives_poison_the_isolate() {
    let mut cx = cx();
    // SAFETY: these procedures return only their arguments, immediates
    // and errors, and keep nothing.
    let boom = unsafe { cx.define_native("boom", Arity::exactly(0), boom) }.unwrap();
    let one = unsafe { cx.define_native("one", Arity::exactly(0), one) }.unwrap();
    let Error::Poisoned(what) = cx.call(&boom, &[]).unwrap_err() else {
        panic!("expected a poisoned isolate");
    };
    assert!(what.contains("boom: panicked: kaboom"), "{what}");
    assert_eq!(
        cx.call(&one, &[]).err(),
        Some(Error::Poisoned(what.clone()))
    );
    assert_eq!(cx.call(&boom, &[]).err(), Some(Error::Poisoned(what)));
}

/// A vector too big for 32-bit field offsets is refused, not allocated
/// with offsets that wrap.
#[test]
fn objects_too_big_to_address_are_refused() {
    let mut cx = cx();
    // SAFETY: the fill is a valid value.
    let result = unsafe { cx.make_vector_raw(1 << 28, Value::FALSE) };
    assert!(result.is_err());
}
