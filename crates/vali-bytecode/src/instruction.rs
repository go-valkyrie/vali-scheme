//! Instructions and their encoding (ADR 0005, `docs/bytecode.md`).
//!
//! Each instruction is one or more little-endian 32-bit words. The first
//! word holds the opcode in its low 8 bits and up to 24 bits of operands in
//! one of three layouts: three 8-bit fields `a b c`, an 8-bit field and a
//! 16-bit field `a d`, or one 24-bit field `e`. Conditional branches add a
//! word holding a signed 32-bit offset; `closure` adds the function index
//! and the slots of its free variables, and the direct calls the function
//! index. Offsets count words from the start of the next instruction.

use std::fmt;

macro_rules! opcodes {
    ($($name:ident = $value:literal, $mnemonic:literal;)*) => {
        /// Every opcode, with its number in the low byte of an instruction's
        /// first word.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Opcode {
            $(
                #[doc = concat!("`", $mnemonic, "`")]
                $name = $value,
            )*
        }

        impl Opcode {
            /// Every opcode, in numeric order.
            pub const ALL: &'static [Opcode] = &[$(Opcode::$name),*];

            /// The opcode numbered `n`, if there is one.
            pub fn from_u8(n: u8) -> Option<Opcode> {
                match n {
                    $($value => Some(Opcode::$name),)*
                    _ => None,
                }
            }

            /// The opcode's name in disassembly.
            pub fn mnemonic(self) -> &'static str {
                match self {
                    $(Opcode::$name => $mnemonic,)*
                }
            }
        }
    };
}

opcodes! {
    Mov = 1, "mov";
    LoadConstant = 2, "load-constant";
    LoadInt = 3, "load-int";
    GetVariable = 4, "get-variable";
    SetVariable = 5, "set-variable";
    Closure = 6, "closure";
    Free = 7, "free";
    MakeBox = 8, "box";
    Unbox = 9, "unbox";
    SetBox = 10, "set-box";
    Jump = 11, "jump";
    If = 12, "if";
    IfEq = 13, "if-eq";
    IfLt = 14, "if-lt";
    IfLe = 15, "if-le";
    IfNumEq = 16, "if-num-eq";
    IfLtImm = 17, "if-lt-imm";
    IfNull = 18, "if-null";
    IfPair = 19, "if-pair";
    Call = 20, "call";
    TailCall = 21, "tail-call";
    Return = 22, "return";
    Add = 23, "add";
    Sub = 24, "sub";
    Mul = 25, "mul";
    AddImm = 26, "add-imm";
    SubImm = 27, "sub-imm";
    Lt = 28, "lt";
    Le = 29, "le";
    NumEq = 30, "num-eq";
    Eq = 31, "eq";
    Not = 32, "not";
    Cons = 33, "cons";
    Car = 34, "car";
    Cdr = 35, "cdr";
    SetCar = 36, "set-car";
    SetCdr = 37, "set-cdr";
    IsNull = 38, "null?";
    IsPair = 39, "pair?";
    MakeVector = 40, "make-vector";
    VectorRef = 41, "vector-ref";
    VectorSet = 42, "vector-set";
    VectorLength = 43, "vector-length";
    SetFree = 44, "set-free";
    Wide = 45, "wide";
    CallDirect = 46, "call-direct";
    TailCallDirect = 47, "tail-call-direct";
}

/// A frame slot (a register). Slots up to 255 fit an instruction's 8-bit
/// fields; larger ones need a `wide` prefix (`docs/bytecode.md`).
pub type Slot = u16;

