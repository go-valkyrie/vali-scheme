//! Precise counting runs code the ownership pass rewrote as it loaded
//! (`vali_bytecode::ownership`): the same results, backtraces and module as
//! under any other strategy, and nothing left counted.

use vali_bytecode::{Arity, Constant, FunctionBuilder, Instruction as I, Module, ModuleBuilder};
use vali_runtime::{
    Cx, DeferredHeadered, Error, Frame, HeapConfig, NoCollection, PreciseHeadered,
    PreciseSideCounts, Strategy,
};

/// A module whose body binds `name` to a closure of `function`.
fn module_defining(
    name: &str,
    build: impl FnOnce(&mut ModuleBuilder) -> FunctionBuilder,
) -> Module {
    let mut m = ModuleBuilder::new("ownership");
    let f = build(&mut m);
    let function = m.function(f.finish().unwrap());
    let variable = m.variable(name);
    let unspecified = m.constant(Constant::Unspecified);
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 2, 0);
    body.emit(I::Closure {
        dst: 1,
        function,
        free: vec![],
    })
    .emit(I::SetVariable { src: 1, variable })
    .emit(I::LoadConstant {
        dst: 1,
        constant: unspecified,
    })
    .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    m.finish(body)
}

/// `(lambda (n) ...)`: builds the list 1 to `n` and sums it, in two loops,
/// with its values in slots past 255, results written over their own
/// operands (`cons r300, r1, r300`, `box r300, r300`, `cdr r300, r300`),
/// and a copy of a box that is never used.
fn sum_of_list(m: &mut ModuleBuilder) -> FunctionBuilder {
    let empty = m.constant(Constant::EmptyList);
    let mut f = FunctionBuilder::new(Some("sum-of-list"), Arity::exactly(1), 301, 0);
    let (build, body, sum, add, done) = (f.label(), f.label(), f.label(), f.label(), f.label());
    f.emit(I::LoadConstant {
        dst: 300,
        constant: empty,
    });
    f.bind(build);
    // Continue if n < 1; otherwise build on.
    f.branch(
        I::IfLtImm {
            a: 1,
            imm: 1,
            offset: 0,
        },
        body,
    );
    f.jump(sum);
    f.bind(body);
    f.emit(I::Cons {
        dst: 300,
        car: 1,
        cdr: 300,
    })
    .emit(I::MakeBox { dst: 300, src: 300 })
    // A copy never used: written over on the next round, and dropped
    // before the return after the last.
    .emit(I::Mov { dst: 299, src: 300 })
    .emit(I::Unbox { dst: 300, src: 300 })
    .emit(I::SubImm {
        dst: 1,
        a: 1,
        imm: 1,
    });
    f.jump(build);
    f.bind(sum);
    f.emit(I::LoadInt { dst: 2, value: 0 });
    f.bind(add);
    f.branch(I::IfPair { a: 300, offset: 0 }, done);
    f.emit(I::Car { dst: 3, pair: 300 })
        .emit(I::Add { dst: 2, a: 2, b: 3 })
        .emit(I::Cdr {
            dst: 300,
            pair: 300,
        });
    f.jump(add);
    f.bind(done);
    f.emit(I::Return { src: 2 });
    f
}

fn sum_under<S: Strategy>(n: i64) -> Option<i64> {
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    let id = cx.load(module_defining("sum", sum_of_list)).unwrap();
    cx.run(id).unwrap();
    let sum = cx.variable(id, "sum").unwrap();
    cx.call(&sum, &[vali_runtime::Handle::fixnum(n).unwrap()])
        .unwrap()
        .as_fixnum()
}

#[test]
fn hand_built_code_runs_alike_under_every_strategy() {
    let expected = Some(1000 * 1001 / 2);
    assert_eq!(sum_under::<NoCollection>(1000), expected);
    assert_eq!(sum_under::<DeferredHeadered>(1000), expected);
    assert_eq!(sum_under::<PreciseHeadered>(1000), expected);
    assert_eq!(sum_under::<PreciseSideCounts>(1000), expected);
}

#[test]
fn hand_built_code_leaves_nothing_counted() {
    let mut cx = Cx::<PreciseHeadered>::new(&HeapConfig::default());
    let id = cx.load(module_defining("sum", sum_of_list)).unwrap();
    cx.run(id).unwrap();
    let sum = cx.variable(id, "sum").unwrap();
    let mut live = || {
        cx.call(&sum, &[vali_runtime::Handle::fixnum(500).unwrap()])
            .unwrap();
        cx.collect_garbage();
        cx.heap().bytes_live()
    };
    let first = live();
    assert_eq!(live(), first);
    assert_eq!(live(), first);
}

