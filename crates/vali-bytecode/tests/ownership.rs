//! The ownership pass: where it places `move` and `drop`, and the contract
//! its output keeps.

use std::collections::HashMap;

use vali_bytecode::ownership::{check, insert};
use vali_bytecode::{
    Arity, Function, FunctionBuilder, Instruction as I, ModuleBuilder, branch_target, instructions,
    validate, validate_rewritten,
};

/// The function `build` makes, of `arity` and `slots`, as the pass rewrites
/// it. The original must be valid.
fn rewritten(arity: u8, slots: u16, build: impl FnOnce(&mut FunctionBuilder)) -> Function {
    let mut m = ModuleBuilder::new("test");
    m.variable("g");
    m.source("test.scm");
    let mut f = FunctionBuilder::new(Some("f"), Arity::exactly(arity), slots, 0);
    build(&mut f);
    m.function(f.finish().unwrap());
    let mut body = FunctionBuilder::new(None, Arity::exactly(0), 1, 0);
    body.emit(I::Return { src: 0 });
    let body = m.function(body.finish().unwrap());
    let module = m.finish(body);
    validate(&module).unwrap();
    let mut functions = insert(&module).unwrap();
    let f = functions.swap_remove(0);
    check(&f).unwrap();
    f
}

/// One line per instruction; a branch names its target's index.
fn listing(f: &Function) -> Vec<String> {
    let decoded: Vec<(usize, I)> = instructions(&f.code).map(Result::unwrap).collect();
    let index: HashMap<usize, usize> = decoded
        .iter()
        .enumerate()
        .map(|(i, (pc, _))| (*pc, i))
        .collect();
    decoded
        .iter()
        .enumerate()
        .map(|(i, (pc, instruction))| match instruction.offset() {
            Some(offset) => {
                let end = decoded.get(i + 1).map_or(f.code.len(), |(pc, _)| *pc);
                let target = branch_target(*pc, end - pc, offset).unwrap();
                // The listing ends with the relative offset.
                let text = instruction.to_string();
                let (text, _) = text.rsplit_once(' ').unwrap();
                format!("{} -> {}", text.trim_end_matches(','), index[&target])
            }
            None => instruction.to_string(),
        })
        .collect()
}

#[test]
fn a_value_is_dropped_after_its_last_use() {
    // (lambda (x) (car x))
    let f = rewritten(1, 3, |f| {
        f.emit(I::Car { dst: 2, pair: 1 })
            .emit(I::Return { src: 2 });
    });
    assert_eq!(listing(&f), ["car r2, r1", "drop r1", "return r2"]);
}

#[test]
fn a_dead_result_is_dropped_at_once() {
    let f = rewritten(1, 3, |f| {
        f.emit(I::Car { dst: 2, pair: 1 })
            .emit(I::Return { src: 1 });
    });
    assert_eq!(listing(&f), ["car r2, r1", "drop r2", "return r1"]);
}

#[test]
fn a_copy_from_a_dying_slot_becomes_a_move() {
    let f = rewritten(1, 3, |f| {
        f.emit(I::Mov { dst: 2, src: 1 }).emit(I::Return { src: 2 });
    });
    assert_eq!(listing(&f), ["move r2, r1", "return r2"]);
    // Still live after it, the source is copied.
    let f = rewritten(1, 4, |f| {
        f.emit(I::Mov { dst: 2, src: 1 })
            .emit(I::Cons {
                dst: 3,
                car: 1,
                cdr: 2,
            })
            .emit(I::Return { src: 3 });
    });
    assert_eq!(
        listing(&f),
        [
            "mov r2, r1",
            "cons r3, r1, r2",
            "drop r1",
            "drop r2",
            "return r3"
        ]
    );
}

#[test]
fn a_value_written_over_needs_no_drop() {
    // Writing a slot counts the value it held out.
    let f = rewritten(1, 2, |f| {
        f.emit(I::Cdr { dst: 1, pair: 1 })
            .emit(I::Return { src: 1 });
    });
    assert_eq!(listing(&f), ["cdr r1, r1", "return r1"]);
}

#[test]
fn immediate_results_need_no_drop() {
    let f = rewritten(1, 4, |f| {
        f.emit(I::LoadInt { dst: 2, value: 7 })
            .emit(I::IsPair { dst: 3, src: 1 })
            .emit(I::Return { src: 1 });
    });
    assert_eq!(listing(&f), ["load-int r2, 7", "pair? r3, r1", "return r1"]);
}

#[test]
fn a_loop_drops_at_its_exit_not_on_each_round() {
    // (lambda (xs y) (let loop ((xs xs)) (if (null? xs) xs (loop (cdr xs)))))
    // with `y` unused: it is dropped once, at the return.
    let f = rewritten(2, 3, |f| {
        let head = f.label();
        let done = f.label();
        f.bind(head);
        f.branch(I::IfPair { a: 1, offset: 0 }, done)
            .emit(I::Cdr { dst: 1, pair: 1 })
            .jump(head);
        f.bind(done);
        f.emit(I::Return { src: 1 });
    });
    assert_eq!(
        listing(&f),
        [
            "if-pair r1 -> 3",
            "cdr r1, r1",
            "jump -> 0",
            "drop r2",
            "return r1"
        ]
    );
}