/// One instruction, decoded. Field names say what each operand is; slots
/// are frame slots, and `offset`s count words from the next instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instruction {
    /// `dst ← src`.
    Mov { dst: Slot, src: Slot },
    /// `dst ←` the module's constant number `constant`.
    LoadConstant { dst: Slot, constant: u16 },
    /// `dst ←` the fixnum `value`.
    LoadInt { dst: Slot, value: i16 },
    /// `dst ←` the module's variable number `variable`; an error if it has
    /// no value yet.
    GetVariable { dst: Slot, variable: u16 },
    /// The module's variable number `variable` `← src`.
    SetVariable { src: Slot, variable: u16 },
    /// `dst ←` a new closure of the module's function number `function`,
    /// capturing the given slots as its free variables, in order.
    Closure {
        dst: Slot,
        function: u32,
        free: Vec<Slot>,
    },
    /// `dst ←` free variable `index` of the running closure.
    Free { dst: Slot, index: u16 },
    /// `dst ←` a new box holding `src`.
    MakeBox { dst: Slot, src: Slot },
    /// `dst ←` the contents of the box in `src`.
    Unbox { dst: Slot, src: Slot },
    /// The box in `cell` now holds `src`.
    SetBox { cell: Slot, src: Slot },
    /// Jump. The offset fits in 24 bits.
    Jump { offset: i32 },
    /// Continue if `test` is true; otherwise jump.
    If { test: Slot, offset: i32 },
    /// Continue if `a` and `b` are `eq?`; otherwise jump.
    IfEq { a: Slot, b: Slot, offset: i32 },
    /// Continue if `a < b`; otherwise jump.
    IfLt { a: Slot, b: Slot, offset: i32 },
    /// Continue if `a <= b`; otherwise jump.
    IfLe { a: Slot, b: Slot, offset: i32 },
    /// Continue if `a = b`; otherwise jump.
    IfNumEq { a: Slot, b: Slot, offset: i32 },
    /// Continue if `a < imm`; otherwise jump.
    IfLtImm { a: Slot, imm: i8, offset: i32 },
    /// Continue if `a` is the empty list; otherwise jump.
    IfNull { a: Slot, offset: i32 },
    /// Continue if `a` is a pair; otherwise jump.
    IfPair { a: Slot, offset: i32 },
    /// Call. Slots `base` to `base + 2` hold the callee's frame header,
    /// `base + 3` the procedure, and the `argc` slots after it the
    /// arguments. The result arrives in `base + 3`.
    Call { base: Slot, argc: u16 },
    /// Tail call the procedure in `proc` with the `argc` arguments after it,
    /// reusing this frame.
    TailCall { proc: Slot, argc: u16 },
    /// Call the module's function number `function`, which has no free
    /// variables and takes `argc` arguments, as [`Instruction::Call`] does,
    /// with that function's closure for the procedure: slot `base + 3` is
    /// the VM's to fill.
    CallDirect {
        base: Slot,
        argc: u16,
        function: u32,
    },
    /// Tail call the module's function number `function` with the `argc`
    /// arguments after `proc`, as [`Instruction::CallDirect`] calls it.
    TailCallDirect {
        proc: Slot,
        argc: u16,
        function: u32,
    },
    /// Return `src`.
    Return { src: Slot },
    /// `dst ← a + b`.
    Add { dst: Slot, a: Slot, b: Slot },
    /// `dst ← a - b`.
    Sub { dst: Slot, a: Slot, b: Slot },
    /// `dst ← a × b`.
    Mul { dst: Slot, a: Slot, b: Slot },
    /// `dst ← a + imm`.
    AddImm { dst: Slot, a: Slot, imm: i8 },
    /// `dst ← a - imm`.
    SubImm { dst: Slot, a: Slot, imm: i8 },
    /// `dst ← (< a b)`.
    Lt { dst: Slot, a: Slot, b: Slot },
    /// `dst ← (<= a b)`.
    Le { dst: Slot, a: Slot, b: Slot },
    /// `dst ← (= a b)`.
    NumEq { dst: Slot, a: Slot, b: Slot },
    /// `dst ← (eq? a b)`.
    Eq { dst: Slot, a: Slot, b: Slot },
    /// `dst ← (not src)`.
    Not { dst: Slot, src: Slot },
    /// `dst ← (cons car cdr)`.
    Cons { dst: Slot, car: Slot, cdr: Slot },
    /// `dst ← (car pair)`.
    Car { dst: Slot, pair: Slot },
    /// `dst ← (cdr pair)`.
    Cdr { dst: Slot, pair: Slot },
    /// `(set-car! pair value)`.
    SetCar { pair: Slot, value: Slot },
    /// `(set-cdr! pair value)`.
    SetCdr { pair: Slot, value: Slot },
    /// `dst ← (null? src)`.
    IsNull { dst: Slot, src: Slot },
    /// `dst ← (pair? src)`.
    IsPair { dst: Slot, src: Slot },
    /// `dst ← (make-vector len fill)`.
    MakeVector { dst: Slot, len: Slot, fill: Slot },
    /// `dst ← (vector-ref vector index)`.
    VectorRef {
        dst: Slot,
        vector: Slot,
        index: Slot,
    },
    /// `(vector-set! vector index value)`.
    VectorSet {
        vector: Slot,
        index: Slot,
        value: Slot,
    },
    /// `dst ← (vector-length vector)`.
    VectorLength { dst: Slot, vector: Slot },
    /// Free variable `index` of the closure in `closure` now holds `src`:
    /// how `letrec` closes the loops between closures that refer to each
    /// other.
    SetFree {
        closure: Slot,
        index: u16,
        src: Slot,
    },
}

