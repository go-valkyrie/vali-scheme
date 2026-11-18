//! The ownership pass: under precise counting, the runtime rewrites each
//! function it loads so that its code counts its own frame slots (ADR 0003,
//! ADR 0017 #6).
//!
//! Without it, the VM counts a frame's every slot out as the frame returns,
//! and counts every value copied between slots in and out again. With it,
//! a returning frame counts out only its closure, and a tail call only the
//! slots its procedure and arguments move into, so the code itself leaves
//! no other reference behind. The pass places:
//!
//! - a `move` in place of a `mov` whose source dies there, as the slots'
//!   liveness says, which hands the reference over without counting;
//! - a `drop` of each slot that may still hold a reference, by a forward
//!   analysis, right before a call, tail call or return that needs it to
//!   hold none. The value there is always dead. A dead value written over
//!   first needs no drop: writing a slot still counts the old value out.
//!   Results that are never references (`load-int`, comparisons and the
//!   like) need none either.
//!
//! What makes that safe is the contract the rewritten code keeps, which
//! [`check`] verifies. Slot 0, the running closure, is the VM's throughout:
//!
//! - **calls:** at `call base, argc` or `call-direct base, argc`, no slot
//!   past the arguments holds a reference, since the callee's frame takes
//!   those slots over. The VM counts out the three slots of the callee's
//!   header itself;
//! - **tail calls:** no slot holds a reference except the procedure and
//!   arguments, and the slots they move into;
//! - **returns:** no slot holds a reference except the value returned,
//!   which moves out of its slot.
//!
//! The pass works on validated code, and its input never holds `move` or
//! `drop`: the validator refuses them, so an untrusted module cannot place
//! one. Run-format code stays independent of the memory strategy; only an
//! isolate that counts precisely rewrites it.

use std::collections::HashMap;
use std::fmt;

use crate::builder::{BuildError, FunctionBuilder, Label};
use crate::instruction::{Instruction, Slot};
use crate::module::{Arity, Function, Module, branch_target};

/// The module's functions, rewritten. `module` must be valid.
pub fn insert(module: &Module) -> Result<Vec<Function>, BuildError> {
    module
        .functions
        .iter()
        .map(|f| {
            let rewritten = rewrite(f)?;
            if cfg!(debug_assertions)
                && let Err(e) = check(&rewritten)
            {
                panic!(
                    "the ownership pass broke its contract in {}: {e}",
                    f.name.as_deref().unwrap_or("<anonymous>")
                );
            }
            Ok(rewritten)
        })
        .collect()
}

/// A set of slots, one bit each. Slot 0 is never in one.
#[derive(Clone, PartialEq, Eq)]
struct Slots(Vec<u64>);

impl Slots {
    fn new(slots: usize) -> Slots {
        Slots(vec![0; slots.div_ceil(64)])
    }

    /// Every slot of a frame of `slots` slots but 0.
    fn all(slots: usize) -> Slots {
        let mut set = Slots::new(slots);
        set.extend(1, slots);
        set
    }

    /// Adds the slots from `from` to `to`, exclusive.
    fn extend(&mut self, from: usize, to: usize) {
        let mut s = from.max(1);
        while s < to {
            let (word, bit) = (s / 64, s % 64);
            let n = (64 - bit).min(to - s);
            self.0[word] |= if n == 64 {
                u64::MAX
            } else {
                ((1 << n) - 1) << bit
            };
            s += n;
        }
    }

    fn insert(&mut self, s: Slot) {
        if s != 0 {
            self.0[usize::from(s) / 64] |= 1 << (s % 64);
        }
    }

    fn remove(&mut self, s: Slot) {
        self.0[usize::from(s) / 64] &= !(1 << (s % 64));
    }

    fn contains(&self, s: Slot) -> bool {
        self.0[usize::from(s) / 64] & (1 << (s % 64)) != 0
    }

    /// Removes every slot from `from` up.
    fn truncate(&mut self, from: Slot) {
        let from = usize::from(from);
        let (word, bit) = (from / 64, from % 64);
        if let Some(w) = self.0.get_mut(word) {
            *w &= (1u64 << bit) - 1;
        }
        for w in self.0.iter_mut().skip(word + 1) {
            *w = 0;
        }
    }

