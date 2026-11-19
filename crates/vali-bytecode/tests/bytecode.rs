//! Encoding, building, validating and disassembling bytecode.

use std::collections::HashSet;

use vali_bytecode::{
    Arity, Constant, DecodeError, FunctionBuilder, Inlined, Instruction as I, JUMP_RANGE, Module,
    ModuleBuilder, Opcode, disassemble, validate,
};

/// At least one example of every instruction, with awkward operands.
fn examples() -> Vec<I> {
    vec![
        I::Mov { dst: 1, src: 255 },
        I::LoadConstant {
            dst: 2,
            constant: 65535,
        },
        I::LoadInt {
            dst: 3,
            value: -32768,
        },
        I::LoadInt {
            dst: 3,
            value: 32767,
        },
        I::GetVariable {
            dst: 4,
            variable: 513,
        },
        I::SetVariable {
            src: 5,
            variable: 0,
        },
        I::Closure {
            dst: 6,
            function: 70_000,
            free: vec![],
        },
        I::Closure {
            dst: 6,
            function: 1,
            free: vec![1, 2, 3, 4, 5],
        },
        I::Free { dst: 7, index: 9 },
        I::MakeBox { dst: 8, src: 9 },
        I::Unbox { dst: 10, src: 11 },
        I::SetBox { cell: 12, src: 13 },
        I::Jump {
            offset: *JUMP_RANGE.start(),
        },
        I::Jump {
            offset: *JUMP_RANGE.end(),
        },
        I::If {
            test: 14,
            offset: i32::MIN,
        },
        I::IfEq {
            a: 1,
            b: 2,
            offset: -3,
        },
        I::IfLt {
            a: 1,
            b: 2,
            offset: 4,
        },
        I::IfLe {
            a: 1,
            b: 2,
            offset: 5,
        },
        I::IfNumEq {
            a: 1,
            b: 2,
            offset: i32::MAX,
        },
        I::IfLtImm {
            a: 1,
            imm: -128,
            offset: 6,
        },
        I::IfNull { a: 3, offset: 7 },
        I::IfPair { a: 3, offset: -8 },
        I::Call { base: 2, argc: 253 },
        I::TailCall { proc: 0, argc: 255 },
        I::CallDirect {
            base: 2,
            argc: 3,
            function: 7,
        },
        I::TailCallDirect {
            proc: 0,
            argc: 2,
            function: 70_000,
        },
        I::Return { src: 200 },
        I::Add { dst: 1, a: 2, b: 3 },
        I::Sub { dst: 1, a: 2, b: 3 },
        I::Mul { dst: 1, a: 2, b: 3 },
        I::AddImm {
            dst: 1,
            a: 2,
            imm: 127,
        },
        I::SubImm {
            dst: 1,
            a: 2,
            imm: -1,
        },
        I::Lt { dst: 1, a: 2, b: 3 },
        I::Le { dst: 1, a: 2, b: 3 },
        I::NumEq { dst: 1, a: 2, b: 3 },
        I::Eq { dst: 1, a: 2, b: 3 },
        I::Not { dst: 1, src: 2 },
        I::Cons {
            dst: 1,
            car: 2,
            cdr: 3,
        },
        I::Car { dst: 1, pair: 2 },
        I::Cdr { dst: 1, pair: 2 },
        I::SetCar { pair: 1, value: 2 },
        I::SetCdr { pair: 1, value: 2 },
        I::IsNull { dst: 1, src: 2 },
        I::IsPair { dst: 1, src: 2 },
        I::MakeVector {
            dst: 1,
            len: 2,
            fill: 3,
        },
        I::VectorRef {
            dst: 1,
            vector: 2,
            index: 3,
        },
        I::VectorSet {
            vector: 1,
            index: 2,
            value: 3,
        },
        I::VectorLength { dst: 1, vector: 2 },
        I::SetFree {
            closure: 1,
            index: 200,
            src: 3,
        },
        I::Move { dst: 1, src: 2 },
        I::Drop { slot: 3 },
        // Wide: a field above 255 needs the `wide` prefix.
        I::Mov { dst: 300, src: 2 },
        I::Move { dst: 2, src: 300 },
        I::Drop { slot: 65_000 },
        I::LoadConstant {
            dst: 256,
            constant: 7,
        },
        I::IfLt {
            a: 2,
            b: 65_000,
            offset: -3,
        },
        I::Call {
            base: 500,
            argc: 300,
        },
        I::Closure {
            dst: 1,
            function: 4,
            free: vec![1, 400, 3],
        },
        I::CallDirect {
            base: 300,
            argc: 2,
            function: 1,
        },
        I::AddImm {
            dst: 1000,
            a: 2,
            imm: -128,
        },
    ]
}

