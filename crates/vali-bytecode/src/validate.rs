//! The validator (ADR 0005 #7): a module that passes it cannot make the VM
//! index outside a frame, a constant pool, a variable table or a function
//! table, jump into the middle of an instruction, or run off the end of its
//! code. Instructions still check their operands' types and bounds at run
//! time, so the dispatch loop needs no checks of its own.

use std::collections::HashSet;
use std::fmt;

use crate::instruction::Instruction;
use crate::module::{Arity, Module, branch_target};

/// The largest frame format version 1 can address: slot operands are 8 bits,
/// and wide forms do not exist yet.
pub const MAX_SLOTS: u16 = 256;

/// Why a module was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    /// The function at fault, if the problem is in one.
    pub function: Option<u32>,
    /// The index of the instruction's first word, if the problem is in one.
    pub pc: Option<usize>,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.function, self.pc) {
            (Some(function), Some(pc)) => write!(f, "function {function}, word {pc}: ")?,
            (Some(function), None) => write!(f, "function {function}: ")?,
            _ => {}
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for ValidationError {}

/// Checks `module`; see the module documentation for what that guarantees.
pub fn validate(module: &Module) -> Result<(), ValidationError> {
    let fail = |function: Option<u32>, pc: Option<usize>, message: String| {
        Err(ValidationError {
            function,
            pc,
            message,
        })
    };
    let Some(body) = module.functions.get(module.body as usize) else {
        return fail(
            None,
            None,
            format!("body function {} does not exist", module.body),
        );
    };
    if body.arity != Arity::exactly(0) || body.free != 0 {
        return fail(
            Some(module.body),
            None,
            "the body takes no arguments and no free variables".into(),
        );
    }
    for (index, function) in module.functions.iter().enumerate() {
        let index = index as u32;
        let fail = |pc: Option<usize>, message: String| fail(Some(index), pc, message);
        let slots = usize::from(function.slots);
        if function.slots > MAX_SLOTS {
            return fail(
                None,
                format!("{slots} slots; at most {MAX_SLOTS} are addressable"),
            );
        }
        if slots < 1 + function.arity.parameter_slots() {
            return fail(
                None,
                format!("{slots} slots cannot hold the closure and parameters"),
            );
        }

        let mut starts = HashSet::new();
        let mut decoded = Vec::new();
        let mut pc = 0;
        while pc < function.code.len() {
            match Instruction::decode(&function.code, pc) {
                Ok((instruction, len)) => {
                    starts.insert(pc);
                    decoded.push((pc, len, instruction));
                    pc += len;
                }
                Err(e) => return fail(Some(pc), e.to_string()),
            }
        }
        match decoded.last() {
            None => return fail(None, "no code".into()),
            Some((pc, _, last)) if !last.is_terminator() => {
                return fail(
                    Some(*pc),
                    "execution can run off the end of the code".into(),
                );
            }
            _ => {}
        }

        for (pc, len, instruction) in &decoded {
            let fail = |message: String| fail(Some(*pc), message);
            let slot = |s: u8| usize::from(s) < slots;
            let span = |first: u8, count: usize| usize::from(first) + count < slots;
            let ok = match instruction {
                Instruction::Mov { dst, src }
                | Instruction::MakeBox { dst, src }
                | Instruction::Unbox { dst, src }
                | Instruction::Not { dst, src }
                | Instruction::IsNull { dst, src }
                | Instruction::IsPair { dst, src }
                | Instruction::Car { dst, pair: src }
                | Instruction::Cdr { dst, pair: src }
                | Instruction::VectorLength { dst, vector: src } => slot(*dst) && slot(*src),
                Instruction::SetBox { cell: a, src: b }
                | Instruction::SetCar { pair: a, value: b }
                | Instruction::SetCdr { pair: a, value: b } => slot(*a) && slot(*b),
                Instruction::LoadInt { dst, .. } => slot(*dst),
                Instruction::LoadConstant { dst, constant } => {
                    if usize::from(*constant) >= module.constants.len() {
                        return fail(format!("constant {constant} does not exist"));
                    }
                    slot(*dst)
                }
                Instruction::GetVariable { dst: s, variable }
                | Instruction::SetVariable { src: s, variable } => {
                    if usize::from(*variable) >= module.variables.len() {
                        return fail(format!("variable {variable} does not exist"));
                    }
                    slot(*s)
                }
                Instruction::Closure {
                    dst,
                    function: target,
                    free,
                } => {
                    let Some(target_function) = module.functions.get(*target as usize) else {
                        return fail(format!("function {target} does not exist"));
                    };
                    if free.len() != usize::from(target_function.free) {
                        return fail(format!(
                            "function {target} has {} free variables, not {}",
                            target_function.free,
                            free.len()
                        ));
                    }
                    slot(*dst) && free.iter().all(|s| slot(*s))
                }
                Instruction::Free { dst, index } => {
                    if *index >= function.free {
                        return fail(format!("free variable {index} does not exist"));
                    }
                    slot(*dst)
                }
                Instruction::Jump { .. } => true,
                Instruction::If { test: a, .. }
                | Instruction::IfLtImm { a, .. }
                | Instruction::IfNull { a, .. }
                | Instruction::IfPair { a, .. } => slot(*a),
                Instruction::IfEq { a, b, .. }
                | Instruction::IfLt { a, b, .. }
                | Instruction::IfLe { a, b, .. }
                | Instruction::IfNumEq { a, b, .. } => slot(*a) && slot(*b),
                Instruction::Call { base, argc } => span(*base, 3 + usize::from(*argc)),
                Instruction::TailCall { proc, argc } => span(*proc, usize::from(*argc)),
                Instruction::Return { src } => slot(*src),
                Instruction::Add { dst, a, b }
                | Instruction::Sub { dst, a, b }
                | Instruction::Mul { dst, a, b }
                | Instruction::Lt { dst, a, b }
                | Instruction::Le { dst, a, b }
                | Instruction::NumEq { dst, a, b }
                | Instruction::Eq { dst, a, b }
                | Instruction::Cons {
                    dst,
                    car: a,
                    cdr: b,
                }
                | Instruction::MakeVector {
                    dst,
                    len: a,
                    fill: b,
                }
                | Instruction::VectorRef {
                    dst,
                    vector: a,
                    index: b,
                }
                | Instruction::VectorSet {
                    vector: dst,
                    index: a,
                    value: b,
                } => slot(*dst) && slot(*a) && slot(*b),
                Instruction::AddImm { dst, a, .. } | Instruction::SubImm { dst, a, .. } => {
                    slot(*dst) && slot(*a)
                }
            };
            if !ok {
                return fail(format!("a slot is outside the {slots}-slot frame"));
            }
            if let Some(offset) = instruction.offset() {
                let target = branch_target(*pc, *len, offset);
                if !target.is_some_and(|t| starts.contains(&t)) {
                    return fail("a branch target is not the start of an instruction".into());
                }
            }
        }
    }
    Ok(())
}