    fn union(&mut self, other: &Slots) -> bool {
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            let new = *a | b;
            changed |= new != *a;
            *a = new;
        }
        changed
    }

    /// The slots in both `self` and `other`, in order.
    fn within(&self, other: &Slots) -> Vec<Slot> {
        let mut out = Vec::new();
        for (i, (a, b)) in self.0.iter().zip(&other.0).enumerate() {
            let mut bits = a & b;
            while bits != 0 {
                out.push((i * 64 + bits.trailing_zeros() as usize) as Slot);
                bits &= bits - 1;
            }
        }
        out
    }
}

/// What an instruction does to frame slots, slot 0 left out.
#[derive(Default)]
struct Effect {
    /// The slots it reads.
    uses: Vec<Slot>,
    /// The slot it writes its result to.
    def: Option<Slot>,
    /// For a call: every slot from here up is the callee's, and after it
    /// holds no reference but the result.
    kill: Option<Slot>,
    /// The slots whose references it takes over.
    consumed: Vec<Slot>,
}

fn range(first: usize, count: usize) -> impl Iterator<Item = Slot> {
    (first..first + count).map(|s| s as Slot)
}

fn effect(instruction: &Instruction) -> Effect {
    use Instruction as I;
    let reads = |uses: &[Slot], def: Option<Slot>| Effect {
        uses: uses.to_vec(),
        def,
        ..Effect::default()
    };
    let mut effect = match *instruction {
        I::Mov { dst, src }
        | I::MakeBox { dst, src }
        | I::Unbox { dst, src }
        | I::Not { dst, src }
        | I::IsNull { dst, src }
        | I::IsPair { dst, src }
        | I::Car { dst, pair: src }
        | I::Cdr { dst, pair: src }
        | I::VectorLength { dst, vector: src }
        | I::AddImm { dst, a: src, .. }
        | I::SubImm { dst, a: src, .. } => reads(&[src], Some(dst)),
        I::Move { dst, src } => Effect {
            uses: vec![src],
            def: Some(dst),
            kill: None,
            consumed: vec![src],
        },
        I::Drop { slot } => Effect {
            consumed: vec![slot],
            ..Effect::default()
        },
        I::LoadConstant { dst, .. }
        | I::LoadInt { dst, .. }
        | I::GetVariable { dst, .. }
        | I::Free { dst, .. } => reads(&[], Some(dst)),
        I::SetVariable { src, .. } => reads(&[src], None),
        I::Closure { dst, ref free, .. } => reads(free, Some(dst)),
        I::SetBox { cell: a, src: b }
        | I::SetCar { pair: a, value: b }
        | I::SetCdr { pair: a, value: b }
        | I::IfEq { a, b, .. }
        | I::IfLt { a, b, .. }
        | I::IfLe { a, b, .. }
        | I::IfNumEq { a, b, .. }
        | I::SetFree {
            closure: a, src: b, ..
        } => reads(&[a, b], None),
        I::If { test: a, .. }
        | I::IfLtImm { a, .. }
        | I::IfNull { a, .. }
        | I::IfPair { a, .. } => reads(&[a], None),
        I::Jump { .. } => Effect::default(),
        I::Add { dst, a, b }
        | I::Sub { dst, a, b }
        | I::Mul { dst, a, b }
        | I::Lt { dst, a, b }
        | I::Le { dst, a, b }
        | I::NumEq { dst, a, b }
        | I::Eq { dst, a, b }
        | I::Cons {
            dst,
            car: a,
            cdr: b,
        }
        | I::MakeVector {
            dst,
            len: a,
            fill: b,
        }
        | I::VectorRef {
            dst,
            vector: a,
            index: b,
        } => reads(&[a, b], Some(dst)),
        I::VectorSet {
            vector,
            index,
            value,
        } => reads(&[vector, index, value], None),
        I::Call { base, argc } | I::CallDirect { base, argc, .. } => {
            // A direct call's procedure is the VM's to put in place.
            let first = if matches!(instruction, I::Call { .. }) {
                3
            } else {
                4
            };
            let window: Vec<Slot> =
                range(usize::from(base) + first, 4 - first + usize::from(argc)).collect();
            Effect {
                uses: window.clone(),
                def: Some(base + 3),
                kill: Some(base),
                consumed: window,
            }
        }
        I::TailCall { proc, argc } | I::TailCallDirect { proc, argc, .. } => {
            let first = if matches!(instruction, I::TailCall { .. }) {
                0
            } else {
                1
            };
            let window: Vec<Slot> =
                range(usize::from(proc) + first, 1 - first + usize::from(argc)).collect();
            Effect {
                uses: window.clone(),
                consumed: window,
                ..Effect::default()
            }
        }
        I::Return { src } => Effect {
            uses: vec![src],
            consumed: vec![src],
            ..Effect::default()
        },
    };
    // Slot 0 is the VM's.
    effect.uses.retain(|&s| s != 0);
    effect.consumed.retain(|&s| s != 0);
    effect
}