#[test]
fn a_value_live_on_one_branch_is_dropped_on_the_other() {
    // (lambda (x y) (if x y x))
    let f = rewritten(2, 3, |f| {
        let otherwise = f.label();
        f.branch(I::If { test: 1, offset: 0 }, otherwise)
            .emit(I::Return { src: 2 });
        f.bind(otherwise);
        f.emit(I::Return { src: 1 });
    });
    assert_eq!(
        listing(&f),
        ["if r1 -> 3", "drop r1", "return r2", "drop r2", "return r1"]
    );
}

#[test]
fn drops_at_a_join_cover_every_way_in() {
    // The return is reached by falling through and by the branch: its drop
    // runs on both.
    let f = rewritten(2, 3, |f| {
        let join = f.label();
        f.branch(I::If { test: 2, offset: 0 }, join)
            .emit(I::Car { dst: 1, pair: 1 });
        f.bind(join);
        f.emit(I::Return { src: 1 });
    });
    assert_eq!(
        listing(&f),
        ["if r2 -> 2", "car r1, r1", "drop r2", "return r1"]
    );
    let f = rewritten(2, 3, |f| {
        let next = f.label();
        f.branch(I::If { test: 2, offset: 0 }, next);
        f.bind(next);
        f.emit(I::Return { src: 1 });
    });
    assert_eq!(listing(&f), ["if r2 -> 1", "drop r2", "return r1"]);
}

#[test]
fn calls_take_their_window_and_leave_the_result() {
    // (lambda (x) (g x) x): x is live across the call, so it is copied in,
    // and the unused result dropped.
    let f = rewritten(1, 7, |f| {
        f.emit(I::GetVariable {
            dst: 5,
            variable: 0,
        })
        .emit(I::Mov { dst: 6, src: 1 })
        .emit(I::Call { base: 2, argc: 1 })
        .emit(I::Return { src: 1 });
    });
    assert_eq!(
        listing(&f),
        [
            "get-variable r5, v0",
            "mov r6, r1",
            "call r2, 1",
            "drop r5",
            "return r1"
        ]
    );
    // (lambda (x) (g x)): x's last use moves it into the window.
    let f = rewritten(1, 7, |f| {
        f.emit(I::GetVariable {
            dst: 5,
            variable: 0,
        })
        .emit(I::Mov { dst: 6, src: 1 })
        .emit(I::Call { base: 2, argc: 1 })
        .emit(I::Return { src: 5 });
    });
    assert_eq!(
        listing(&f),
        [
            "get-variable r5, v0",
            "move r6, r1",
            "call r2, 1",
            "return r5"
        ]
    );
}

#[test]
fn a_call_leaves_nothing_above_its_result() {
    // Slot 6 is read after the call, where the callee left no reference,
    // so it needs no drop.
    let f = rewritten(1, 7, |f| {
        f.emit(I::GetVariable {
            dst: 5,
            variable: 0,
        })
        .emit(I::Call { base: 2, argc: 0 })
        .emit(I::Cons {
            dst: 1,
            car: 5,
            cdr: 6,
        })
        .emit(I::Return { src: 1 });
    });
    // The unused parameter needs no drop: `cons` writes over it.
    assert_eq!(
        listing(&f),
        [
            "get-variable r5, v0",
            "call r2, 0",
            "cons r1, r5, r6",
            "drop r5",
            "return r1"
        ]
    );
}

#[test]
fn returns_and_tail_calls_pass_their_operands_on() {
    let f = rewritten(1, 2, |f| {
        f.emit(I::Return { src: 0 });
    });
    assert_eq!(listing(&f), ["drop r1", "return r0"]);
    let f = rewritten(2, 3, |f| {
        f.emit(I::TailCall { proc: 1, argc: 1 });
    });
    assert_eq!(listing(&f), ["tail-call r1, 1"]);
    // A self tail call keeps the closure in slot 0.
    let f = rewritten(2, 3, |f| {
        f.emit(I::TailCall { proc: 0, argc: 1 });
    });
    assert_eq!(listing(&f), ["drop r2", "tail-call r0, 1"]);
}

#[test]
fn wide_slots_are_handled() {
    let f = rewritten(1, 400, |f| {
        f.emit(I::Mov { dst: 300, src: 1 })
            .emit(I::Mov { dst: 350, src: 300 })
            .emit(I::Return { src: 1 });
    });
    assert_eq!(
        listing(&f),
        ["mov r300, r1", "move r350, r300", "drop r350", "return r1"]
    );
}