#[test]
fn wide_forms_are_used_only_when_needed() {
    let mut code = Vec::new();
    I::Mov { dst: 255, src: 2 }.encode(&mut code);
    assert_eq!(code.len(), 1);
    I::Mov { dst: 256, src: 2 }.encode(&mut code);
    assert_eq!(code.len(), 3);
    assert_eq!(code[1] as u8, Opcode::Wide as u8);
    // A prefix that is not needed, or that widens an immediate, is refused.
    let unneeded = [Opcode::Wide as u32, code[0]];
    assert!(I::decode(&unneeded, 0).is_err());
    let mut imm = Vec::new();
    I::AddImm {
        dst: 1,
        a: 2,
        imm: 5,
    }
    .encode(&mut imm);
    let widened = [Opcode::Wide as u32 | 1 << 24, imm[0]];
    assert!(I::decode(&widened, 0).is_err());
    // Nor may one prefix another.
    let doubled = [Opcode::Wide as u32 | 1 << 8, code[1], code[2]];
    assert!(I::decode(&doubled, 0).is_err());
}

#[test]
fn every_instruction_round_trips() {
    let examples = examples();
    let covered: HashSet<Opcode> = examples.iter().map(I::opcode).collect();
    for &op in Opcode::ALL {
        // `wide` is a prefix: the wide examples cover it.
        assert!(
            covered.contains(&op) || op == Opcode::Wide,
            "no example of {op:?}"
        );
        assert_eq!(Opcode::from_u8(op as u8), Some(op));
    }
    let mut code = Vec::new();
    let mut starts = Vec::new();
    for instruction in &examples {
        starts.push(code.len());
        instruction.encode(&mut code);
    }
    for (i, instruction) in examples.iter().enumerate() {
        let (decoded, len) = I::decode(&code, starts[i]).unwrap();
        assert_eq!(&decoded, instruction);
        let end = starts.get(i + 1).copied().unwrap_or(code.len());
        assert_eq!(len, end - starts[i], "{instruction:?}");
    }
}

#[test]
fn decoding_errors() {
    assert_eq!(I::decode(&[0], 0), Err(DecodeError::UnknownOpcode(0)));
    assert_eq!(I::decode(&[255], 0), Err(DecodeError::UnknownOpcode(255)));
    assert_eq!(I::decode(&[], 0), Err(DecodeError::Truncated));
    let mut code = Vec::new();
    I::IfLt {
        a: 1,
        b: 2,
        offset: 0,
    }
    .encode(&mut code);
    assert_eq!(I::decode(&code[..1], 0), Err(DecodeError::Truncated));
}

/// `(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))`, by
/// hand: each call reserves three slots for the callee's frame header, then
/// the procedure, then the argument.
fn fib_module() -> Module {
    let mut m = ModuleBuilder::new("fib");
    let fib_var = m.variable("fib");
    let fib = m.declare();

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
    m.define(fib, f.finish().unwrap());

    let mut body = FunctionBuilder::new(Some("body"), Arity::exactly(0), 2, 0);
    let unspecified = m.constant(Constant::Unspecified);
    body.emit(I::Closure {
        dst: 1,
        function: fib,
        free: vec![],
    })
    .emit(I::SetVariable {
        src: 1,
        variable: fib_var,
    })
    .emit(I::LoadConstant {
        dst: 1,
        constant: unspecified,
    })
    .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    m.finish(body)
}

#[test]
fn fib_builds_validates_and_disassembles() {
    let module = fib_module();
    validate(&module).unwrap();
    let listing = disassemble(&module.functions[0]);
    let expected = "\
fib: arity 1, 11 slots, 0 free
  0000  if-lt-imm r1, 2, +1  ; -> 0003
  0002  return r1
  0003  get-variable r5, v0
  0004  sub-imm r6, r1, 1
  0005  call r2, 1
  0006  get-variable r9, v0
  0007  sub-imm r10, r1, 2
  0008  call r6, 1
  0009  add r1, r5, r9
  0010  return r1
";
    assert_eq!(listing, expected);
}