/// A function's instructions, and how control flows between them.
struct Flow {
    /// Each instruction, with its first word and length.
    code: Vec<(usize, usize, Instruction)>,
    /// The instruction each one branches to.
    target: Vec<Option<usize>>,
    /// The basic blocks, as ranges of instructions.
    blocks: Vec<std::ops::Range<usize>>,
    /// The block each instruction starts, if it starts one.
    starts: Vec<Option<usize>>,
}

impl Flow {
    fn new(function: &Function) -> Result<Flow, String> {
        let mut code = Vec::new();
        let mut at = HashMap::new();
        let mut pc = 0;
        while pc < function.code.len() {
            let (instruction, len) =
                Instruction::decode(&function.code, pc).map_err(|e| format!("word {pc}: {e}"))?;
            at.insert(pc, code.len());
            code.push((pc, len, instruction));
            pc += len;
        }
        let target: Vec<Option<usize>> = code
            .iter()
            .map(|(pc, len, i)| {
                i.offset()
                    .map(|offset| {
                        branch_target(*pc, *len, offset)
                            .and_then(|t| at.get(&t).copied())
                            .ok_or_else(|| format!("word {pc}: a branch to no instruction"))
                    })
                    .transpose()
            })
            .collect::<Result<_, _>>()?;
        let n = code.len();
        let mut leader = vec![false; n + 1];
        leader[0] = true;
        leader[n] = true;
        for (i, (_, _, instruction)) in code.iter().enumerate() {
            if let Some(t) = target[i] {
                leader[t] = true;
            }
            if target[i].is_some() || instruction.is_terminator() {
                leader[i + 1] = true;
            }
        }
        let mut blocks = Vec::new();
        let mut starts = vec![None; n];
        let mut start = 0;
        for (i, _) in leader.iter().enumerate().skip(1).filter(|&(_, &l)| l) {
            starts[start] = Some(blocks.len());
            blocks.push(start..i);
            start = i;
        }
        Ok(Flow {
            code,
            target,
            blocks,
            starts,
        })
    }

    /// Whether execution can continue from instruction `i` to `i + 1`.
    fn falls_through(&self, i: usize) -> bool {
        i + 1 < self.code.len()
            && !self.code[i].2.is_terminator()
            && !matches!(self.code[i].2, Instruction::Jump { .. })
    }

    /// The blocks control can go to from block `b`.
    fn successors(&self, b: usize) -> impl Iterator<Item = usize> + '_ {
        let last = self.blocks[b].end - 1;
        let fall = self
            .falls_through(last)
            .then(|| self.starts[last + 1].expect("a block follows"));
        let taken = self.target[last].map(|t| self.starts[t].expect("targets start blocks"));
        fall.into_iter().chain(taken)
    }
}

/// Each block's live slots at its start.
fn liveness(flow: &Flow, slots: usize) -> Vec<Slots> {
    let mut live_in = vec![Slots::new(slots); flow.blocks.len()];
    let effects: Vec<Effect> = flow.code.iter().map(|(_, _, i)| effect(i)).collect();
    // Control only goes backward to loop heads, so sweeping the blocks in
    // reverse settles in a few passes.
    loop {
        let mut changed = false;
        for b in (0..flow.blocks.len()).rev() {
            let mut live = Slots::new(slots);
            for s in flow.successors(b) {
                live.union(&live_in[s]);
            }
            for i in flow.blocks[b].clone().rev() {
                transfer(&mut live, &effects[i]);
            }
            if live != live_in[b] {
                live_in[b] = live;
                changed = true;
            }
        }
        if !changed {
            return live_in;
        }
    }
}

