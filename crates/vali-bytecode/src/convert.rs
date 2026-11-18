//! Converting the distribution format's IR to run-format bytecode
//! (ADR 0017 #2).
//!
//! Each function is converted in three steps, none of which recurses:
//!
//! 1. **Linearise:** the terms in the order their code is emitted. A join
//!    point's body comes before its handler, and an `if`'s `then` before
//!    its `otherwise`, so control only moves backward in the order by
//!    jumping to a `rec join`.
//! 2. **Allocate:** each variable's live interval runs from its binding to
//!    its last use, and a variable bound outside a loop and used inside it
//!    lives to the loop's end. Slots are assigned by linear scan, twice: the
//!    second pass hints each call's operands and result into the slots its
//!    window will use, as the first pass placed it.
//! 3. **Emit:** a call's window goes above every slot live across it, since
//!    the callee's frame overlays everything from there up; operands move
//!    into place with parallel moves. Free variables and constants are not
//!    allocated: each use loads them into a slot of their own.
//!
//! A comparison bound to a variable that only an `if` tests becomes one
//! branch instruction.

use std::collections::HashMap;
use std::fmt;

use vali_dist as ir;

use crate::{
    Arity, Constant, Function, FunctionBuilder, Inlined, Instruction as I, Label, Module,
    ModuleBuilder, Slot,
};