#[test]
fn labels_resolve_backward_and_forward() {
    let mut f = FunctionBuilder::new(None, Arity::exactly(1), 3, 0);
    let top = f.label();
    let done = f.label();
    f.bind(top);
    f.branch(I::IfPair { a: 1, offset: 0 }, done);
    f.emit(I::Cdr { dst: 1, pair: 1 });
    f.jump(top);
    f.bind(done);
    f.emit(I::Return { src: 1 });
    let function = f.finish().unwrap();
    let (jump, _) = I::decode(&function.code, 3).unwrap();
    assert_eq!(jump, I::Jump { offset: -4 });
    let (branch, _) = I::decode(&function.code, 0).unwrap();
    assert_eq!(branch, I::IfPair { a: 1, offset: 2 });

    let mut g = FunctionBuilder::new(None, Arity::exactly(0), 1, 0);
    let nowhere = g.label();
    g.jump(nowhere);
    assert!(g.finish().is_err());
}

/// A module whose body is one function with `code`, `slots` slots.
fn module_with(slots: u16, build: impl FnOnce(&mut ModuleBuilder, &mut FunctionBuilder)) -> Module {
    let mut m = ModuleBuilder::new("test");
    let mut f = FunctionBuilder::new(None, Arity::exactly(0), slots, 0);
    build(&mut m, &mut f);
    let body = m.function(f.finish().unwrap());
    m.finish(body)
}

fn rejected(module: &Module) -> String {
    validate(module).unwrap_err().to_string()
}

#[test]
fn the_validator_rejects_out_of_range_operands() {
    let bad_slot = module_with(2, |_, f| {
        f.emit(I::Mov { dst: 2, src: 0 }).emit(I::Return { src: 0 });
    });
    assert!(rejected(&bad_slot).contains("outside the 2-slot frame"));

    let bad_call = module_with(4, |_, f| {
        f.emit(I::Call { base: 1, argc: 1 })
            .emit(I::Return { src: 0 });
    });
    assert!(rejected(&bad_call).contains("outside"));

    let bad_constant = module_with(2, |_, f| {
        f.emit(I::LoadConstant {
            dst: 1,
            constant: 0,
        })
        .emit(I::Return { src: 1 });
    });
    assert!(rejected(&bad_constant).contains("constant 0 does not exist"));

    let bad_variable = module_with(2, |_, f| {
        f.emit(I::GetVariable {
            dst: 1,
            variable: 3,
        })
        .emit(I::Return { src: 1 });
    });
    assert!(rejected(&bad_variable).contains("variable 3 does not exist"));

    let bad_function = module_with(2, |_, f| {
        f.emit(I::Closure {
            dst: 1,
            function: 9,
            free: vec![],
        })
        .emit(I::Return { src: 1 });
    });
    assert!(rejected(&bad_function).contains("function 9 does not exist"));

    let bad_free = module_with(2, |_, f| {
        f.emit(I::Free { dst: 1, index: 0 })
            .emit(I::Return { src: 1 });
    });
    assert!(rejected(&bad_free).contains("free variable 0 does not exist"));
}

/// Direct calls name a function with no free variables that takes their
/// arguments, and inlined copies' scopes name copies that exist, each
/// called from an earlier one.
#[test]
fn the_validator_checks_direct_calls_and_inlined_copies() {
    let direct = |argc, free| {
        let mut m = ModuleBuilder::new("test");
        let mut f = FunctionBuilder::new(None, Arity::exactly(0), 8, 0);
        f.emit(I::CallDirect {
            base: 1,
            argc,
            function: 1,
        })
        .emit(I::Return { src: 1 });
        let body = m.function(f.finish().unwrap());
        let mut g = FunctionBuilder::new(Some("g"), Arity::exactly(1), 2, free);
        g.emit(I::Return { src: 1 });
        m.function(g.finish().unwrap());
        m.finish(body)
    };
    validate(&direct(1, 0)).unwrap();
    assert!(rejected(&direct(2, 0)).contains("does not take 2 arguments"));
    assert!(rejected(&direct(1, 1)).contains("has free variables"));

    let scoped = |caller, scope| {
        module_with(2, |_, f| {
            f.inline(Inlined {
                name: Some("g".into()),
                call: None,
                caller,
            })
            .scope(scope)
            .emit(I::Return { src: 0 });
        })
    };
    validate(&scoped(0, 1)).unwrap();
    assert!(rejected(&scoped(0, 2)).contains("scopes must be"));
    assert!(rejected(&scoped(1, 1)).contains("later caller"));
}