/// The slots that must hold no reference as `instruction` runs: for a call,
/// those the callee's frame takes over past its arguments; for a tail call,
/// all but the procedure and arguments and the slots they move into; for a
/// return, all but the value returned. Slot 0 is the VM's.
fn barrier(instruction: &Instruction, slots: usize) -> Slots {
    use Instruction as I;
    let mut clear = Slots::new(slots);
    let keep = |clear: &mut Slots, first: usize, count: usize| {
        for s in range(first, count) {
            clear.remove(s);
        }
    };
    match *instruction {
        I::Return { src } => {
            clear = Slots::all(slots);
            clear.remove(src);
        }
        I::TailCall { proc, argc } | I::TailCallDirect { proc, argc, .. } => {
            // The procedure and arguments move into slots 0 to `argc`,
            // counting out what those held. A direct call's procedure is the
            // VM's to put in place.
            let direct = usize::from(matches!(instruction, I::TailCallDirect { .. }));
            clear = Slots::all(slots);
            keep(
                &mut clear,
                usize::from(proc) + direct,
                usize::from(argc) + 1 - direct,
            );
            keep(&mut clear, 1, usize::from(argc));
        }
        I::Call { base, argc } | I::CallDirect { base, argc, .. } => {
            // The VM counts out the header and the procedure slot, and the
            // callee takes the arguments.
            clear.extend(usize::from(base) + 4 + usize::from(argc), slots);
        }
        _ => {}
    }
    clear
}

/// Whether `instruction`'s result is never a reference: a fixnum, a
/// boolean or the like.
fn immediate(instruction: &Instruction) -> bool {
    use Instruction as I;
    matches!(
        instruction,
        I::LoadInt { .. }
            | I::Lt { .. }
            | I::Le { .. }
            | I::NumEq { .. }
            | I::Eq { .. }
            | I::Not { .. }
            | I::IsNull { .. }
            | I::IsPair { .. }
            | I::VectorLength { .. }
    )
}

/// Follows `instruction` forward over `held`, the slots that may hold a
/// reference.
fn hold(held: &mut Slots, instruction: &Instruction) {
    let e = effect(instruction);
    for &s in &e.consumed {
        held.remove(s);
    }
    if let Some(from) = e.kill {
        held.truncate(from);
    }
    if let Some(def) = e.def {
        if immediate(instruction) {
            held.remove(def);
        } else {
            held.insert(def);
        }
    }
}

/// The slots that may hold a reference as each block starts, if the block
/// is reached: at entry, the parameters.
fn holding(flow: &Flow, code: &[Instruction], arity: Arity, slots: usize) -> Vec<Option<Slots>> {
    let mut entry = Slots::new(slots);
    entry.extend(1, 1 + arity.parameter_slots());
    forward(flow, code, entry, hold)
}

/// Follows `instruction` forward over `cleared`, the slots that `move` or
/// `drop` may have cleared, which nothing may read until something writes
/// them again. A call ends what the slots from its base up held, as it
/// does in the code the pass was given.
fn clear(cleared: &mut Slots, instruction: &Instruction) {
    let e = effect(instruction);
    if let Some(from) = e.kill {
        cleared.truncate(from);
    }
    if let Some(def) = e.def {
        cleared.remove(def);
    }
    match *instruction {
        Instruction::Move { src, .. } => cleared.insert(src),
        Instruction::Drop { slot } => cleared.insert(slot),
        _ => {}
    }
}

/// A set of slots as each reached block starts, followed forward from
/// `entry` by `step` and joined by union where paths meet; `None` for a
/// block nothing reaches.
fn forward(
    flow: &Flow,
    code: &[Instruction],
    entry: Slots,
    step: fn(&mut Slots, &Instruction),
) -> Vec<Option<Slots>> {
    let mut at_start = vec![None::<Slots>; flow.blocks.len()];
    at_start[0] = Some(entry);
    let mut work = vec![0];
    while let Some(b) = work.pop() {
        let mut held = at_start[b].clone().expect("reached");
        for i in flow.blocks[b].clone() {
            step(&mut held, &code[i]);
        }
        for s in flow.successors(b) {
            let changed = match &mut at_start[s] {
                Some(known) => known.union(&held),
                none => {
                    *none = Some(held.clone());
                    true
                }
            };
            if changed {
                work.push(s);
            }
        }
    }
    at_start
}

/// The slots live before an instruction, given those live after it.
fn transfer(live: &mut Slots, effect: &Effect) {
    if let Some(from) = effect.kill {
        live.truncate(from);
    }
    if let Some(def) = effect.def {
        live.remove(def);
    }
    for &s in &effect.uses {
        live.insert(s);
    }
}

