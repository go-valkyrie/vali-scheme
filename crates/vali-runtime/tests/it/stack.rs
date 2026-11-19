//! What frames leave in the stack's words: under deferred counting no slot
//! is counted, so a returned call's values stay in its caller's slots,
//! uncounted, and an epoch that does not scan them may free what they refer
//! to.

use vali_bytecode::{Arity, FunctionBuilder, Instruction as I, Module, ModuleBuilder};
use vali_runtime::{
    Cx, DeferredHeadered, DeferredSideCounts, HeapConfig, NoCollection, PreciseHeadered,
    PreciseSideCounts, Strategy,
};

/// How many pairs `spin` and the body make: enough for several epochs.
const ROUNDS: i16 = 20_000;

/// A body that calls `leave`, a function with a wide frame that leaves a box
/// high in it and returns, then `spin`, which has a small frame and makes
/// garbage, so that an epoch runs inside it, and so does not scan the slots
/// past its frame. Back in the body, which has as many slots as `leave`'s
/// frame reached, it makes garbage in a loop of its own, and the epochs
/// there scan all its slots, those `leave` left included.
fn module() -> Module {
    let mut m = ModuleBuilder::new("stack");
    // `leave`'s frame starts at the body's slot 4.
    let mut leave = FunctionBuilder::new(Some("leave"), Arity::exactly(0), 64, 0);
    leave
        .emit(I::LoadInt { dst: 1, value: 0 })
        .emit(I::MakeBox { dst: 60, src: 1 })
        .emit(I::Return { src: 1 });
    let leave = m.function(leave.finish().unwrap());
    // `(lambda (n) ... n pairs ...)`
    let mut spin = FunctionBuilder::new(Some("spin"), Arity::exactly(1), 4, 0);
    let (top, round) = (spin.label(), spin.label());
    spin.bind(top);
    spin.branch(
        I::IfLtImm {
            a: 1,
            imm: 1,
            offset: 0,
        },
        round,
    );
    spin.emit(I::Return { src: 1 });
    spin.bind(round);
    spin.emit(I::Cons {
        dst: 2,
        car: 1,
        cdr: 1,
    })
    .emit(I::SubImm {
        dst: 1,
        a: 1,
        imm: 1,
    })
    .jump(top);
    let spin = m.function(spin.finish().unwrap());
    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 70, 0);
    body.emit(I::CallDirect {
        base: 1,
        argc: 0,
        function: leave,
    });
    // `spin` and the loop each make more than the allocation between
    // epochs, so epochs fall in both; after the first, the box is freed.
    for _ in 0..12 {
        body.emit(I::LoadInt {
            dst: 5,
            value: ROUNDS,
        })
        .emit(I::CallDirect {
            base: 1,
            argc: 1,
            function: spin,
        })
        .emit(I::LoadInt {
            dst: 8,
            value: ROUNDS,
        });
        let (top, round) = (body.label(), body.label());
        body.bind(top);
        body.branch(
            I::IfLtImm {
                a: 8,
                imm: 1,
                offset: 0,
            },
            round,
        );
        let next = body.label();
        body.jump(next);
        body.bind(round);
        body.emit(I::Cons {
            dst: 9,
            car: 8,
            cdr: 8,
        })
        .emit(I::SubImm {
            dst: 8,
            a: 8,
            imm: 1,
        })
        .jump(top);
        body.bind(next);
    }
    body.emit(I::Return { src: 8 });
    let body = m.function(body.finish().unwrap());
    m.finish(body)
}

fn runs<S: Strategy>() {
    let mut cx = Cx::<S>::new(&HeapConfig::default());
    let id = cx.load(module()).unwrap();
    cx.run(id).unwrap();
    cx.collect_garbage();
}

#[test]
fn an_epoch_does_not_retain_what_a_returned_call_left_in_a_slot() {
    runs::<NoCollection>();
    runs::<DeferredHeadered>();
    runs::<DeferredSideCounts>();
    runs::<PreciseHeadered>();
    runs::<PreciseSideCounts>();
}