/// Slot 0 holds the running closure, which the VM reads back for the code
/// it runs: nothing may write it, and no call's header may cover it.
#[test]
fn the_validator_keeps_slot_0() {
    let write = module_with(2, |_, f| {
        f.emit(I::LoadInt { dst: 0, value: 1 })
            .emit(I::Return { src: 0 });
    });
    assert!(rejected(&write).contains("slot 0"));
    let call = module_with(8, |_, f| {
        f.emit(I::Call { base: 0, argc: 1 })
            .emit(I::Return { src: 3 });
    });
    assert!(rejected(&call).contains("slot 0"));
}

/// Every instruction that writes a slot is rejected when that slot is 0.
#[test]
fn the_validator_rejects_each_write_to_slot_0() {
    let writes = [
        I::Mov { dst: 0, src: 1 },
        I::MakeBox { dst: 0, src: 1 },
        I::Unbox { dst: 0, src: 1 },
        I::Not { dst: 0, src: 1 },
        I::IsNull { dst: 0, src: 1 },
        I::IsPair { dst: 0, src: 1 },
        I::Car { dst: 0, pair: 1 },
        I::Cdr { dst: 0, pair: 1 },
        I::VectorLength { dst: 0, vector: 1 },
        I::LoadInt { dst: 0, value: 1 },
        I::LoadConstant {
            dst: 0,
            constant: 0,
        },
        I::GetVariable {
            dst: 0,
            variable: 0,
        },
        I::Closure {
            dst: 0,
            function: 0,
            // The function holding the instruction has one free variable.
            free: vec![1],
        },
        I::Free { dst: 0, index: 0 },
        I::Add { dst: 0, a: 1, b: 1 },
        I::Sub { dst: 0, a: 1, b: 1 },
        I::Mul { dst: 0, a: 1, b: 1 },
        I::Lt { dst: 0, a: 1, b: 1 },
        I::Le { dst: 0, a: 1, b: 1 },
        I::NumEq { dst: 0, a: 1, b: 1 },
        I::Eq { dst: 0, a: 1, b: 1 },
        I::Cons {
            dst: 0,
            car: 1,
            cdr: 1,
        },
        I::MakeVector {
            dst: 0,
            len: 1,
            fill: 1,
        },
        I::VectorRef {
            dst: 0,
            vector: 1,
            index: 1,
        },
        I::AddImm {
            dst: 0,
            a: 1,
            imm: 1,
        },
        I::SubImm {
            dst: 0,
            a: 1,
            imm: 1,
        },
    ];
    for write in writes {
        let mut m = ModuleBuilder::new("slot0");
        m.constant(Constant::Integer(1));
        m.variable("x");
        let mut f = FunctionBuilder::new(None, Arity::exactly(0), 2, 1);
        f.emit(write.clone()).emit(I::Return { src: 1 });
        m.function(f.finish().unwrap());
        let mut body = FunctionBuilder::new(None, Arity::exactly(0), 1, 0);
        body.emit(I::Return { src: 0 });
        let body = m.function(body.finish().unwrap());
        let module = m.finish(body);
        let error = validate(&module).expect_err(&format!("{write:?} was accepted"));
        let message = error.to_string();
        assert!(message.contains("slot 0"), "{write:?}: {message}");
    }
}

/// Only the runtime places ownership operations: a module may not.
#[test]
fn the_validator_rejects_ownership_operations() {
    for instruction in [I::Move { dst: 1, src: 2 }, I::Drop { slot: 1 }] {
        assert!(instruction.opcode().is_internal());
        let module = module_with(3, |_, f| {
            f.emit(I::LoadInt { dst: 2, value: 1 })
                .emit(instruction.clone())
                .emit(I::Return { src: 1 });
        });
        let message = rejected(&module);
        assert!(message.contains("inserted by the runtime"), "{message}");
    }
    assert!(!Opcode::Mov.is_internal());
}