/// `function` with ownership operations placed.
fn rewrite(function: &Function) -> Result<Function, BuildError> {
    let flow = Flow::new(function).expect("the module was validated");
    let slots = usize::from(function.slots);
    let n = flow.code.len();

    // A copy from a slot whose value dies there becomes a move.
    let live_in = liveness(&flow, slots);
    let mut code: Vec<Instruction> = flow.code.iter().map(|(_, _, i)| i.clone()).collect();
    for (b, block) in flow.blocks.iter().enumerate() {
        let mut live = Slots::new(slots);
        for s in flow.successors(b) {
            live.union(&live_in[s]);
        }
        for i in block.clone().rev() {
            if let Instruction::Mov { dst, src } = code[i]
                && src != 0
                && src != dst
                && !live.contains(src)
            {
                code[i] = Instruction::Move { dst, src };
            }
            transfer(&mut live, &effect(&flow.code[i].2));
        }
    }

    // A slot that may hold a reference where a call, tail call or return
    // needs it clear is dropped right before. Nothing there reads it: the
    // value is dead.
    let held_in = holding(&flow, &code, function.arity, slots);
    let mut drops = vec![Vec::new(); n];
    for (b, block) in flow.blocks.iter().enumerate() {
        let Some(mut held) = held_in[b].clone() else {
            continue;
        };
        for i in block.clone() {
            drops[i] = held.within(&barrier(&code[i], slots));
            for &s in &drops[i] {
                held.remove(s);
            }
            hold(&mut held, &code[i]);
        }
    }

    let mut b = FunctionBuilder::new(
        function.name.as_deref(),
        function.arity,
        function.slots,
        function.free,
    );
    for copy in &function.inlined {
        b.inline(copy.clone());
    }
    let mut labels: Vec<Option<Label>> = vec![None; n];
    for &t in flow.target.iter().flatten() {
        if labels[t].is_none() {
            labels[t] = Some(b.label());
        }
    }
    let mut positions = function.positions.iter().peekable();
    let mut scopes = function.scopes.iter().peekable();
    for (i, &(pc, len, _)) in flow.code.iter().enumerate() {
        if let Some(label) = labels[i] {
            b.bind(label);
        }
        for &slot in &drops[i] {
            b.emit(Instruction::Drop { slot });
        }
        let end = (pc + len) as u32;
        while let Some(p) = positions.next_if(|p| p.pc < end) {
            b.position(p.source, p.line, p.column);
        }
        while let Some(s) = scopes.next_if(|s| s.pc < end) {
            b.scope(s.inlined);
        }
        match flow.target[i] {
            Some(t) => {
                let target = labels[t].expect("targets have labels");
                if let Instruction::Jump { .. } = code[i] {
                    b.jump(target);
                } else {
                    b.branch(code[i].clone(), target);
                }
            }
            None => {
                b.emit(code[i].clone());
            }
        }
    }
    b.finish()
}

/// How rewritten code breaks the contract the VM relies on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractError {
    /// The index of the instruction's first word.
    pub pc: usize,
    pub message: String,
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "word {}: {}", self.pc, self.message)
    }
}

impl std::error::Error for ContractError {}

/// Checks that `function`, as [`insert`] rewrote it, keeps the contract the
/// VM relies on (see the module documentation), and reads no slot that a
/// `move` or `drop` may have cleared, on every path: which slots may hold a
/// reference is followed forward from the parameters, and which may be
/// cleared from none.
pub fn check(function: &Function) -> Result<(), ContractError> {
    let flow = Flow::new(function).map_err(|message| ContractError { pc: 0, message })?;
    let slots = usize::from(function.slots);
    let code: Vec<Instruction> = flow.code.iter().map(|(_, _, i)| i.clone()).collect();
    let held_in = holding(&flow, &code, function.arity, slots);
    let cleared_in = forward(&flow, &code, Slots::new(slots), clear);
    for (b, block) in flow.blocks.iter().enumerate() {
        let (Some(mut held), Some(mut cleared)) = (held_in[b].clone(), cleared_in[b].clone())
        else {
            continue;
        };
        for i in block.clone() {
            let fail = |message: String| {
                Err(ContractError {
                    pc: flow.code[i].0,
                    message,
                })
            };
            let mnemonic = code[i].opcode().mnemonic();
            // Leaving a reference where it must not be leaks it.
            if let Some(s) = held.within(&barrier(&code[i], slots)).first() {
                return fail(format!("r{s} may hold a reference at {mnemonic}"));
            }
            // Reading a slot the pass cleared would read the wrong value.
            if let Some(s) = effect(&code[i]).uses.iter().find(|&&s| cleared.contains(s)) {
                return fail(format!(
                    "{mnemonic} reads r{s}, which may have been cleared"
                ));
            }
            hold(&mut held, &code[i]);
            clear(&mut cleared, &code[i]);
        }
    }
    Ok(())
}
