//! The validator (ADR 0005 #7): a module that passes it cannot make the VM
//! index outside a frame, a constant pool, a variable table or a function
//! table, jump into the middle of an instruction, or run off the end of its
//! code. Instructions still check their operands' types and bounds at run
//! time, so the dispatch loop needs no checks of its own.

use std::collections::HashSet;
use std::fmt;

use crate::instruction::Instruction;
use crate::module::{Arity, Link, Module, branch_target};

/// The largest frame a function may have: slots up to 255 fit an
/// instruction's fields, larger ones need a `wide` prefix.
pub const MAX_SLOTS: u16 = u16::MAX;

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
    let mut exported = HashSet::new();
    for export in &module.exports {
        if usize::from(export.variable) >= module.variables.len() {
            return fail(
                None,
                None,
                format!(
                    "export {} names variable {}, which does not exist",
                    export.name, export.variable
                ),
            );
        }
        if !exported.insert(&export.name) {
            return fail(None, None, format!("{} is exported twice", export.name));
        }
    }
    for (index, function) in module.functions.iter().enumerate() {
        let index = index as u32;
        let fail = |pc: Option<usize>, message: String| fail(Some(index), pc, message);
        // At most `MAX_SLOTS`, which a u16 cannot exceed.
        let slots = usize::from(function.slots);
        if slots < 1 + function.arity.parameter_slots() {
            return fail(
                None,
                format!("{slots} slots cannot hold the closure and parameters"),
            );
        }

        let mut last = None;
        for position in &function.positions {
            if position.pc as usize >= function.code.len()
                || last.is_some_and(|l| position.pc <= l)
                || usize::from(position.source) >= module.sources.len()
            {
                return fail(
                    Some(position.pc as usize),
                    "positions must be in order, at instructions, in the module's sources".into(),
                );
            }
            last = Some(position.pc);
        }

        // Each copy was called from an earlier one or the function's own
        // code, so a backtrace following calls out always ends.
        for (i, copy) in function.inlined.iter().enumerate() {
            if copy.caller as usize > i
                || copy
                    .call
                    .is_some_and(|(source, ..)| usize::from(source) >= module.sources.len())
            {
                return fail(
                    None,
                    format!("inlined copy {i} has a later caller, or no such source"),
                );
            }
        }
        let mut last = None;
        for scope in &function.scopes {
            if scope.pc as usize >= function.code.len()
                || last.is_some_and(|l| scope.pc <= l)
                || scope.inlined as usize > function.inlined.len()
            {
                return fail(
                    Some(scope.pc as usize),
                    "scopes must be in order, at instructions, in the function's copies".into(),
                );
            }
            last = Some(scope.pc);
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
            let slot = |s: u16| usize::from(s) < slots;
            let span = |first: u16, count: usize| usize::from(first) + count < slots;
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
                Instruction::GetVariable { dst: s, variable } => {
                    if usize::from(*variable) >= module.variables.len() {
                        return fail(format!("variable {variable} does not exist"));
                    }
                    slot(*s)
                }
                Instruction::SetVariable { src: s, variable } => {
                    match module.variables.get(usize::from(*variable)) {
                        None => return fail(format!("variable {variable} does not exist")),
                        // An importer cannot assign another library's
                        // variable (R7RS 5.6.1), a primitive or a native
                        // function.
                        Some(v)
                            if matches!(
                                v.link,
                                Link::Imported(_) | Link::Primitive | Link::Native(_)
                            ) =>
                        {
                            return fail(format!("variable {variable} is imported"));
                        }
                        Some(_) => {}
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
                Instruction::CallDirect {
                    base: first,
                    argc,
                    function: target,
                }
                | Instruction::TailCallDirect {
                    proc: first,
                    argc,
                    function: target,
                } => {
                    // The VM gives the callee a closure of its own, with no
                    // free variables, and checks no arity.
                    let Some(target_function) = module.functions.get(*target as usize) else {
                        return fail(format!("function {target} does not exist"));
                    };
                    if target_function.free != 0 {
                        return fail(format!(
                            "function {target} has free variables, and cannot be called directly"
                        ));
                    }
                    if !target_function.arity.accepts(usize::from(*argc)) {
                        return fail(format!("function {target} does not take {argc} arguments"));
                    }
                    let header = if matches!(instruction, Instruction::CallDirect { .. }) {
                        3
                    } else {
                        0
                    };
                    span(*first, header + usize::from(*argc))
                }
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
                // The closure's free-variable count is only known at run
                // time, where the VM checks `index`.
                Instruction::SetFree { closure, src, .. } => slot(*closure) && slot(*src),
                Instruction::AddImm { dst, a, .. } | Instruction::SubImm { dst, a, .. } => {
                    slot(*dst) && slot(*a)
                }
            };
            if !ok {
                return fail(format!("a slot is outside the {slots}-slot frame"));
            }
            // Slot 0 holds the running closure, which the VM reads back for
            // the code, constants and variables it runs with, and a call's
            // header would overwrite it from base 0.
            if written_slot(instruction) == Some(0)
                || matches!(
                    instruction,
                    Instruction::Call { base: 0, .. } | Instruction::CallDirect { base: 0, .. }
                )
            {
                return fail("slot 0 holds the running closure, and cannot be written".into());
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

/// The slot `instruction` writes its result to, if it writes one.
fn written_slot(instruction: &Instruction) -> Option<u16> {
    match *instruction {
        Instruction::Mov { dst, .. }
        | Instruction::MakeBox { dst, .. }
        | Instruction::Unbox { dst, .. }
        | Instruction::Not { dst, .. }
        | Instruction::IsNull { dst, .. }
        | Instruction::IsPair { dst, .. }
        | Instruction::Car { dst, .. }
        | Instruction::Cdr { dst, .. }
        | Instruction::VectorLength { dst, .. }
        | Instruction::LoadInt { dst, .. }
        | Instruction::LoadConstant { dst, .. }
        | Instruction::GetVariable { dst, .. }
        | Instruction::Closure { dst, .. }
        | Instruction::Free { dst, .. }
        | Instruction::Add { dst, .. }
        | Instruction::Sub { dst, .. }
        | Instruction::Mul { dst, .. }
        | Instruction::Lt { dst, .. }
        | Instruction::Le { dst, .. }
        | Instruction::NumEq { dst, .. }
        | Instruction::Eq { dst, .. }
        | Instruction::Cons { dst, .. }
        | Instruction::MakeVector { dst, .. }
        | Instruction::VectorRef { dst, .. }
        | Instruction::AddImm { dst, .. }
        | Instruction::SubImm { dst, .. } => Some(dst),
        // `Call` and `CallDirect` write `base + 3`, which is never slot 0,
        // and their headers are checked separately above.
        Instruction::SetVariable { .. }
        | Instruction::SetBox { .. }
        | Instruction::Jump { .. }
        | Instruction::If { .. }
        | Instruction::IfEq { .. }
        | Instruction::IfLt { .. }
        | Instruction::IfLe { .. }
        | Instruction::IfNumEq { .. }
        | Instruction::IfLtImm { .. }
        | Instruction::IfNull { .. }
        | Instruction::IfPair { .. }
        | Instruction::Call { .. }
        | Instruction::TailCall { .. }
        | Instruction::CallDirect { .. }
        | Instruction::TailCallDirect { .. }
        | Instruction::Return { .. }
        | Instruction::SetCar { .. }
        | Instruction::SetCdr { .. }
        | Instruction::VectorSet { .. }
        | Instruction::SetFree { .. } => None,
    }
}