#[test]
fn the_validator_rejects_bad_control_flow() {
    let mut runs_off = module_with(2, |_, f| {
        f.emit(I::Return { src: 0 });
    });
    runs_off.functions[0].code.clear();
    I::Mov { dst: 1, src: 0 }.encode(&mut runs_off.functions[0].code);
    assert!(rejected(&runs_off).contains("run off the end"));

    let mut mid_instruction = module_with(3, |_, f| {
        f.emit(I::Return { src: 0 });
    });
    let code = &mut mid_instruction.functions[0].code;
    code.clear();
    I::IfLt {
        a: 1,
        b: 2,
        offset: -1,
    }
    .encode(code);
    I::Return { src: 0 }.encode(code);
    assert!(rejected(&mid_instruction).contains("branch target"));

    let mut garbage = module_with(2, |_, f| {
        f.emit(I::Return { src: 0 });
    });
    garbage.functions[0].code.insert(0, 0xFF);
    assert!(rejected(&garbage).contains("unknown opcode"));
}

#[test]
fn the_validator_checks_functions_and_closures() {
    let mut m = ModuleBuilder::new("closures");
    let mut inner = FunctionBuilder::new(None, Arity::exactly(0), 2, 2);
    inner
        .emit(I::Free { dst: 1, index: 1 })
        .emit(I::Return { src: 1 });
    let inner = m.function(inner.finish().unwrap());
    let mut body = FunctionBuilder::new(None, Arity::exactly(0), 2, 0);
    body.emit(I::Closure {
        dst: 1,
        function: inner,
        free: vec![1],
    })
    .emit(I::Return { src: 1 });
    let body = m.function(body.finish().unwrap());
    let module = m.finish(body);
    assert!(rejected(&module).contains("has 2 free variables, not 1"));

    let mut too_small = module_with(1, |_, f| {
        f.emit(I::Return { src: 0 });
    });
    too_small.functions[0].arity = Arity::exactly(1);
    assert!(rejected(&too_small).contains("body takes no arguments"));

    let mut m = ModuleBuilder::new("params");
    let mut f = FunctionBuilder::new(
        None,
        Arity {
            required: 1,
            optional: 1,
            rest: true,
        },
        3,
        0,
    );
    f.emit(I::Return { src: 0 });
    m.function(f.finish().unwrap());
    let mut body = FunctionBuilder::new(None, Arity::exactly(0), 1, 0);
    body.emit(I::Return { src: 0 });
    let body = m.function(body.finish().unwrap());
    assert!(rejected(&m.finish(body)).contains("cannot hold the closure and parameters"));
}

#[test]
fn arities() {
    let fixed = Arity::exactly(2);
    assert!(fixed.accepts(2) && !fixed.accepts(1) && !fixed.accepts(3));
    let optional = Arity {
        required: 1,
        optional: 2,
        rest: false,
    };
    assert!(optional.accepts(1) && optional.accepts(3) && !optional.accepts(4));
    let rest = Arity {
        required: 1,
        optional: 0,
        rest: true,
    };
    assert!(!rest.accepts(0) && rest.accepts(100));
    assert_eq!(rest.parameter_slots(), 2);
}

#[test]
fn constants_and_variables_are_shared() {
    let mut m = ModuleBuilder::new("dedupe");
    assert_eq!(m.constant(Constant::Integer(1)), 0);
    assert_eq!(m.constant(Constant::String("x".into())), 1);
    assert_eq!(m.constant(Constant::Integer(1)), 0);
    assert_eq!(m.variable("a"), 0);
    assert_eq!(m.variable("b"), 1);
    assert_eq!(m.variable("a"), 0);
}

#[test]
fn equal_pairs_and_vectors_are_not_shared() {
    // Each is a mutable object: two equal quoted lists must stay two.
    let pair = || {
        Constant::Pair(
            Box::new(Constant::Integer(1)),
            Box::new(Constant::EmptyList),
        )
    };
    let vector = || Constant::Vector(vec![Constant::Integer(1)]);
    let mut m = ModuleBuilder::new("mutable");
    assert_eq!(m.constant(pair()), 0);
    assert_eq!(m.constant(pair()), 1);
    assert_eq!(m.constant(vector()), 2);
    assert_eq!(m.constant(vector()), 3);
}

/// A finished module holds no spare capacity: evaluated code stays loaded
/// as long as its module does (#31), and `Vec` growth would leave most of
/// a small module's buffers empty.
#[test]
fn finished_modules_hold_no_spare_capacity() {
    let module = fib_module();
    assert_eq!(module.functions.capacity(), module.functions.len());
    assert_eq!(module.constants.capacity(), module.constants.len());
    assert_eq!(module.variables.capacity(), module.variables.len());
    for f in &module.functions {
        assert_eq!(f.code.capacity(), f.code.len());
        assert_eq!(f.positions.capacity(), f.positions.len());
    }
}