/// Why a module could not be converted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConvertError {
    /// The function the problem is in, if it is in one.
    pub function: Option<u32>,
    pub message: String,
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.function {
            Some(n) => write!(f, "function f{n}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ConvertError {}

type Result<T> = std::result::Result<T, String>;

/// The highest slot a frame can address.
const TOP: usize = u16::MAX as usize - 1;

/// Converts `module`, which is first validated.
pub fn convert(module: &ir::Module) -> std::result::Result<Module, ConvertError> {
    let fail = |message: String| ConvertError {
        function: None,
        message,
    };
    ir::validate(module).map_err(|e| fail(e.to_string()))?;
    let limit = usize::from(u16::MAX) + 1;
    if module.constants.len() > limit
        || module.globals.len() > limit
        || module.sources.len() > limit
    {
        return Err(fail(
            "more than 65,536 constants, variables or sources".into(),
        ));
    }
    let mut b = ModuleBuilder::new(&module.name);
    let constants: Vec<u16> = module
        .constants
        .iter()
        .map(|c| b.constant(constant(c)))
        .collect();
    let variables: Vec<u16> = module
        .globals
        .iter()
        .map(|g| match &g.link {
            ir::GlobalLink::Defined => b.defined(&g.name),
            ir::GlobalLink::Imported(library) => b.imported(library, &g.name),
            ir::GlobalLink::Primitive => b.primitive(&g.name),
            ir::GlobalLink::Native(library) => b.native(library, &g.name),
            ir::GlobalLink::Repl => b.variable(&g.name),
        })
        .collect();
    let sources: Vec<u16> = module.sources.iter().map(|s| b.source(s)).collect();
    let unspecified = b.constant(Constant::Unspecified);
    let indexes: Vec<u32> = module.functions.iter().map(|_| b.declare()).collect();
    let tables = Tables {
        module,
        constants: &constants,
        variables: &variables,
        sources: &sources,
        unspecified,
    };
    for (n, function) in module.functions.iter().enumerate() {
        let converted = convert_function(&tables, function).map_err(|message| ConvertError {
            function: Some(n as u32),
            message,
        })?;
        b.define(indexes[n], converted);
    }
    for (name, global) in &module.exports {
        b.export(name, variables[global.0 as usize]);
    }
    Ok(b.finish(indexes[module.body.0 as usize]))
}

/// A constant as run format's, built iteratively.
fn constant(c: &ir::Constant) -> Constant {
    enum Task<'a> {
        Visit(&'a ir::Constant),
        Pair,
        Ratio,
        Complex,
        Vector(usize),
    }
    let mut tasks = vec![Task::Visit(c)];
    let mut out: Vec<Constant> = Vec::new();
    let two = |out: &mut Vec<Constant>| {
        let b = out.pop().expect("a part");
        let a = out.pop().expect("a part");
        (Box::new(a), Box::new(b))
    };
    while let Some(task) = tasks.pop() {
        match task {
            Task::Visit(c) => match c {
                ir::Constant::Integer(n) => out.push(Constant::Integer(*n)),
                ir::Constant::Bignum { negative, limbs } => out.push(Constant::Bignum {
                    negative: *negative,
                    limbs: limbs.clone(),
                }),
                ir::Constant::Flonum(x) => out.push(Constant::Flonum(*x)),
                ir::Constant::Boolean(x) => out.push(Constant::Boolean(*x)),
                ir::Constant::Character(x) => out.push(Constant::Character(*x)),
                ir::Constant::EmptyList => out.push(Constant::EmptyList),
                ir::Constant::Unspecified => out.push(Constant::Unspecified),
                ir::Constant::String(s) => out.push(Constant::String(s.clone())),
                ir::Constant::Symbol(s) => out.push(Constant::Symbol(s.clone())),
                ir::Constant::Keyword(s) => out.push(Constant::Keyword(s.clone())),
                ir::Constant::Bytevector(bytes) => out.push(Constant::Bytevector(bytes.clone())),
                ir::Constant::Pair(a, b)
                | ir::Constant::Ratio(a, b)
                | ir::Constant::Complex(a, b) => {
                    tasks.push(match c {
                        ir::Constant::Pair(..) => Task::Pair,
                        ir::Constant::Ratio(..) => Task::Ratio,
                        _ => Task::Complex,
                    });
                    tasks.push(Task::Visit(b));
                    tasks.push(Task::Visit(a));
                }
                ir::Constant::Vector(items) => {
                    tasks.push(Task::Vector(items.len()));
                    tasks.extend(items.iter().rev().map(Task::Visit));
                }
            },
            Task::Pair => {
                let (a, b) = two(&mut out);
                out.push(Constant::Pair(a, b));
            }
            Task::Ratio => {
                let (a, b) = two(&mut out);
                out.push(Constant::Ratio(a, b));
            }
            Task::Complex => {
                let (a, b) = two(&mut out);
                out.push(Constant::Complex(a, b));
            }
            Task::Vector(n) => {
                let items = out.split_off(out.len() - n);
                out.push(Constant::Vector(items));
            }
        }
    }
    out.pop().expect("a constant")
}

/// The module-wide tables a function's code indexes.
struct Tables<'a> {
    module: &'a ir::Module,
    constants: &'a [u16],
    variables: &'a [u16],
    sources: &'a [u16],
    /// The unspecified value's constant.
    unspecified: u16,
}

/// One step of a function's code, in emission order.
#[derive(Clone, Copy, Debug)]
enum Event {
    /// The function starts: its parameters are bound here, before any
    /// term, so they are live across the first.
    Entry,
    Term(ir::TermId),
    /// A join point's handler starts.
    Bind(ir::JoinId),
    /// An `if`'s `otherwise` starts.
    Else(ir::TermId),
    /// A join point's handler has ended.
    End(ir::JoinId),
}

/// The terms of `f` in emission order.
fn linearise(f: &ir::Function) -> Vec<Event> {
    let mut events = vec![Event::Entry];
    let mut pending = vec![Event::Term(f.body)];
    while let Some(event) = pending.pop() {
        events.push(event);
        let Event::Term(id) = event else {
            continue;
        };
        match f.term(id) {
            ir::Term::Let { body, .. } => pending.push(Event::Term(*body)),
            ir::Term::Join {
                join,
                handler,
                body,
                ..
            } => {
                pending.push(Event::End(*join));
                pending.push(Event::Term(*handler));
                pending.push(Event::Bind(*join));
                pending.push(Event::Term(*body));
            }
            ir::Term::If {
                then, otherwise, ..
            } => {
                pending.push(Event::Term(*otherwise));
                pending.push(Event::Else(id));
                pending.push(Event::Term(*then));
            }
            ir::Term::Jump { .. }
            | ir::Term::Return(_)
            | ir::Term::TailCall { .. }
            | ir::Term::TailCallDirect { .. } => {}
        }
    }
    events
}

/// The branch a comparison becomes when an `if` is its only use.
fn fusible(op: ir::Primitive) -> bool {
    matches!(
        op,
        ir::Primitive::Lt
            | ir::Primitive::Le
            | ir::Primitive::NumEq
            | ir::Primitive::Eq
            | ir::Primitive::IsNull
            | ir::Primitive::IsPair
    )
}

fn value_operands(value: &ir::Value) -> Vec<ir::Operand> {
    match value {
        ir::Value::Operand(x) | ir::Value::Box(x) | ir::Value::Unbox(x) => vec![*x],
        ir::Value::SetGlobal(_, x) => vec![*x],
        ir::Value::Primitive { args, .. } => args.clone(),
        ir::Value::Call { callee, args } => std::iter::once(*callee)
            .chain(args.iter().copied())
            .collect(),
        ir::Value::CallDirect { args, .. } => args.clone(),
        ir::Value::Closure { free, .. } => free.clone(),
        ir::Value::Global(_) => Vec::new(),
        ir::Value::SetBox(a, b) => vec![*a, *b],
        ir::Value::SetFree { closure, value, .. } => vec![*closure, *value],
    }
}

/// What a term reads.
fn term_operands(term: &ir::Term) -> Vec<ir::Operand> {
    match term {
        ir::Term::Let { value, .. } => value_operands(value),
        ir::Term::Join { .. } => Vec::new(),
        ir::Term::Jump { args, .. } => args.clone(),
        ir::Term::If { test, .. } => vec![*test],
        ir::Term::Return(x) => vec![*x],
        ir::Term::TailCall { callee, args, .. } => std::iter::once(*callee)
            .chain(args.iter().copied())
            .collect(),
        ir::Term::TailCallDirect { args, .. } => args.clone(),
    }
}

/// A call's window from the callee's slot up: the callee, which a direct
/// call leaves to the VM, then the arguments; and whether the call returns
/// here, not being a tail call.
fn window(term: &ir::Term) -> Option<(Vec<Option<ir::Operand>>, bool)> {
    let (callee, args, returns) = match term {
        ir::Term::Let {
            value: ir::Value::Call { callee, args },
            ..
        } => (Some(*callee), args, true),
        ir::Term::Let {
            value: ir::Value::CallDirect { args, .. },
            ..
        } => (None, args, true),
        ir::Term::TailCall { callee, args, .. } => (Some(*callee), args, false),
        ir::Term::TailCallDirect { args, .. } => (None, args, false),
        _ => return None,
    };
    let operands = std::iter::once(callee)
        .chain(args.iter().copied().map(Some))
        .collect();
    Some((operands, returns))
}

/// What the allocator knows of one function.
struct Analysis {
    events: Vec<Event>,
    /// Each variable's interval, as positions in `events`.
    def: Vec<u32>,
    end: Vec<u32>,
    /// Whether each variable needs a slot: free variables and fused
    /// comparisons do not.
    needs_slot: Vec<bool>,
    /// How many times each variable is used.
    uses: Vec<u32>,
    /// The `let`s whose comparison an `if` right after them branches on.
    fused: Vec<bool>,
    /// Each function parameter's fixed slot.
    fixed: Vec<Option<Slot>>,
    /// The variables whose last use is as an operand of a call or tail
    /// call: they are computed into its window.
    call_operand: Vec<bool>,
    /// Where each call's operands start being computed: the earliest
    /// binding of one of them, or the call itself; and whether it returns
    /// here, not being a tail call.
    region: HashMap<usize, (usize, bool)>,
    /// Each join point's parameters.
    join_params: Vec<Vec<ir::Var>>,
}

fn analyse(f: &ir::Function) -> Result<Analysis> {
    let events = linearise(f);
    if events.len() >= u32::MAX as usize {
        return Err("a function is too long".into());
    }
    let n = f.vars as usize;
    let mut uses = vec![0u32; n];
    let mut fused = vec![false; f.terms.len()];
    for event in &events {
        if let Event::Term(id) = event {
            for x in term_operands(f.term(*id)) {
                if let ir::Operand::Var(v) = x {
                    uses[v.0 as usize] += 1;
                }
            }
        }
    }
    let mut def = vec![0u32; n];
    let mut end = vec![0u32; n];
    let mut needs_slot = vec![true; n];
    for v in &f.free {
        needs_slot[v.0 as usize] = false;
    }
    let mut join_params = vec![Vec::new(); f.joins as usize];
    let mut regions = Vec::new();
    let mut starts = vec![0u32; f.joins as usize];
    let mut recursive = vec![false; f.joins as usize];
    // Uses, as (variable, position) pairs.
    let mut used_at: Vec<(ir::Var, u32)> = Vec::new();
    for (p, event) in events.iter().enumerate() {
        let p = p as u32;
        match *event {
            Event::Term(id) => {
                let term = f.term(id);
                if let ir::Term::Let {
                    var,
                    value: ir::Value::Primitive { op, args },
                    body,
                    ..
                } = term
                    && fusible(*op)
                    && uses[var.0 as usize] == 1
                    && matches!(f.term(*body), ir::Term::If { test: ir::Operand::Var(t), .. } if t == var)
                {
                    // The comparison is emitted at the `if`, after it.
                    fused[id.0 as usize] = true;
                    needs_slot[var.0 as usize] = false;
                    def[var.0 as usize] = p;
                    end[var.0 as usize] = p;
                    for x in args {
                        if let ir::Operand::Var(v) = x {
                            used_at.push((*v, p + 1));
                        }
                    }
                    continue;
                }
                if let ir::Term::Let { var, .. } = term {
                    def[var.0 as usize] = p;
                    end[var.0 as usize] = p;
                }
                let skip_test = matches!(term, ir::Term::If { test: ir::Operand::Var(t), .. }
                    if !needs_slot[t.0 as usize] && !f.free.contains(t));
                if skip_test {
                    continue;
                }
                for x in term_operands(term) {
                    if let ir::Operand::Var(v) = x {
                        used_at.push((v, p));
                    }
                }
                if let ir::Term::Join {
                    join,
                    params,
                    recursive: rec,
                    ..
                } = term
                {
                    join_params[join.0 as usize].clone_from(params);
                    recursive[join.0 as usize] = *rec;
                }
            }
            Event::Bind(join) => {
                starts[join.0 as usize] = p;
                for v in &join_params[join.0 as usize] {
                    // At least one position, so parameters bound together
                    // never share a slot.
                    def[v.0 as usize] = p;
                    end[v.0 as usize] = p + 1;
                }
            }
            Event::End(join) => {
                if recursive[join.0 as usize] {
                    regions.push((starts[join.0 as usize], p));
                }
            }
            Event::Else(_) | Event::Entry => {}
        }
    }
    for &(v, p) in &used_at {
        let v = v.0 as usize;
        end[v] = end[v].max(p);
        for &(start, stop) in &regions {
            if def[v] < start && start < p && p <= stop {
                end[v] = end[v].max(stop);
            }
        }
    }
    let mut fixed = vec![None; n];
    for (i, v) in f.params.iter().enumerate() {
        let slot = u16::try_from(i + 1).map_err(|_| "too many parameters".to_owned())?;
        fixed[v.0 as usize] = Some(slot);
        def[v.0 as usize] = 0;
    }
    let mut call_operand = vec![false; n];
    let mut region = HashMap::new();
    for (p, event) in events.iter().enumerate() {
        let Event::Term(id) = event else {
            continue;
        };
        let term = f.term(*id);
        let Some((operands, returns)) = window(term) else {
            continue;
        };
        let mut start = p;
        for x in operands.into_iter().flatten() {
            if let ir::Operand::Var(v) = x {
                let v = v.0 as usize;
                if end[v] as usize == p && fixed[v].is_none() && needs_slot[v] {
                    call_operand[v] = true;
                    start = start.min(def[v] as usize);
                }
            }
        }
        region.insert(p, (start, returns));
    }
    Ok(Analysis {
        events,
        def,
        end,
        needs_slot,
        uses,
        fused,
        fixed,
        call_operand,
        region,
        join_params,
    })
}

/// Assigns slots by linear scan, preferring each variable's hint.
fn allocate(a: &Analysis, hints: &HashMap<usize, Slot>) -> Result<Vec<Option<Slot>>> {
    let n = a.def.len();
    let mut slots = vec![None; n];
    // When each slot becomes free again; slot 0 is the closure's. It grows
    // as slots are first used, so a small function stays small to scan.
    let mut busy: Vec<u32> = vec![u32::MAX];
    let grow = |busy: &mut Vec<u32>, slot: usize| {
        if busy.len() <= slot {
            busy.resize(slot + 1, 0);
        }
    };
    for (v, fixed) in a.fixed.iter().enumerate() {
        if let Some(slot) = *fixed {
            slots[v] = Some(slot);
            let slot = usize::from(slot);
            grow(&mut busy, slot);
            busy[slot] = busy[slot].max(a.end[v].max(1));
        }
    }
    let mut order: Vec<usize> = (0..n)
        .filter(|&v| a.needs_slot[v] && a.fixed[v].is_none())
        .collect();
    order.sort_by_key(|&v| (a.def[v], v));
    for v in order {
        let start = a.def[v];
        let free = |slot: usize| slot > 0 && busy.get(slot).is_none_or(|&b| b <= start);
        let slot = match hints.get(&v) {
            Some(&h) if free(usize::from(h)) => usize::from(h),
            _ => (1..=TOP)
                .find(|&s| free(s))
                .ok_or_else(|| format!("a function needs more than {} slots", TOP + 1))?,
        };
        grow(&mut busy, slot);
        busy[slot] = a.end[v];
        slots[v] = Some(slot as u16);
    }
    Ok(slots)
}

/// Each call's window: above every slot in use while its operands are
/// computed, except those operands' own, and for a call that returns,
/// above every slot live across it.
fn bases(a: &Analysis, slots: &[Option<Slot>]) -> HashMap<usize, usize> {
    let (_, across) = pressure(a, slots, |_| true);
    let (others, _) = pressure(a, slots, |v| !a.call_operand[v]);
    a.region
        .iter()
        .map(|(&p, &(start, returns))| {
            // Up to the call, not at it: what is bound there is its result,
            // and what is last used there is read before the window fills.
            let busy = others[start..p].iter().copied().max().unwrap_or(0);
            let floor = if returns { busy.max(across[p]) } else { busy };
            (p, floor.max(0) as usize + 1)
        })
        .collect()
}

/// For each position, the highest slot of a variable live there, and of
/// one live across it: bound before and used after. Only variables
/// `counted` keeps count.
fn pressure(
    a: &Analysis,
    slots: &[Option<Slot>],
    counted: impl Fn(usize) -> bool,
) -> (Vec<i32>, Vec<i32>) {
    let len = a.events.len();
    let mut starting: Vec<Vec<usize>> = vec![Vec::new(); len + 1];
    let mut ending: Vec<Vec<usize>> = vec![Vec::new(); len + 1];
    for (v, slot) in slots.iter().enumerate() {
        if slot.is_some() && counted(v) {
            starting[a.def[v] as usize].push(v);
            ending[(a.end[v] as usize).min(len)].push(v);
        }
    }
    let top = slots
        .iter()
        .flatten()
        .map(|&s| usize::from(s))
        .max()
        .unwrap_or(0);
    let mut count = vec![0u32; top + 1];
    let max = |count: &[u32]| {
        (0..count.len())
            .rev()
            .find(|&s| count[s] > 0)
            .map_or(0, |s| s as i32)
    };
    let mut cover = vec![0; len];
    let mut across = vec![0; len];
    let slot = |v: usize| usize::from(slots[v].expect("allocated"));
    for p in 0..len {
        for &v in &starting[p] {
            count[slot(v)] += 1;
        }
        cover[p] = max(&count);
        // Those bound or last used here are not live across it.
        let edges: Vec<usize> = starting[p]
            .iter()
            .copied()
            .chain(
                ending[p]
                    .iter()
                    .copied()
                    .filter(|&v| a.def[v] as usize != p),
            )
            .collect();
        for &v in &edges {
            count[slot(v)] -= 1;
        }
        across[p] = max(&count);
        for &v in &edges {
            count[slot(v)] += 1;
        }
        for &v in &ending[p] {
            count[slot(v)] -= 1;
        }
    }
    (cover, across)
}

/// Slots for `f`'s variables, and the pressure they give. Each pass hints
/// each call's operands that end there, and its result, into its window
/// as the last pass placed it, and a tail call's into slots 1 and up,
/// until the hints settle.
type Allocation = (Analysis, Vec<Option<Slot>>, Vec<i32>, HashMap<usize, usize>);

fn allocation(f: &ir::Function) -> Result<Allocation> {
    let a = analyse(f)?;
    let mut hints = HashMap::new();
    let mut slots = allocate(&a, &hints)?;
    for _ in 0..4 {
        let next = call_hints(f, &a, &bases(&a, &slots));
        if next == hints {
            break;
        }
        hints = next;
        slots = allocate(&a, &hints)?;
    }
    let (cover, _) = pressure(&a, &slots, |_| true);
    let bases = bases(&a, &slots);
    Ok((a, slots, cover, bases))
}

fn call_hints(
    f: &ir::Function,
    a: &Analysis,
    bases: &HashMap<usize, usize>,
) -> HashMap<usize, Slot> {
    let mut hints = HashMap::new();
    for (p, event) in a.events.iter().enumerate() {
        let Event::Term(id) = event else {
            continue;
        };
        let term = f.term(*id);
        let Some((operands, returns)) = window(term) else {
            continue;
        };
        let base = if returns { bases[&p] + 3 } else { bases[&p] };
        if let ir::Term::Let { var, .. } = term
            && let Ok(result) = u16::try_from(base)
        {
            hints.insert(var.0 as usize, result);
        }
        for (i, x) in operands.into_iter().enumerate() {
            if let Some(ir::Operand::Var(v)) = x
                && a.call_operand[v.0 as usize]
                && a.end[v.0 as usize] as usize == p
                && let Ok(slot) = u16::try_from(base + i)
            {
                hints.entry(v.0 as usize).or_insert(slot);
            }
        }
    }
    hints
}

fn convert_function(tables: &Tables<'_>, f: &ir::Function) -> Result<Function> {
    let (a, slots, cover, bases) = allocation(f)?;
    let arity = Arity {
        required: f.arity.required,
        optional: f.arity.optional,
        rest: f.arity.rest,
    };
    let free = u16::try_from(f.free.len()).map_err(|_| "too many free variables".to_owned())?;
    let mut free_index = vec![None; f.vars as usize];
    for (i, v) in f.free.iter().enumerate() {
        free_index[v.0 as usize] = Some(i as u16);
    }
    let mut e = Emitter {
        tables,
        f,
        a: &a,
        slots: &slots,
        free_index,
        cover: &cover,
        bases: &bases,
        b: FunctionBuilder::new(f.name.as_deref(), arity, 0, free),
        joins: Vec::new(),
        elses: HashMap::new(),
        top: slots
            .iter()
            .flatten()
            .map(|&s| usize::from(s))
            .max()
            .unwrap_or(0)
            .max(f.params.len()),
        scratch: 0,
    };
    e.joins = (0..f.joins).map(|_| e.b.label()).collect();
    for copy in &f.inlined {
        e.b.inline(Inlined {
            name: copy.name.clone(),
            call: copy
                .call
                .map(|p| (tables.sources[usize::from(p.source)], p.line, p.column)),
            caller: copy.call.map_or(0, |p| p.inlined),
        });
    }
    e.run()?;
    let top = e.top;
    let mut function = e.b.finish().map_err(|e| e.to_string())?;
    function.slots = u16::try_from(top + 1).map_err(|_| "too many slots".to_owned())?;
    Ok(function)
}

struct Emitter<'a> {
    tables: &'a Tables<'a>,
    f: &'a ir::Function,
    a: &'a Analysis,
    slots: &'a [Option<Slot>],
    free_index: Vec<Option<Slot>>,
    cover: &'a [i32],
    /// Each call's window, by position.
    bases: &'a HashMap<usize, usize>,
    b: FunctionBuilder,
    joins: Vec<Label>,
    elses: HashMap<ir::TermId, Label>,
    /// The highest slot used.
    top: usize,
    /// The next slot for loading a free variable or constant.
    scratch: usize,
}

