//! Random bytecode, to fuzz the validator and the VM: a module whose
//! functions are random instruction words is validated, and if it is
//! valid, run under every memory strategy. Valid bytecode, however odd,
//! must never panic or crash the VM (ADR 0005 #7); it may fail with an
//! error.
//!
//! [`module`] draws from any source of random numbers: [`XorShift`] for
//! the seeded test in the normal suite, or the bytes a fuzzer feeds the
//! target in `fuzz/` ([`Bytes`]), which runs under AddressSanitizer and so
//! also finds memory errors that do not crash.

use std::panic::{AssertUnwindSafe, catch_unwind};

use vali_bytecode::{
    Arity, Constant, Function, Instruction, Link, Module, Opcode, Variable, validate,
};
use vali_runtime::{
    Cx, DeferredHeadered, DeferredSideCounts, HeapConfig, NoCollection, PreciseHeadered,
    PreciseSideCounts, Strategy,
};

/// A source of random 64-bit numbers.
pub trait Random {
    /// The next number.
    fn next(&mut self) -> u64;

    /// A number below `n`, which must not be 0.
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A small, fast, seeded generator (xorshift64*), so a failing case can be
/// run again from its seed.
#[derive(Debug)]
pub struct XorShift(u64);

impl XorShift {
    /// Starts from `seed`.
    pub fn new(seed: u64) -> XorShift {
        XorShift(seed.max(1))
    }
}

impl Random for XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Numbers read from a fuzzer's input, then zeros once it runs out.
#[derive(Debug)]
pub struct Bytes<'a>(pub &'a [u8]);

impl Random for Bytes<'_> {
    fn next(&mut self) -> u64 {
        let take = self.0.len().min(8);
        let (head, rest) = self.0.split_at(take);
        self.0 = rest;
        let mut word = [0; 8];
        word[..take].copy_from_slice(head);
        u64::from_le_bytes(word)
    }
}

/// The primitives random modules link to: the ones whose arguments come
/// straight from the call window, and a few ordinary ones.
const PRIMITIVES: &[&str] = &[
    "%catch",
    "%call/cc",
    "%continue",
    "apply",
    "car",
    "cons",
    "vector-ref",
    "+",
];

/// A random module: a few constants and variables, and one to three
/// functions, the first its body. Half the modules' functions are random
/// instruction words, which test what the validator refuses; the others
/// are random instructions with operands mostly in range and branches
/// forward to real instructions, which are often valid and so test the VM.
pub fn module(random: &mut impl Random) -> Module {
    let constants = vec![
        Constant::Integer(random.below(100) as i64),
        Constant::Boolean(random.below(2) == 0),
        Constant::EmptyList,
        Constant::String("s".into()),
        Constant::Pair(
            Box::new(Constant::Integer(1)),
            Box::new(Constant::EmptyList),
        ),
    ];
    let mut variables = vec![
        Variable {
            name: "defined".into(),
            link: Link::Defined,
        },
        Variable {
            name: "global".into(),
            link: Link::Global,
        },
    ];
    for name in PRIMITIVES {
        variables.push(Variable {
            name: (*name).into(),
            link: Link::Primitive,
        });
    }
    // Each function's shape first, so closures can match their targets.
    let count = 1 + random.below(3) as usize;
    let shapes: Vec<Shape> = (0..count)
        .map(|i| {
            let (arity, free) = if i == 0 {
                (0, 0)
            } else {
                (random.below(3) as u8, random.below(3) as u16)
            };
            Shape {
                arity,
                free,
                // One frame in four is wide enough to need `wide` forms.
                slots: u16::from(1 + arity)
                    + 4
                    + if random.below(4) == 0 {
                        250 + random.below(60) as u16
                    } else {
                        random.below(12) as u16
                    },
            }
        })
        .collect();
    let words = random.below(2) == 0;
    let functions = shapes
        .iter()
        .enumerate()
        .map(|(i, shape)| {
            let code = if words {
                random_words(random, shape.slots)
            } else {
                random_instructions(
                    random,
                    shape,
                    &shapes,
                    constants.len() as u16,
                    variables.len() as u16,
                )
            };
            Function {
                name: Some(if i == 0 { "body" } else { "f" }.into()),
                arity: Arity::exactly(shape.arity),
                slots: shape.slots,
                free: shape.free,
                code,
                positions: Vec::new(),
                inlined: Vec::new(),
                scopes: Vec::new(),
            }
        })
        .collect();
    Module {
        name: "fuzz".into(),
        constants,
        variables,
        exports: Vec::new(),
        functions,
        sources: Vec::new(),
        body: 0,
        digest: None,
    }
}

/// A function's arity, free variables and frame size.
struct Shape {
    arity: u8,
    free: u16,
    slots: u16,
}