#[test]
fn positions_and_scopes_stay_with_their_instructions() {
    let f = rewritten(1, 3, |f| {
        f.position(0, 1, 0).emit(I::Car { dst: 2, pair: 1 });
        f.position(0, 2, 4)
            .scope(0)
            .emit(I::Cdr { dst: 1, pair: 2 });
        f.position(0, 3, 8).emit(I::Return { src: 1 });
    });
    assert_eq!(
        listing(&f),
        ["car r2, r1", "cdr r1, r2", "drop r2", "return r1"]
    );
    let lines: Vec<(u32, u32)> = f.positions.iter().map(|p| (p.pc, p.line)).collect();
    assert_eq!(lines, [(0, 1), (1, 2), (3, 3)]);
}

#[test]
fn the_check_finds_a_broken_contract() {
    let f = |code: &[I]| {
        let mut b = FunctionBuilder::new(None, Arity::exactly(1), 6, 0);
        for i in code {
            b.emit(i.clone());
        }
        check(&b.finish().unwrap())
    };
    // Writing over a reference counts it out.
    f(&[
        I::Car { dst: 2, pair: 1 },
        I::Car { dst: 2, pair: 1 },
        I::Drop { slot: 1 },
        I::Return { src: 2 },
    ])
    .unwrap();
    // A reference in a call's header or procedure slot is the VM's to
    // count out; past them, the callee's frame goes.
    f(&[
        I::Car { dst: 4, pair: 1 },
        I::Call { base: 1, argc: 0 },
        I::Return { src: 4 },
    ])
    .unwrap();
    let past = f(&[
        I::Car { dst: 5, pair: 1 },
        I::Drop { slot: 1 },
        I::Call { base: 1, argc: 0 },
        I::Return { src: 4 },
    ]);
    assert!(past.unwrap_err().message.contains("r5"));
    // Returning with a reference left.
    let left = f(&[I::Car { dst: 2, pair: 1 }, I::Return { src: 2 }]);
    assert!(left.unwrap_err().message.contains("r1"));
    // A tail call may leave references only in what it moves, and where.
    let tail = f(&[
        I::Car { dst: 3, pair: 1 },
        I::Drop { slot: 1 },
        I::TailCall { proc: 2, argc: 0 },
    ]);
    assert!(tail.unwrap_err().message.contains("r3"));
}

#[test]
fn the_check_finds_a_slot_read_after_it_was_cleared() {
    let f = |code: &[I]| {
        let mut b = FunctionBuilder::new(None, Arity::exactly(1), 4, 0);
        for i in code {
            b.emit(i.clone());
        }
        check(&b.finish().unwrap())
    };
    // Read after a move took its value.
    let moved = f(&[
        I::Move { dst: 2, src: 1 },
        I::Car { dst: 3, pair: 1 },
        I::Drop { slot: 2 },
        I::Return { src: 3 },
    ]);
    assert!(moved.unwrap_err().message.contains("reads r1"));
    // Read after a drop, on one path of two.
    let mut b = FunctionBuilder::new(None, Arity::exactly(1), 4, 0);
    let join = b.label();
    b.branch(I::If { test: 1, offset: 0 }, join)
        .emit(I::Drop { slot: 1 });
    b.bind(join);
    b.emit(I::Return { src: 1 });
    let dropped = check(&b.finish().unwrap());
    assert!(dropped.unwrap_err().message.contains("return reads r1"));
    // Written again, a cleared slot may be read.
    f(&[
        I::Move { dst: 2, src: 1 },
        I::Car { dst: 1, pair: 2 },
        I::Drop { slot: 2 },
        I::Return { src: 1 },
    ])
    .unwrap();
}

#[test]
fn the_rewritten_code_is_validated() {
    let module = |slots: u16, code: &[I]| {
        let mut m = ModuleBuilder::new("test");
        let mut f = FunctionBuilder::new(None, Arity::exactly(0), slots, 0);
        for i in code {
            f.emit(i.clone());
        }
        let body = m.function(f.finish().unwrap());
        m.finish(body)
    };
    let original = module(3, &[I::LoadInt { dst: 1, value: 1 }, I::Return { src: 1 }]);
    validate(&original).unwrap();
    // The pass's own output passes.
    validate_rewritten(&original, &insert(&original).unwrap()).unwrap();
    let rewritten = |code: &[I]| {
        let functions = module(3, code).functions;
        validate_rewritten(&original, &functions)
            .unwrap_err()
            .to_string()
    };
    // Nothing may clear the running closure.
    assert!(rewritten(&[I::Drop { slot: 0 }, I::Return { src: 1 }]).contains("slot 0"));
    assert!(rewritten(&[I::Move { dst: 1, src: 0 }, I::Return { src: 1 }]).contains("slot 0"));
    assert!(rewritten(&[I::Move { dst: 0, src: 1 }, I::Return { src: 1 }]).contains("slot 0"));
    // Operands stay inside the frame.
    assert!(rewritten(&[I::Drop { slot: 3 }, I::Return { src: 1 }]).contains("outside"));
    // The frame stays the same.
    let bigger = module(4, &[I::LoadInt { dst: 1, value: 1 }, I::Return { src: 1 }]);
    let error = validate_rewritten(&original, &bigger.functions).unwrap_err();
    assert!(error.to_string().contains("frame"));
}