impl Emitter<'_> {
    fn touch(&mut self, slot: usize) -> Result<Slot> {
        if slot > TOP {
            return Err("a function needs more than 256 slots".into());
        }
        self.top = self.top.max(slot);
        Ok(slot as u16)
    }

    fn slot(&self, v: ir::Var) -> Option<Slot> {
        self.slots[v.0 as usize]
    }

    /// Loads constant `c` into `dst`.
    fn load(&mut self, dst: Slot, c: ir::ConstantId) {
        match &self.tables.module.constants[c.0 as usize] {
            ir::Constant::Integer(n) if i16::try_from(*n).is_ok() => {
                self.b.emit(I::LoadInt {
                    dst,
                    value: *n as i16,
                });
            }
            _ => {
                let constant = self.tables.constants[c.0 as usize];
                self.b.emit(I::LoadConstant { dst, constant });
            }
        }
    }

    /// Puts `x`, which has no slot of its own, into `dst`.
    fn materialise(&mut self, dst: Slot, x: ir::Operand) -> Result<()> {
        match x {
            ir::Operand::Const(c) => self.load(dst, c),
            ir::Operand::Var(v) => match self.free_index[v.0 as usize] {
                Some(index) => {
                    self.b.emit(I::Free { dst, index });
                }
                None => match self.slot(v) {
                    Some(src) if src != dst => {
                        self.b.emit(I::Mov { dst, src });
                    }
                    Some(_) => {}
                    None => return Err(format!("v{} has no slot", v.0)),
                },
            },
        }
        Ok(())
    }

    /// A slot holding `x`: its own, or a scratch slot it is loaded into.
    fn read(&mut self, x: ir::Operand) -> Result<Slot> {
        if let ir::Operand::Var(v) = x
            && let Some(slot) = self.slot(v)
        {
            return Ok(slot);
        }
        self.scratch += 1;
        let dst = self.touch(self.scratch)?;
        self.materialise(dst, x)?;
        Ok(dst)
    }

    /// A small integer constant fitting an 8-bit immediate.
    fn immediate(&self, x: ir::Operand) -> Option<i8> {
        match x {
            ir::Operand::Const(c) => match self.tables.module.constants[c.0 as usize] {
                ir::Constant::Integer(n) => i8::try_from(n).ok(),
                _ => None,
            },
            ir::Operand::Var(_) => None,
        }
    }

    /// Puts each operand into its target slot: those in slots by parallel
    /// moves, then the others.
    fn place(&mut self, targets: &[(Slot, ir::Operand)]) -> Result<()> {
        let mut moves: Vec<(Slot, Slot)> = Vec::new();
        for &(dst, x) in targets {
            if let ir::Operand::Var(v) = x
                && let Some(src) = self.slot(v)
                && src != dst
            {
                moves.push((dst, src));
            }
        }
        let highest = moves
            .iter()
            .map(|&(d, s)| usize::from(d.max(s)))
            .max()
            .unwrap_or(0);
        let mut scratch = None;
        while !moves.is_empty() {
            match moves
                .iter()
                .position(|&(dst, _)| !moves.iter().any(|&(_, src)| src == dst))
            {
                Some(i) => {
                    let (dst, src) = moves.remove(i);
                    self.b.emit(I::Mov { dst, src });
                }
                None => {
                    // A cycle: park one source in a free slot.
                    let temp = match scratch {
                        Some(temp) => temp,
                        None => {
                            let temp = self.touch(self.scratch.max(highest) + 1)?;
                            scratch = Some(temp);
                            temp
                        }
                    };
                    let src = moves[0].1;
                    self.b.emit(I::Mov { dst: temp, src });
                    for m in &mut moves {
                        if m.1 == src {
                            m.1 = temp;
                        }
                    }
                }
            }
        }
        for &(dst, x) in targets {
            let in_slot = matches!(x, ir::Operand::Var(v) if self.slot(v).is_some());
            if !in_slot {
                self.materialise(dst, x)?;
            }
        }
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        let events = &self.a.events;
        for (p, event) in events.iter().enumerate() {
            self.scratch = (self.cover[p].max(0) as usize).max(self.f.params.len());
            match *event {
                Event::Term(id) => self.term(id, p)?,
                Event::Bind(join) => self.b.bind(self.joins[join.0 as usize]),
                Event::Else(id) => {
                    let label = self.elses[&id];
                    self.b.bind(label);
                }
                Event::End(_) | Event::Entry => {}
            }
        }
        Ok(())
    }

    fn position(&mut self, position: Option<ir::Position>) {
        if let Some(position) = position {
            let source = self.tables.sources[usize::from(position.source)];
            self.b.position(source, position.line, position.column);
            self.b.scope(position.inlined);
        }
    }

    fn term(&mut self, id: ir::TermId, p: usize) -> Result<()> {
        match self.f.term(id) {
            ir::Term::Let {
                var,
                value,
                position,
                ..
            } => {
                self.position(*position);
                if !self.a.fused[id.0 as usize] {
                    self.value(*var, value, p)?;
                }
            }
            ir::Term::Join { .. }
            | ir::Term::Return(_)
            | ir::Term::TailCall { .. }
            | ir::Term::TailCallDirect { .. } => {
                self.tail(id, p)?;
            }
            ir::Term::Jump { join, args } => {
                let params = &self.a.join_params[join.0 as usize];
                let mut targets = Vec::new();
                for (param, &x) in params.iter().zip(args) {
                    let slot = self.slot(*param).ok_or("a join parameter has no slot")?;
                    targets.push((slot, x));
                }
                self.place(&targets)?;
                let next = self.a.events.get(p + 1);
                if !matches!(next, Some(Event::Bind(j)) if j == join) {
                    self.b.jump(self.joins[join.0 as usize]);
                }
            }
            ir::Term::If { test, .. } => self.branch(id, *test, p)?,
        }
        Ok(())
    }

    /// `return`, a tail call, or a join point, which emits nothing itself.
    fn tail(&mut self, id: ir::TermId, p: usize) -> Result<()> {
        match self.f.term(id) {
            ir::Term::Return(x) => {
                let src = self.read(*x)?;
                self.b.emit(I::Return { src });
            }
            ir::Term::TailCall {
                callee,
                args,
                position,
            } => {
                self.position(*position);
                let argc =
                    u16::try_from(args.len()).map_err(|_| "too many arguments".to_owned())?;
                let proc = self.touch(self.bases[&p])?;
                let mut targets = vec![(proc, *callee)];
                for (i, &x) in args.iter().enumerate() {
                    targets.push((self.touch(usize::from(proc) + 1 + i)?, x));
                }
                self.scratch = self.scratch.max(usize::from(proc) + args.len());
                self.place(&targets)?;
                self.b.emit(I::TailCall { proc, argc });
            }
            ir::Term::TailCallDirect {
                function,
                args,
                position,
            } => {
                self.position(*position);
                let argc =
                    u16::try_from(args.len()).map_err(|_| "too many arguments".to_owned())?;
                let proc = self.touch(self.bases[&p])?;
                let mut targets = Vec::new();
                for (i, &x) in args.iter().enumerate() {
                    targets.push((self.touch(usize::from(proc) + 1 + i)?, x));
                }
                self.scratch = self.scratch.max(usize::from(proc) + args.len());
                self.place(&targets)?;
                self.b.emit(I::TailCallDirect {
                    proc,
                    argc,
                    function: function.0,
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// An `if`: a branch to its `otherwise`, fused with the comparison
    /// before it if that is all the comparison is for.
    fn branch(&mut self, id: ir::TermId, test: ir::Operand, p: usize) -> Result<()> {
        let otherwise = self.b.label();
        self.elses.insert(id, otherwise);
        let fused = match (test, p.checked_sub(1).map(|q| self.a.events[q])) {
            (ir::Operand::Var(v), Some(Event::Term(at)))
                if self.slot(v).is_none() && self.free_index[v.0 as usize].is_none() =>
            {
                self.comparison(at)
            }
            _ => None,
        };
        let instruction = match fused {
            Some((op, args)) => match (op, &args[..]) {
                (ir::Primitive::Lt, &[x, y]) => match self.immediate(y) {
                    Some(imm) => I::IfLtImm {
                        a: self.read(x)?,
                        imm,
                        offset: 0,
                    },
                    None => I::IfLt {
                        a: self.read(x)?,
                        b: self.read(y)?,
                        offset: 0,
                    },
                },
                (ir::Primitive::Le, &[x, y]) => I::IfLe {
                    a: self.read(x)?,
                    b: self.read(y)?,
                    offset: 0,
                },
                (ir::Primitive::NumEq, &[x, y]) => I::IfNumEq {
                    a: self.read(x)?,
                    b: self.read(y)?,
                    offset: 0,
                },
                (ir::Primitive::Eq, &[x, y]) => I::IfEq {
                    a: self.read(x)?,
                    b: self.read(y)?,
                    offset: 0,
                },
                (ir::Primitive::IsNull, &[x]) => I::IfNull {
                    a: self.read(x)?,
                    offset: 0,
                },
                (ir::Primitive::IsPair, &[x]) => I::IfPair {
                    a: self.read(x)?,
                    offset: 0,
                },
                _ => return Err(format!("{op} cannot be fused")),
            },
            None => I::If {
                test: self.read(test)?,
                offset: 0,
            },
        };
        self.b.branch(instruction, otherwise);
        Ok(())
    }

    /// The comparison of the fused `let` `id`, which comes right before
    /// the `if` that tests it.
    fn comparison(&self, id: ir::TermId) -> Option<(ir::Primitive, Vec<ir::Operand>)> {
        match self.f.term(id) {
            ir::Term::Let {
                value: ir::Value::Primitive { op, args },
                ..
            } if self.a.fused[id.0 as usize] => Some((*op, args.clone())),
            _ => None,
        }
    }

    fn value(&mut self, var: ir::Var, value: &ir::Value, p: usize) -> Result<()> {
        let dst = self.slot(var).ok_or("a let has no slot")?;
        let used = self.a.uses[var.0 as usize] > 0;
        match value {
            ir::Value::Operand(x) => self.materialise(dst, *x)?,
            ir::Value::Primitive { op, args } => self.primitive(dst, *op, args, used)?,
            ir::Value::Call { callee, args } => {
                let base = self.touch(self.bases[&p])?;
                let argc =
                    u16::try_from(args.len()).map_err(|_| "too many arguments".to_owned())?;
                let mut targets = vec![(self.touch(usize::from(base) + 3)?, *callee)];
                for (i, &x) in args.iter().enumerate() {
                    targets.push((self.touch(usize::from(base) + 4 + i)?, x));
                }
                self.scratch = self.scratch.max(usize::from(base) + 3 + args.len());
                self.place(&targets)?;
                self.b.emit(I::Call { base, argc });
                if dst != base + 3 {
                    self.b.emit(I::Mov { dst, src: base + 3 });
                }
            }
            ir::Value::CallDirect { function, args } => {
                let base = self.touch(self.bases[&p])?;
                let argc =
                    u16::try_from(args.len()).map_err(|_| "too many arguments".to_owned())?;
                self.touch(usize::from(base) + 3)?;
                let mut targets = Vec::new();
                for (i, &x) in args.iter().enumerate() {
                    targets.push((self.touch(usize::from(base) + 4 + i)?, x));
                }
                self.scratch = self.scratch.max(usize::from(base) + 3 + args.len());
                self.place(&targets)?;
                self.b.emit(I::CallDirect {
                    base,
                    argc,
                    function: function.0,
                });
                if dst != base + 3 {
                    self.b.emit(I::Mov { dst, src: base + 3 });
                }
            }
            ir::Value::Closure { function, free } => {
                let free = free
                    .iter()
                    .map(|&x| self.read(x))
                    .collect::<Result<Vec<_>>>()?;
                self.b.emit(I::Closure {
                    dst,
                    function: function.0,
                    free,
                });
            }
            ir::Value::Global(g) => {
                let variable = self.tables.variables[g.0 as usize];
                self.b.emit(I::GetVariable { dst, variable });
            }
            ir::Value::SetGlobal(g, x) => {
                let src = self.read(*x)?;
                let variable = self.tables.variables[g.0 as usize];
                self.b.emit(I::SetVariable { src, variable });
                self.unspecified(dst, used);
            }
            ir::Value::Box(x) => {
                let src = self.read(*x)?;
                self.b.emit(I::MakeBox { dst, src });
            }
            ir::Value::Unbox(x) => {
                let src = self.read(*x)?;
                self.b.emit(I::Unbox { dst, src });
            }
            ir::Value::SetBox(cell, x) => {
                let cell = self.read(*cell)?;
                let src = self.read(*x)?;
                self.b.emit(I::SetBox { cell, src });
                self.unspecified(dst, used);
            }
            ir::Value::SetFree {
                closure,
                index,
                value,
            } => {
                let index =
                    u16::try_from(*index).map_err(|_| "a free variable index above 65,535")?;
                let closure = self.read(*closure)?;
                let src = self.read(*value)?;
                self.b.emit(I::SetFree {
                    closure,
                    index,
                    src,
                });
                self.unspecified(dst, used);
            }
        }
        Ok(())
    }

    /// Gives the result of an operation done for its effect, if it is used.
    fn unspecified(&mut self, dst: Slot, used: bool) {
        if used {
            let constant = self.tables.unspecified;
            self.b.emit(I::LoadConstant { dst, constant });
        }
    }

    fn primitive(
        &mut self,
        dst: Slot,
        op: ir::Primitive,
        args: &[ir::Operand],
        used: bool,
    ) -> Result<()> {
        use ir::Primitive as P;
        let instruction = match (op, args) {
            (P::Add | P::Sub, &[x, y]) if self.immediate(y).is_some() => {
                let imm = self.immediate(y).expect("checked");
                let a = self.read(x)?;
                if op == P::Add {
                    I::AddImm { dst, a, imm }
                } else {
                    I::SubImm { dst, a, imm }
                }
            }
            (P::Add | P::Sub | P::Mul | P::Lt | P::Le | P::NumEq | P::Eq, &[x, y]) => {
                let a = self.read(x)?;
                let b = self.read(y)?;
                match op {
                    P::Add => I::Add { dst, a, b },
                    P::Sub => I::Sub { dst, a, b },
                    P::Mul => I::Mul { dst, a, b },
                    P::Lt => I::Lt { dst, a, b },
                    P::Le => I::Le { dst, a, b },
                    P::NumEq => I::NumEq { dst, a, b },
                    _ => I::Eq { dst, a, b },
                }
            }
            (P::Cons, &[x, y]) => I::Cons {
                dst,
                car: self.read(x)?,
                cdr: self.read(y)?,
            },
            (P::Not, &[x]) => I::Not {
                dst,
                src: self.read(x)?,
            },
            (P::Car, &[x]) => I::Car {
                dst,
                pair: self.read(x)?,
            },
            (P::Cdr, &[x]) => I::Cdr {
                dst,
                pair: self.read(x)?,
            },
            (P::IsNull, &[x]) => I::IsNull {
                dst,
                src: self.read(x)?,
            },
            (P::IsPair, &[x]) => I::IsPair {
                dst,
                src: self.read(x)?,
            },
            (P::VectorLength, &[x]) => I::VectorLength {
                dst,
                vector: self.read(x)?,
            },
            (P::MakeVector, &[x, y]) => I::MakeVector {
                dst,
                len: self.read(x)?,
                fill: self.read(y)?,
            },
            (P::VectorRef, &[x, y]) => I::VectorRef {
                dst,
                vector: self.read(x)?,
                index: self.read(y)?,
            },
            (P::SetCar | P::SetCdr, &[x, y]) => {
                let pair = self.read(x)?;
                let value = self.read(y)?;
                self.b.emit(if op == P::SetCar {
                    I::SetCar { pair, value }
                } else {
                    I::SetCdr { pair, value }
                });
                self.unspecified(dst, used);
                return Ok(());
            }
            (P::VectorSet, &[x, y, z]) => {
                let vector = self.read(x)?;
                let index = self.read(y)?;
                let value = self.read(z)?;
                self.b.emit(I::VectorSet {
                    vector,
                    index,
                    value,
                });
                self.unspecified(dst, used);
                return Ok(());
            }
            _ => return Err(format!("{op} with {} operands", args.len())),
        };
        self.b.emit(instruction);
        Ok(())
    }
}