/// Random instruction words, ending with a return.
fn random_words(random: &mut impl Random, slots: u16) -> Vec<u32> {
    let small = |n: u64| (n % (u64::from(slots) + 2)) as u32;
    let mut code = Vec::new();
    for _ in 0..random.below(24) {
        let opcode = Opcode::ALL[random.below(Opcode::ALL.len() as u64) as usize];
        let (a, b) = (small(random.next()), small(random.next()));
        let c = if random.below(2) == 0 {
            0
        } else {
            small(random.next())
        };
        code.push(opcode as u32 | a << 8 | b << 16 | c << 24);
        if opcode == Opcode::Closure {
            for _ in 0..b {
                code.push(small(random.next()));
            }
        }
    }
    let src = (random.next() % u64::from(slots)) as u32;
    code.push(Opcode::Return as u32 | src << 8);
    code
}

/// Random instructions, ending with a return: slots mostly inside the
/// frame and past slot 0, indexes mostly in range, and every branch
/// forward to the start of a later instruction.
fn random_instructions(
    random: &mut impl Random,
    shape: &Shape,
    shapes: &[Shape],
    constants: u16,
    variables: u16,
) -> Vec<u32> {
    use Instruction as I;
    let top = shape.slots as u64;
    let n = random.below(24) as usize;
    let mut instructions = Vec::with_capacity(n + 1);
    // How many instructions each branch skips.
    let mut skips = Vec::with_capacity(n + 1);
    for _ in 0..n {
        // One operand in sixteen is anything, to test the validator.
        let mut s = || {
            if random.below(16) == 0 {
                random.below(top + 2) as u16
            } else {
                (1 + random.below(top - 1)) as u16
            }
        };
        let (a, b, c) = (s(), s(), s());
        let imm = random.next() as i8;
        let offset = 0;
        let instruction = match random.below(44) {
            0 => I::Mov { dst: a, src: b },
            1 => I::LoadConstant {
                dst: a,
                constant: random.below(u64::from(constants) + 1) as u16,
            },
            2 => I::LoadInt {
                dst: a,
                value: random.next() as i16,
            },
            3 => I::GetVariable {
                dst: a,
                variable: random.below(u64::from(variables) + 1) as u16,
            },
            4 => I::SetVariable {
                src: a,
                variable: random.below(u64::from(variables)) as u16,
            },
            5 => {
                let function = random.below(shapes.len() as u64 + 1) as u32;
                let free = shapes.get(function as usize).map_or(0, |s| s.free);
                I::Closure {
                    dst: a,
                    function,
                    free: (0..free).map(|_| b).collect(),
                }
            }
            6 => I::Free {
                dst: a,
                index: random.below(u64::from(shape.free) + 1) as u16,
            },
            7 => I::MakeBox { dst: a, src: b },
            8 => I::Unbox { dst: a, src: b },
            9 => I::SetBox { cell: a, src: b },
            10 => I::Jump { offset },
            11 => I::If { test: a, offset },
            12 => I::IfEq { a, b, offset },
            13 => I::IfLt { a, b, offset },
            14 => I::IfLe { a, b, offset },
            15 => I::IfNumEq { a, b, offset },
            16 => I::IfLtImm { a, imm, offset },
            17 => I::IfNull { a, offset },
            18 => I::IfPair { a, offset },
            19 | 20 => I::Call {
                base: (1 + random.below(top / 2)) as u16,
                argc: random.below(4) as u16,
            },
            21 => I::TailCall {
                proc: a,
                argc: random.below(4) as u16,
            },
            22 => I::Return { src: a },
            23 => I::Add { dst: a, a: b, b: c },
            24 => I::Sub { dst: a, a: b, b: c },
            25 => I::Mul { dst: a, a: b, b: c },
            26 => I::AddImm { dst: a, a: b, imm },
            27 => I::SubImm { dst: a, a: b, imm },
            28 => I::Lt { dst: a, a: b, b: c },
            29 => I::Le { dst: a, a: b, b: c },
            30 => I::NumEq { dst: a, a: b, b: c },
            31 => I::Eq { dst: a, a: b, b: c },
            32 => I::Not { dst: a, src: b },
            33 => I::Cons {
                dst: a,
                car: b,
                cdr: c,
            },
            34 => I::Car { dst: a, pair: b },
            35 => I::Cdr { dst: a, pair: b },
            36 => I::SetCar { pair: a, value: b },
            37 => I::SetCdr { pair: a, value: b },
            38 => I::IsNull { dst: a, src: b },
            39 => I::IsPair { dst: a, src: b },
            40 => I::MakeVector {
                dst: a,
                len: b,
                fill: c,
            },
            41 => I::VectorRef {
                dst: a,
                vector: b,
                index: c,
            },
            42 => I::VectorSet {
                vector: a,
                index: b,
                value: c,
            },
            _ => I::SetFree {
                closure: a,
                index: random.below(3) as u16,
                src: b,
            },
        };
        skips.push(random.below(4) as usize);
        instructions.push(instruction);
    }
    instructions.push(I::Return {
        src: random.below(top) as u16,
    });
    skips.push(0);
    // Where each instruction starts, in words, then each branch's offset
    // from the next instruction to its target.
    let mut starts = Vec::with_capacity(instructions.len() + 1);
    let mut at = 0;
    for instruction in &instructions {
        starts.push(at);
        let mut words = Vec::new();
        instruction.encode(&mut words);
        at += words.len();
    }
    starts.push(at);
    let last = instructions.len() - 1;
    let mut code = Vec::new();
    for (i, mut instruction) in instructions.into_iter().enumerate() {
        let target = (i + 1 + skips[i]).min(last);
        // Only branches use it, and none is last, so it is never negative.
        let offset = starts[target] as i32 - starts[i + 1] as i32;
        match &mut instruction {
            I::Jump { offset: o }
            | I::If { offset: o, .. }
            | I::IfEq { offset: o, .. }
            | I::IfLt { offset: o, .. }
            | I::IfLe { offset: o, .. }
            | I::IfNumEq { offset: o, .. }
            | I::IfLtImm { offset: o, .. }
            | I::IfNull { offset: o, .. }
            | I::IfPair { offset: o, .. } => *o = offset,
            _ => {}
        }
        instruction.encode(&mut code);
    }
    code
}