/// The range of a [`Instruction::Jump`] offset.
pub const JUMP_RANGE: std::ops::RangeInclusive<i32> = -(1 << 23)..=(1 << 23) - 1;

/// What went wrong decoding an instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The low byte is not an opcode.
    UnknownOpcode(u8),
    /// The instruction runs past the end of the code.
    Truncated,
    /// A `wide` prefix that is not needed, or that sets bits a field does
    /// not have: every instruction has one encoding.
    NotCanonical,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::UnknownOpcode(n) => write!(f, "unknown opcode {n}"),
            DecodeError::Truncated => f.write_str("instruction runs past the end of the code"),
            DecodeError::NotCanonical => f.write_str("a wide prefix the instruction does not need"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn word_abc(op: Opcode, a: u8, b: u8, c: u8) -> u32 {
    op as u32 | (a as u32) << 8 | (b as u32) << 16 | (c as u32) << 24
}

/// Whether fields this large need a `wide` prefix.
fn needs_wide(a: u16, b: u16, c: u16) -> bool {
    a > 0xFF || b > 0xFF || c > 0xFF
}

/// Pushes the `wide` prefix holding the high bytes of `a`, `b` and `c`.
fn push_wide(code: &mut Vec<u32>, a: u16, b: u16, c: u16) {
    code.push(word_abc(
        Opcode::Wide,
        (a >> 8) as u8,
        (b >> 8) as u8,
        (c >> 8) as u8,
    ));
}

/// Pushes an instruction in the `a b c` layout, after a `wide` prefix if a
/// field needs more than 8 bits.
fn abc(code: &mut Vec<u32>, op: Opcode, a: u16, b: u16, c: u16) {
    if needs_wide(a, b, c) {
        push_wide(code, a, b, c);
    }
    code.push(word_abc(op, a as u8, b as u8, c as u8));
}

/// Pushes an instruction in the `a d` layout, after a `wide` prefix if `a`
/// needs more than 8 bits.
fn ad(code: &mut Vec<u32>, op: Opcode, a: u16, d: u16) {
    if a > 0xFF {
        push_wide(code, a, 0, 0);
    }
    code.push(op as u32 | ((a as u8) as u32) << 8 | (d as u32) << 16);
}

fn e(op: Opcode, x: u32) -> u32 {
    op as u32 | (x & 0xFF_FFFF) << 8
}

/// The `a`, `b` and `c` fields of a first word.
pub fn fields(word: u32) -> (u8, u8, u8) {
    ((word >> 8) as u8, (word >> 16) as u8, (word >> 24) as u8)
}

/// The `d` field of a first word.
pub fn field_d(word: u32) -> u16 {
    (word >> 16) as u16
}

/// The `e` field of a first word, sign-extended.
pub fn field_e_signed(word: u32) -> i32 {
    (word as i32) >> 8
}

impl Instruction {
    /// The instruction's opcode.
    pub fn opcode(&self) -> Opcode {
        use Instruction as I;
        match self {
            I::Mov { .. } => Opcode::Mov,
            I::LoadConstant { .. } => Opcode::LoadConstant,
            I::LoadInt { .. } => Opcode::LoadInt,
            I::GetVariable { .. } => Opcode::GetVariable,
            I::SetVariable { .. } => Opcode::SetVariable,
            I::Closure { .. } => Opcode::Closure,
            I::Free { .. } => Opcode::Free,
            I::MakeBox { .. } => Opcode::MakeBox,
            I::Unbox { .. } => Opcode::Unbox,
            I::SetBox { .. } => Opcode::SetBox,
            I::Jump { .. } => Opcode::Jump,
            I::If { .. } => Opcode::If,
            I::IfEq { .. } => Opcode::IfEq,
            I::IfLt { .. } => Opcode::IfLt,
            I::IfLe { .. } => Opcode::IfLe,
            I::IfNumEq { .. } => Opcode::IfNumEq,
            I::IfLtImm { .. } => Opcode::IfLtImm,
            I::IfNull { .. } => Opcode::IfNull,
            I::IfPair { .. } => Opcode::IfPair,
            I::Call { .. } => Opcode::Call,
            I::TailCall { .. } => Opcode::TailCall,
            I::CallDirect { .. } => Opcode::CallDirect,
            I::TailCallDirect { .. } => Opcode::TailCallDirect,
            I::Return { .. } => Opcode::Return,
            I::Add { .. } => Opcode::Add,
            I::Sub { .. } => Opcode::Sub,
            I::Mul { .. } => Opcode::Mul,
            I::AddImm { .. } => Opcode::AddImm,
            I::SubImm { .. } => Opcode::SubImm,
            I::Lt { .. } => Opcode::Lt,
            I::Le { .. } => Opcode::Le,
            I::NumEq { .. } => Opcode::NumEq,
            I::Eq { .. } => Opcode::Eq,
            I::Not { .. } => Opcode::Not,
            I::Cons { .. } => Opcode::Cons,
            I::Car { .. } => Opcode::Car,
            I::Cdr { .. } => Opcode::Cdr,
            I::SetCar { .. } => Opcode::SetCar,
            I::SetCdr { .. } => Opcode::SetCdr,
            I::IsNull { .. } => Opcode::IsNull,
            I::IsPair { .. } => Opcode::IsPair,
            I::MakeVector { .. } => Opcode::MakeVector,
            I::VectorRef { .. } => Opcode::VectorRef,
            I::VectorSet { .. } => Opcode::VectorSet,
            I::VectorLength { .. } => Opcode::VectorLength,
            I::SetFree { .. } => Opcode::SetFree,
        }
    }

    /// The branch offset, if this instruction branches.
    pub fn offset(&self) -> Option<i32> {
        use Instruction as I;
        match *self {
            I::Jump { offset }
            | I::If { offset, .. }
            | I::IfEq { offset, .. }
            | I::IfLt { offset, .. }
            | I::IfLe { offset, .. }
            | I::IfNumEq { offset, .. }
            | I::IfLtImm { offset, .. }
            | I::IfNull { offset, .. }
            | I::IfPair { offset, .. } => Some(offset),
            _ => None,
        }
    }

    /// Whether execution never continues to the next instruction.
    pub fn is_terminator(&self) -> bool {
        matches!(
            self,
            Instruction::Jump { .. }
                | Instruction::TailCall { .. }
                | Instruction::TailCallDirect { .. }
                | Instruction::Return { .. }
        )
    }

    /// Appends the instruction's words to `code`.
    ///
    /// # Panics
    ///
    /// If a jump offset does not fit in 24 bits, or a closure captures more
    /// than 255 slots.
    pub fn encode(&self, code: &mut Vec<u32>) {
        use Instruction as I;
        let op = self.opcode();
        match *self {
            I::Mov { dst, src }
            | I::MakeBox { dst, src }
            | I::Unbox { dst, src }
            | I::Not { dst, src }
            | I::IsNull { dst, src }
            | I::IsPair { dst, src } => abc(code, op, dst, src, 0),
            I::Car { dst, pair } | I::Cdr { dst, pair } => abc(code, op, dst, pair, 0),
            I::VectorLength { dst, vector } => abc(code, op, dst, vector, 0),
            I::Free { dst, index } => abc(code, op, dst, index, 0),
            I::SetBox { cell, src } => abc(code, op, cell, src, 0),
            I::SetCar { pair, value } | I::SetCdr { pair, value } => {
                abc(code, op, pair, value, 0);
            }
            I::LoadConstant { dst, constant } => ad(code, op, dst, constant),
            I::LoadInt { dst, value } => ad(code, op, dst, value as u16),
            I::GetVariable { dst, variable } => ad(code, op, dst, variable),
            I::SetVariable { src, variable } => ad(code, op, src, variable),
            I::Closure {
                dst,
                function,
                ref free,
            } => {
                let count = u16::try_from(free.len()).expect("at most 65,535 free variables");
                // Wide, each slot takes two bytes; otherwise one.
                let wide = needs_wide(dst, count, 0) || free.iter().any(|&s| s > 0xFF);
                if wide {
                    push_wide(code, dst, count, 0);
                }
                code.push(word_abc(op, dst as u8, count as u8, 0));
                code.push(function);
                let per_word = if wide { 2 } else { 4 };
                let bits = 32 / per_word;
                for chunk in free.chunks(per_word) {
                    let mut word = 0u32;
                    for (i, &slot) in chunk.iter().enumerate() {
                        word |= (slot as u32) << (bits * i);
                    }
                    code.push(word);
                }
            }
            I::Jump { offset } => {
                assert!(JUMP_RANGE.contains(&offset), "jump offset out of range");
                code.push(e(op, offset as u32));
            }
            I::If { test, offset } => {
                abc(code, op, test, 0, 0);
                code.push(offset as u32);
            }
            I::IfNull { a, offset } | I::IfPair { a, offset } => {
                abc(code, op, a, 0, 0);
                code.push(offset as u32);
            }
            I::IfEq { a, b, offset }
            | I::IfLt { a, b, offset }
            | I::IfLe { a, b, offset }
            | I::IfNumEq { a, b, offset } => {
                abc(code, op, a, b, 0);
                code.push(offset as u32);
            }
            I::IfLtImm { a, imm, offset } => {
                abc(code, op, a, u16::from(imm as u8), 0);
                code.push(offset as u32);
            }
            I::Call { base, argc } => abc(code, op, base, argc, 0),
            I::TailCall { proc, argc } => abc(code, op, proc, argc, 0),
            I::CallDirect {
                base: a,
                argc,
                function,
            }
            | I::TailCallDirect {
                proc: a,
                argc,
                function,
            } => {
                abc(code, op, a, argc, 0);
                code.push(function);
            }
            I::Return { src } => abc(code, op, src, 0, 0),
            I::Add { dst, a, b }
            | I::Sub { dst, a, b }
            | I::Mul { dst, a, b }
            | I::Lt { dst, a, b }
            | I::Le { dst, a, b }
            | I::NumEq { dst, a, b }
            | I::Eq { dst, a, b } => abc(code, op, dst, a, b),
            I::AddImm { dst, a, imm } | I::SubImm { dst, a, imm } => {
                abc(code, op, dst, a, u16::from(imm as u8));
            }
            I::Cons { dst, car, cdr } => abc(code, op, dst, car, cdr),
            I::MakeVector { dst, len, fill } => abc(code, op, dst, len, fill),
            I::VectorRef { dst, vector, index } => abc(code, op, dst, vector, index),
            I::VectorSet {
                vector,
                index,
                value,
            } => abc(code, op, vector, index, value),
            I::SetFree {
                closure,
                index,
                src,
            } => abc(code, op, closure, index, src),
        }
    }

    /// Decodes the instruction starting at `code[pc]`, returning it and how
    /// many words it takes.
    pub fn decode(code: &[u32], start: usize) -> Result<(Instruction, usize), DecodeError> {
        let first = *code.get(start).ok_or(DecodeError::Truncated)?;
        if first as u8 != Opcode::Wide as u8 {
            return Self::decode_one(code, start, None);
        }
        // Wide: the next instruction's fields, with these high bytes. It
        // must need them, so each instruction has one encoding.
        let (instruction, len) = Self::decode_one(code, start + 1, Some(fields(first)))?;
        let mut again = Vec::new();
        instruction.encode(&mut again);
        if code.get(start..start + 1 + len) != Some(&again[..]) {
            return Err(DecodeError::NotCanonical);
        }
        Ok((instruction, 1 + len))
    }

    /// Decodes the instruction at `code[pc]`, its fields widened by the
    /// high bytes of a `wide` prefix if it had one.
    fn decode_one(
        code: &[u32],
        pc: usize,
        high: Option<(u8, u8, u8)>,
    ) -> Result<(Instruction, usize), DecodeError> {
        use Instruction as I;
        let word = *code.get(pc).ok_or(DecodeError::Truncated)?;
        let op = Opcode::from_u8(word as u8).ok_or(DecodeError::UnknownOpcode(word as u8))?;
        let (ha, hb, hc) = high.unwrap_or((0, 0, 0));
        let (a, b, c) = fields(word);
        let widen = |high: u8, low: u8| u16::from(high) << 8 | u16::from(low);
        let (a, b, c) = (widen(ha, a), widen(hb, b), widen(hc, c));
        let d = field_d(word);
        let next = |n: usize| code.get(pc + n).copied().ok_or(DecodeError::Truncated);
        let branch = || next(1).map(|w| w as i32);
        let instruction = match op {
            Opcode::Mov => I::Mov { dst: a, src: b },
            Opcode::LoadConstant => I::LoadConstant {
                dst: a,
                constant: d,
            },
            Opcode::LoadInt => I::LoadInt {
                dst: a,
                value: d as i16,
            },
            Opcode::GetVariable => I::GetVariable {
                dst: a,
                variable: d,
            },
            Opcode::SetVariable => I::SetVariable {
                src: a,
                variable: d,
            },
            Opcode::Closure => {
                let function = next(1)?;
                let count = b as usize;
                let per_word = if high.is_some() { 2 } else { 4 };
                let bits = 32 / per_word;
                if count.div_ceil(per_word) > code.len().saturating_sub(pc + 2) {
                    return Err(DecodeError::Truncated);
                }
                let mut free = Vec::with_capacity(count);
                for i in 0..count {
                    let word = next(2 + i / per_word)?;
                    let mask = (1u32 << bits) - 1;
                    free.push(((word >> (bits * (i % per_word))) & mask) as u16);
                }
                let len = 2 + count.div_ceil(per_word);
                return Ok((
                    I::Closure {
                        dst: a,
                        function,
                        free,
                    },
                    len,
                ));
            }
            Opcode::Free => I::Free { dst: a, index: b },
            Opcode::MakeBox => I::MakeBox { dst: a, src: b },
            Opcode::Unbox => I::Unbox { dst: a, src: b },
            Opcode::SetBox => I::SetBox { cell: a, src: b },
            Opcode::Jump => I::Jump {
                offset: field_e_signed(word),
            },
            Opcode::If => I::If {
                test: a,
                offset: branch()?,
            },
            Opcode::IfEq => I::IfEq {
                a,
                b,
                offset: branch()?,
            },
            Opcode::IfLt => I::IfLt {
                a,
                b,
                offset: branch()?,
            },
            Opcode::IfLe => I::IfLe {
                a,
                b,
                offset: branch()?,
            },
            Opcode::IfNumEq => I::IfNumEq {
                a,
                b,
                offset: branch()?,
            },
            Opcode::IfLtImm => I::IfLtImm {
                a,
                imm: b as u8 as i8,
                offset: branch()?,
            },
            Opcode::IfNull => I::IfNull {
                a,
                offset: branch()?,
            },
            Opcode::IfPair => I::IfPair {
                a,
                offset: branch()?,
            },
            Opcode::Call => I::Call { base: a, argc: b },
            Opcode::TailCall => I::TailCall { proc: a, argc: b },
            Opcode::CallDirect => {
                let function = next(1)?;
                return Ok((
                    I::CallDirect {
                        base: a,
                        argc: b,
                        function,
                    },
                    2,
                ));
            }
            Opcode::TailCallDirect => {
                let function = next(1)?;
                return Ok((
                    I::TailCallDirect {
                        proc: a,
                        argc: b,
                        function,
                    },
                    2,
                ));
            }
            Opcode::Return => I::Return { src: a },
            Opcode::Add => I::Add { dst: a, a: b, b: c },
            Opcode::Sub => I::Sub { dst: a, a: b, b: c },
            Opcode::Mul => I::Mul { dst: a, a: b, b: c },
            Opcode::AddImm => I::AddImm {
                dst: a,
                a: b,
                imm: c as u8 as i8,
            },
            Opcode::SubImm => I::SubImm {
                dst: a,
                a: b,
                imm: c as u8 as i8,
            },
            Opcode::Lt => I::Lt { dst: a, a: b, b: c },
            Opcode::Le => I::Le { dst: a, a: b, b: c },
            Opcode::NumEq => I::NumEq { dst: a, a: b, b: c },
            Opcode::Eq => I::Eq { dst: a, a: b, b: c },
            Opcode::Not => I::Not { dst: a, src: b },
            Opcode::Cons => I::Cons {
                dst: a,
                car: b,
                cdr: c,
            },
            Opcode::Car => I::Car { dst: a, pair: b },
            Opcode::Cdr => I::Cdr { dst: a, pair: b },
            Opcode::SetCar => I::SetCar { pair: a, value: b },
            Opcode::SetCdr => I::SetCdr { pair: a, value: b },
            Opcode::IsNull => I::IsNull { dst: a, src: b },
            Opcode::IsPair => I::IsPair { dst: a, src: b },
            Opcode::MakeVector => I::MakeVector {
                dst: a,
                len: b,
                fill: c,
            },
            Opcode::VectorRef => I::VectorRef {
                dst: a,
                vector: b,
                index: c,
            },
            Opcode::VectorSet => I::VectorSet {
                vector: a,
                index: b,
                value: c,
            },
            Opcode::VectorLength => I::VectorLength { dst: a, vector: b },
            Opcode::SetFree => I::SetFree {
                closure: a,
                index: b,
                src: c,
            },
            Opcode::Wide => return Err(DecodeError::NotCanonical),
        };
        let len = if instruction.offset().is_some() && op != Opcode::Jump {
            2
        } else {
            1
        };
        Ok((instruction, len))
    }
}

impl fmt::Display for Instruction {
    /// Disassembly syntax: the mnemonic, then operands. Slots print as
    /// `rN`; branch offsets print relative, and [`crate::disassemble`]
    /// shows targets.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Instruction as I;
        let m = self.opcode().mnemonic();
        match self {
            I::Mov { dst, src }
            | I::MakeBox { dst, src }
            | I::Unbox { dst, src }
            | I::Not { dst, src }
            | I::IsNull { dst, src }
            | I::IsPair { dst, src } => write!(f, "{m} r{dst}, r{src}"),
            I::Car { dst, pair } | I::Cdr { dst, pair } => write!(f, "{m} r{dst}, r{pair}"),
            I::VectorLength { dst, vector } => write!(f, "{m} r{dst}, r{vector}"),
            I::SetFree {
                closure,
                index,
                src,
            } => write!(f, "{m} r{closure}, {index}, r{src}"),
            I::Free { dst, index } => write!(f, "{m} r{dst}, {index}"),
            I::SetBox { cell, src } => write!(f, "{m} r{cell}, r{src}"),
            I::SetCar { pair, value } | I::SetCdr { pair, value } => {
                write!(f, "{m} r{pair}, r{value}")
            }
            I::LoadConstant { dst, constant } => write!(f, "{m} r{dst}, k{constant}"),
            I::LoadInt { dst, value } => write!(f, "{m} r{dst}, {value}"),
            I::GetVariable { dst, variable } => write!(f, "{m} r{dst}, v{variable}"),
            I::SetVariable { src, variable } => write!(f, "{m} v{variable}, r{src}"),
            I::Closure {
                dst,
                function,
                free,
            } => {
                write!(f, "{m} r{dst}, f{function}")?;
                for slot in free {
                    write!(f, ", r{slot}")?;
                }
                Ok(())
            }
            I::Jump { offset } => write!(f, "{m} {offset:+}"),
            I::If { test, offset } => write!(f, "{m} r{test}, {offset:+}"),
            I::IfNull { a, offset } | I::IfPair { a, offset } => write!(f, "{m} r{a}, {offset:+}"),
            I::IfEq { a, b, offset }
            | I::IfLt { a, b, offset }
            | I::IfLe { a, b, offset }
            | I::IfNumEq { a, b, offset } => write!(f, "{m} r{a}, r{b}, {offset:+}"),
            I::IfLtImm { a, imm, offset } => write!(f, "{m} r{a}, {imm}, {offset:+}"),
            I::Call { base, argc } => write!(f, "{m} r{base}, {argc}"),
            I::TailCall { proc, argc } => write!(f, "{m} r{proc}, {argc}"),
            I::CallDirect {
                base: a,
                argc,
                function,
            }
            | I::TailCallDirect {
                proc: a,
                argc,
                function,
            } => write!(f, "{m} r{a}, {argc}, f{function}"),
            I::Return { src } => write!(f, "{m} r{src}"),
            I::Add { dst, a, b }
            | I::Sub { dst, a, b }
            | I::Mul { dst, a, b }
            | I::Lt { dst, a, b }
            | I::Le { dst, a, b }
            | I::NumEq { dst, a, b }
            | I::Eq { dst, a, b } => write!(f, "{m} r{dst}, r{a}, r{b}"),
            I::AddImm { dst, a, imm } | I::SubImm { dst, a, imm } => {
                write!(f, "{m} r{dst}, r{a}, {imm}")
            }
            I::Cons { dst, car, cdr } => write!(f, "{m} r{dst}, r{car}, r{cdr}"),
            I::MakeVector { dst, len, fill } => write!(f, "{m} r{dst}, r{len}, r{fill}"),
            I::VectorRef { dst, vector, index } => write!(f, "{m} r{dst}, r{vector}, r{index}"),
            I::VectorSet {
                vector,
                index,
                value,
            } => write!(f, "{m} r{vector}, r{index}, r{value}"),
        }
    }
}