#[test]
fn the_loaded_module_is_the_one_given() {
    let mut cx = Cx::<PreciseHeadered>::new(&HeapConfig::default());
    let module = module_defining("sum", sum_of_list);
    let id = cx.load_library(module.clone()).unwrap();
    assert!(cx.is_loaded_from(id, &module));
    // A module loaded other than as a library has no digest.
    let id = cx.load(module.clone()).unwrap();
    assert!(!cx.is_loaded_from(id, &module));
}

/// The loaded module keeps what runs, not what it was given, so whether
/// another is the one loaded is answered by digest: a module that differs
/// anywhere, in code the rewrite changes or in a constant built into the
/// pool, is not it, under every strategy.
#[test]
fn a_loaded_module_is_told_from_one_that_differs() {
    fn check<S: Strategy>() {
        let mut cx = Cx::<S>::new(&HeapConfig::default());
        let module = module_defining("sum", sum_of_list);
        let id = cx.load_library(module.clone()).unwrap();
        assert!(cx.is_loaded_from(id, &module));
        let mut other = module.clone();
        other.functions[0].code[0] ^= 1;
        assert!(!cx.is_loaded_from(id, &other));
        let mut other = module.clone();
        other.constants.push(Constant::Integer(7));
        assert!(!cx.is_loaded_from(id, &other));
        // It still runs, and still names its variables, with the trees and
        // the code it was given gone.
        cx.run(id).unwrap();
        assert!(cx.variable(id, "sum").is_some());
    }
    check::<NoCollection>();
    check::<DeferredHeadered>();
    check::<PreciseHeadered>();
    check::<PreciseSideCounts>();
}

#[test]
fn a_module_cannot_hold_ownership_operations() {
    for instruction in [I::Drop { slot: 1 }, I::Move { dst: 1, src: 1 }] {
        let mut m = ModuleBuilder::new("owned");
        let mut body = FunctionBuilder::new(None, Arity::exactly(0), 2, 0);
        body.emit(I::LoadInt { dst: 1, value: 1 })
            .emit(instruction)
            .emit(I::Return { src: 1 });
        let body = m.function(body.finish().unwrap());
        let mut cx = Cx::<PreciseHeadered>::new(&HeapConfig::default());
        let Error::Scheme(e) = cx.load(m.finish(body)).unwrap_err() else {
            panic!("expected a refusal");
        };
        assert!(
            e.message.contains("inserted by the runtime"),
            "{}",
            e.message
        );
    }
}

/// `(lambda () (car (car '(5 . 6))))` over three lines: the failing `car`
/// comes after drops the pass placed, yet keeps its line.
fn failing(m: &mut ModuleBuilder) -> FunctionBuilder {
    let source = m.source("test.scm");
    let pair = m.constant(Constant::Pair(
        Box::new(Constant::Integer(5)),
        Box::new(Constant::Integer(6)),
    ));
    let other = m.constant(Constant::Pair(
        Box::new(Constant::Integer(7)),
        Box::new(Constant::Integer(8)),
    ));
    let mut f = FunctionBuilder::new(Some("failing"), Arity::exactly(0), 6, 0);
    f.emit(I::LoadConstant {
        dst: 1,
        constant: pair,
    })
    .emit(I::LoadConstant {
        dst: 2,
        constant: other,
    });
    f.position(source, 1, 0).emit(I::Car { dst: 3, pair: 1 });
    f.position(source, 2, 0).emit(I::Car { dst: 4, pair: 2 });
    f.position(source, 3, 0).emit(I::Car { dst: 5, pair: 3 });
    f.emit(I::Return { src: 5 });
    f
}

fn backtrace_under<S: Strategy>() -> Vec<Frame> {
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    let id = cx.load(module_defining("failing", failing)).unwrap();
    cx.run(id).unwrap();
    let failing = cx.variable(id, "failing").unwrap();
    let Error::Scheme(e) = cx.call(&failing, &[]).unwrap_err() else {
        panic!("expected a Scheme error");
    };
    e.backtrace
}

#[test]
fn backtraces_are_the_same_under_precise_counting() {
    let expected = backtrace_under::<NoCollection>();
    assert_eq!(expected[0].position.as_ref().map(|p| p.line), Some(3));
    let lines = |frames: &[Frame]| {
        frames
            .iter()
            .map(|f| {
                (
                    f.name.clone(),
                    f.position.as_ref().map(|p| (p.line, p.column)),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        lines(&backtrace_under::<PreciseHeadered>()),
        lines(&expected)
    );
}