/// Whether any of `module`'s branches goes backward, which could loop
/// forever: such a module is validated but not run, since fuel is counted
/// only as procedures are entered.
fn loops(module: &Module) -> bool {
    module.functions.iter().any(|f| {
        let mut pc = 0;
        while let Ok((instruction, len)) = Instruction::decode(&f.code, pc) {
            if instruction.offset().is_some_and(|o| o < 0) {
                return true;
            }
            pc += len;
        }
        false
    })
}

/// Validates `module`, and if it is valid, runs it under every strategy,
/// returning whether it ran. An error is a panic in the VM, which includes
/// the debug assertions that catch silent corruption (a frame whose slot 0
/// no longer holds its own closure, say), or an answer that depends on the
/// strategy: valid bytecode may fail, never panic, and no strategy may
/// change the answer.
pub fn check(module: &Module) -> Result<bool, String> {
    if validate(module).is_err() || loops(module) {
        return Ok(false);
    }
    let outcomes = [
        run::<NoCollection>(module)?,
        run::<DeferredHeadered>(module)?,
        run::<DeferredSideCounts>(module)?,
        run::<PreciseHeadered>(module)?,
        run::<PreciseSideCounts>(module)?,
    ];
    // Running out of memory is the one answer that may differ: a strategy
    // that reclaims runs further than one that never does.
    let comparable: Vec<&(&str, String)> = outcomes
        .iter()
        .filter(|(_, outcome)| !outcome.starts_with("error: out of memory"))
        .collect();
    if let Some(first) = comparable.first()
        && let Some(other) = comparable.iter().find(|o| o.1 != first.1)
    {
        return Err(format!(
            "the answer depends on the strategy: under {} it is {}, under {} it is {}",
            first.0, first.1, other.0, other.1
        ));
    }
    Ok(true)
}

/// `text` with each hexadecimal address, which differs from strategy to
/// strategy, replaced by `0x`.
fn without_addresses(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("0x") {
        out.push_str(&rest[..at + 2]);
        rest = rest[at + 2..].trim_start_matches(|c: char| c.is_ascii_hexdigit());
    }
    out.push_str(rest);
    out
}

/// Runs `module` under strategy `S` and describes how it ended: the value
/// it returned as `write` prints it, or its error.
fn run<S: Strategy>(module: &Module) -> Result<(&'static str, String), String> {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let mut cx = Cx::<S>::new(&HeapConfig {
            max_bytes: Some(64 << 20),
            ..Default::default()
        });
        cx.set_fuel_limit(Some(10_000));
        cx.set_max_stack_bytes(1 << 20);
        let mut outcome = String::from("not loaded");
        if let Ok(id) = cx.load(module.clone()) {
            outcome = match cx.run(id) {
                Ok(value) => match cx.write_string(&value) {
                    Ok(text) => format!("value: {text}"),
                    Err(e) => format!("error: {e}"),
                },
                Err(e) => format!("error: {e}"),
            };
            cx.collect_garbage();
        }
        outcome
    }));
    match outcome {
        Ok(outcome) => Ok((S::NAME, without_addresses(&outcome))),
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "a panic".into());
            Err(format!("under {}: panicked: {message}", S::NAME))
        }
    }
}
