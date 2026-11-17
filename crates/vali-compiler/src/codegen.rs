//! From the resolved tree to bytecode, in one walk per function.
//!
//! Slots are handed out by a counter: the next free slot. Every expression
//! gives back the slots it took, so a call's header, procedure and
//! arguments always sit at the top, as `call` needs (`docs/bytecode.md`),
//! and nested calls use the slots above them. The frame's size is the most
//! slots ever in use.
//!
//! Variables follow ADR 0005 #4: module variables for top-level
//! definitions, frame slots for locals, closure free variables for captured
//! locals that are never assigned, and boxes for locals that are.

use std::collections::{HashMap, HashSet};

use vali_bytecode::{
    Arity, Constant, Function, FunctionBuilder, Instruction as I, Label, Module, ModuleBuilder,
};
use vali_num::Number;
use vali_syntax::{Datum, DatumKind, Span};

use crate::CompileError;
use crate::ast::{Expr, Lambda, LambdaId, VarId, VarInfo};
use crate::expand::{RED_ZONE, STACK_GROWTH};

type Result<T> = std::result::Result<T, CompileError>;

/// The most elements a call to `vector` builds inline; longer ones call
/// the procedure.
const MAX_INLINE_VECTOR: usize = 8;

/// Where an expression's value goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    /// Into this slot.
    Slot(u8),
    /// Returned from the function: the expression is in tail position.
    Return,
    /// Nowhere: only its effects matter.
    Effect,
}

/// Where a variable lives in the function being compiled.
#[derive(Clone, Copy, Debug)]
enum Location {
    Slot(u8),
    Free(u8),
}

/// A loop being compiled: where its body starts, and its parameters'
/// slots.
struct Loop {
    head: Label,
    params: Vec<u8>,
}

/// One function being compiled.
struct Frame {
    b: FunctionBuilder,
    locations: HashMap<VarId, Location>,
    loops: HashMap<VarId, Loop>,
    next: usize,
    max: usize,
}

pub(crate) struct Generator<'a> {
    pub(crate) vars: &'a [VarInfo],
    pub(crate) lambdas: &'a [Lambda],
    pub(crate) loops: &'a HashSet<VarId>,
    pub(crate) module: ModuleBuilder,
}

fn error(message: impl Into<String>, span: Option<Span>) -> CompileError {
    CompileError {
        message: message.into(),
        span,
    }
}

/// A number as a constant.
fn number_constant(n: &Number) -> Constant {
    match n {
        Number::Integer(n) => match n.to_i64() {
            Some(n) => Constant::Integer(n),
            None => {
                let (negative, limbs) = n.limbs();
                Constant::Bignum {
                    negative,
                    limbs: limbs.to_vec(),
                }
            }
        },
        Number::Real(x) => Constant::Flonum(*x),
        Number::Rational(r) => Constant::Ratio(
            Box::new(number_constant(&Number::Integer(r.numerator()))),
            Box::new(number_constant(&Number::Integer(r.denominator()))),
        ),
        Number::Complex(c) => Constant::Complex(
            Box::new(number_constant(&c.real_part())),
            Box::new(number_constant(&c.imag_part())),
        ),
        _ => unreachable!("vali-num has no other kinds of number"),
    }
}

/// A datum as a constant.
fn constant(d: &Datum) -> Result<Constant> {
    Ok(match &d.kind {
        DatumKind::Number(n) => number_constant(n),
        DatumKind::Boolean(b) => Constant::Boolean(*b),
        DatumKind::Character(c) => Constant::Character(*c),
        DatumKind::String(s) => Constant::String(s.as_str().into()),
        DatumKind::Symbol(s) => Constant::Symbol(s.as_str().into()),
        DatumKind::List { items, tail } => {
            let mut list = match tail {
                Some(t) => constant(t)?,
                None => Constant::EmptyList,
            };
            for item in items.iter().rev() {
                list = Constant::Pair(Box::new(constant(item)?), Box::new(list));
            }
            list
        }
        DatumKind::Vector(items) => {
            Constant::Vector(items.iter().map(constant).collect::<Result<_>>()?)
        }
        DatumKind::Bytevector(bytes) => Constant::Bytevector(bytes.clone()),
        DatumKind::Keyword(_) => return Err(error("keywords are not supported yet", Some(d.span))),
        DatumKind::Labelled(..) | DatumKind::LabelRef(_) => {
            return Err(error(
                "datum labels in constants are not supported yet",
                Some(d.span),
            ));
        }
        _ => return Err(error("unsupported constant", Some(d.span))),
    })
}

/// A small integer constant fitting an 8-bit immediate.
fn small(e: &Expr) -> Option<i8> {
    match e {
        Expr::Const(Datum {
            kind: DatumKind::Number(Number::Integer(n)),
            ..
        }) => n.to_i64().and_then(|n| i8::try_from(n).ok()),
        _ => None,
    }
}

impl Generator<'_> {
    /// Compiles the program's body, and every function it contains.
    pub(crate) fn program(mut self, body: LambdaId) -> Result<Module> {
        let index = self.function(body)?;
        Ok(self.module.finish(index))
    }

    /// Compiles `lambda` and the functions inside it, returning its index.
    fn function(&mut self, l: LambdaId) -> Result<u32> {
        let lambda = &self.lambdas[l];
        let index = self.module.declare();
        let arity = Arity {
            required: lambda.required.len() as u8,
            optional: 0,
            rest: lambda.rest.is_some(),
        };
        let mut f = Frame {
            b: FunctionBuilder::new(lambda.name.as_deref(), arity, 0, 0),
            locations: HashMap::new(),
            loops: HashMap::new(),
            next: 1,
            max: 1,
        };
        for (i, &v) in lambda.free.iter().enumerate() {
            let i = u8::try_from(i).map_err(|_| error("too many free variables", None))?;
            f.locations.insert(v, Location::Free(i));
        }
        let params: Vec<VarId> = lambda.required.iter().copied().chain(lambda.rest).collect();
        for &v in &params {
            let s = self.alloc(&mut f)?;
            f.locations.insert(v, Location::Slot(s));
            if self.vars[v].boxed() {
                f.b.emit(I::MakeBox { dst: s, src: s });
            }
        }
        self.compile(&mut f, &lambda.body, Target::Return)?;
        let mut function: Function = f.b.finish().map_err(|e| error(e.to_string(), None))?;
        function.slots = f.max as u16;
        function.free = lambda.free.len() as u8;
        self.module.define(index, function);
        Ok(index)
    }

    fn alloc(&self, f: &mut Frame) -> Result<u8> {
        let s = u8::try_from(f.next)
            .map_err(|_| error("a function needs more than 256 slots", None))?;
        f.next += 1;
        f.max = f.max.max(f.next);
        Ok(s)
    }

    /// Where `target` wants a value made: its slot, or a fresh temporary.
    fn dst(&self, f: &mut Frame, target: Target) -> Result<u8> {
        match target {
            Target::Slot(s) => Ok(s),
            _ => self.alloc(f),
        }
    }

    /// Delivers the value in `s` to `target`.
    fn deliver(&self, f: &mut Frame, s: u8, target: Target) {
        match target {
            Target::Slot(t) if t != s => {
                f.b.emit(I::Mov { dst: t, src: s });
            }
            Target::Return => {
                f.b.emit(I::Return { src: s });
            }
            _ => {}
        }
    }

    /// A slot holding `e`'s value: the variable's own slot when it is an
    /// unboxed local in a slot, a fresh temporary otherwise.
    fn value(&mut self, f: &mut Frame, e: &Expr) -> Result<u8> {
        if let Expr::Local(v) = e
            && !self.vars[*v].boxed()
            && let Some(Location::Slot(s)) = f.locations.get(v)
        {
            return Ok(*s);
        }
        // A call's result is already in a slot of its own: its procedure's.
        if let Expr::Call(callee, args, span) = e
            && self.is_general_call(callee, args.len())
        {
            return self.general_call(f, callee, args, *span);
        }
        let t = self.alloc(f)?;
        self.compile(f, e, Target::Slot(t))?;
        Ok(t)
    }

    fn load(&mut self, f: &mut Frame, c: Constant, target: Target) -> Result<()> {
        if target == Target::Effect {
            return Ok(());
        }
        let dst = self.dst(f, target)?;
        match c {
            Constant::Integer(n) if i16::try_from(n).is_ok() => {
                f.b.emit(I::LoadInt {
                    dst,
                    value: n as i16,
                });
            }
            c => {
                let constant = self.module.constant(c);
                f.b.emit(I::LoadConstant { dst, constant });
            }
        }
        self.deliver(f, dst, target);
        Ok(())
    }

    /// Reads variable `v` into `dst`.
    fn read(&self, f: &mut Frame, v: VarId, dst: u8) -> Result<()> {
        let location = *f
            .locations
            .get(&v)
            .ok_or_else(|| error(format!("{}: no location", self.vars[v].name), None))?;
        match location {
            Location::Slot(s) if self.vars[v].boxed() => {
                f.b.emit(I::Unbox { dst, src: s });
            }
            Location::Slot(s) => {
                if s != dst {
                    f.b.emit(I::Mov { dst, src: s });
                }
            }
            Location::Free(i) => {
                f.b.emit(I::Free { dst, index: i });
                if self.vars[v].boxed() {
                    f.b.emit(I::Unbox { dst, src: dst });
                }
            }
        }
        Ok(())
    }

    /// The slot holding `v`'s box, loading it from the closure if need be.
    fn cell(&self, f: &mut Frame, v: VarId) -> Result<u8> {
        match f.locations[&v] {
            Location::Slot(s) => Ok(s),
            Location::Free(i) => {
                let t = self.alloc(f)?;
                f.b.emit(I::Free { dst: t, index: i });
                Ok(t)
            }
        }
    }

    /// Compiles `e`, delivering its value to `target`. Gives back every
    /// slot it took.
    fn compile(&mut self, f: &mut Frame, e: &Expr, target: Target) -> Result<()> {
        let saved = f.next;
        stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || self.expr(f, e, target))?;
        f.next = saved;
        Ok(())
    }

    fn expr(&mut self, f: &mut Frame, e: &Expr, target: Target) -> Result<()> {
        match e {
            Expr::Unspecified => self.load(f, Constant::Unspecified, target),
            Expr::Const(d) => self.load(f, constant(d)?, target),
            Expr::Local(v) => {
                if target == Target::Effect {
                    return Ok(());
                }
                if let Target::Slot(s) = target {
                    return self.read(f, *v, s);
                }
                let s = self.value(f, e)?;
                self.deliver(f, s, target);
                Ok(())
            }
            Expr::Global(name) | Expr::Free(name) | Expr::Imported(_, name) => {
                if target == Target::Effect {
                    return Ok(());
                }
                let dst = self.dst(f, target)?;
                let variable = match e {
                    Expr::Global(_) => self.module.defined(name),
                    Expr::Imported(library, _) => self.module.imported(library, name),
                    _ => self.module.variable(name),
                };
                f.b.emit(I::GetVariable { dst, variable });
                self.deliver(f, dst, target);
                Ok(())
            }
            Expr::SetLocal(v, value) => {
                if self.vars[*v].boxed() {
                    let src = self.value(f, value)?;
                    let cell = self.cell(f, *v)?;
                    f.b.emit(I::SetBox { cell, src });
                } else {
                    let Location::Slot(s) = f.locations[v] else {
                        return Err(error("an assigned captured variable must be boxed", None));
                    };
                    self.compile(f, value, Target::Slot(s))?;
                }
                self.load(f, Constant::Unspecified, target)
            }
            Expr::SetGlobal(name, value) => {
                let src = self.value(f, value)?;
                let variable = self.module.defined(name);
                f.b.emit(I::SetVariable { src, variable });
                self.load(f, Constant::Unspecified, target)
            }
            Expr::If(c, t, e) => self.if_(f, c, t, e, target),
            Expr::Begin(es) => {
                let (last, init) = es.split_last().expect("a non-empty begin");
                for e in init {
                    self.compile(f, e, Target::Effect)?;
                }
                self.compile(f, last, target)
            }
            Expr::Lambda(l) => {
                if target == Target::Effect {
                    return Ok(());
                }
                let dst = self.dst(f, target)?;
                self.closure(f, *l, dst)?;
                self.deliver(f, dst, target);
                Ok(())
            }
            Expr::Let(bindings, body) => {
                let mut slots = Vec::new();
                for (_, init) in bindings {
                    let s = self.alloc(f)?;
                    self.compile(f, init, Target::Slot(s))?;
                    slots.push(s);
                }
                for ((v, _), s) in bindings.iter().zip(slots) {
                    f.locations.insert(*v, Location::Slot(s));
                    if self.vars[*v].boxed() {
                        f.b.emit(I::MakeBox { dst: s, src: s });
                    }
                }
                self.compile(f, body, target)
            }
            Expr::Letrec(bindings, body) => self.letrec(f, bindings, body, target),
            Expr::Call(callee, args, span) => self.call(f, callee, args, *span, target),
        }
    }

    /// Makes a closure of `l` in `dst`, capturing its free variables from
    /// where they live in this frame.
    fn closure(&mut self, f: &mut Frame, l: LambdaId, dst: u8) -> Result<()> {
        let function = self.function(l)?;
        let mut free = Vec::new();
        for &v in &self.lambdas[l].free {
            let location = f.locations.get(&v).copied();
            free.push(match location {
                Some(Location::Slot(s)) => s,
                Some(Location::Free(i)) => {
                    let t = self.alloc(f)?;
                    f.b.emit(I::Free { dst: t, index: i });
                    t
                }
                None => {
                    return Err(error(
                        format!("{}: a captured variable has no location", self.vars[v].name),
                        None,
                    ));
                }
            });
        }
        f.b.emit(I::Closure {
            dst,
            function,
            free,
        });
        Ok(())
    }

    fn if_(&mut self, f: &mut Frame, c: &Expr, t: &Expr, e: &Expr, target: Target) -> Result<()> {
        // (if (not x) t e) is (if x e t).
        if let Expr::Call(callee, args, _) = c
            && let [x] = &args[..]
            && self.inlinable(callee, "not")
        {
            return self.if_(f, x, e, t, target);
        }
        let otherwise = f.b.label();
        self.test(f, c, otherwise)?;
        self.compile(f, t, target)?;
        if target == Target::Return {
            f.b.bind(otherwise);
            return self.compile(f, e, target);
        }
        let end = f.b.label();
        f.b.jump(end);
        f.b.bind(otherwise);
        self.compile(f, e, target)?;
        f.b.bind(end);
        Ok(())
    }

    /// Continues if `c` is true; jumps to `otherwise` if not. Comparisons
    /// and type tests of standard procedures become one branch.
    fn test(&mut self, f: &mut Frame, c: &Expr, otherwise: Label) -> Result<()> {
        let saved = f.next;
        if let Expr::Call(callee, args, _) = c
            && let Expr::Free(name) = &**callee
        {
            let branch = match (name.as_str(), &args[..]) {
                ("<", [a, b]) => match small(b) {
                    Some(imm) => Some(I::IfLtImm {
                        a: self.value(f, a)?,
                        imm,
                        offset: 0,
                    }),
                    None => Some(I::IfLt {
                        a: self.value(f, a)?,
                        b: self.value(f, b)?,
                        offset: 0,
                    }),
                },
                (">", [a, b]) => {
                    let (a, b) = (self.value(f, a)?, self.value(f, b)?);
                    Some(I::IfLt {
                        a: b,
                        b: a,
                        offset: 0,
                    })
                }
                ("<=", [a, b]) => Some(I::IfLe {
                    a: self.value(f, a)?,
                    b: self.value(f, b)?,
                    offset: 0,
                }),
                (">=", [a, b]) => {
                    let (a, b) = (self.value(f, a)?, self.value(f, b)?);
                    Some(I::IfLe {
                        a: b,
                        b: a,
                        offset: 0,
                    })
                }
                ("=", [a, b]) => Some(I::IfNumEq {
                    a: self.value(f, a)?,
                    b: self.value(f, b)?,
                    offset: 0,
                }),
                ("eq?", [a, b]) => Some(I::IfEq {
                    a: self.value(f, a)?,
                    b: self.value(f, b)?,
                    offset: 0,
                }),
                ("null?", [a]) => Some(I::IfNull {
                    a: self.value(f, a)?,
                    offset: 0,
                }),
                ("pair?", [a]) => Some(I::IfPair {
                    a: self.value(f, a)?,
                    offset: 0,
                }),
                _ => None,
            };
            if let Some(branch) = branch {
                f.b.branch(branch, otherwise);
                f.next = saved;
                return Ok(());
            }
        }
        let test = self.value(f, c)?;
        f.b.branch(I::If { test, offset: 0 }, otherwise);
        f.next = saved;
        Ok(())
    }

    fn letrec(
        &mut self,
        f: &mut Frame,
        bindings: &[(VarId, Expr)],
        body: &Expr,
        target: Target,
    ) -> Result<()> {
        // Every variable gets its slot first, and boxes before any closure
        // can capture them.
        let mut slots = Vec::new();
        for (v, _) in bindings {
            if self.loops.contains(v) {
                slots.push(None);
                continue;
            }
            let s = self.alloc(f)?;
            f.locations.insert(*v, Location::Slot(s));
            if self.vars[*v].boxed() {
                self.load(f, Constant::Unspecified, Target::Slot(s))?;
                f.b.emit(I::MakeBox { dst: s, src: s });
            }
            slots.push(Some(s));
        }
        for (k, ((v, init), s)) in bindings.iter().zip(&slots).enumerate() {
            let Some(s) = *s else {
                continue;
            };
            if self.vars[*v].boxed() {
                let src = self.value(f, init)?;
                f.b.emit(I::SetBox { cell: s, src });
                continue;
            }
            self.compile(f, init, Target::Slot(s))?;
            // Closures made so far that captured this variable before it had
            // its value get it now.
            for ((_, earlier), closure) in bindings[..=k].iter().zip(&slots) {
                if let (Expr::Lambda(l), Some(closure)) = (earlier, closure)
                    && let Some(index) = self.lambdas[*l].free.iter().position(|w| w == v)
                {
                    f.b.emit(I::SetFree {
                        closure: *closure,
                        index: index as u8,
                        src: s,
                    });
                }
            }
        }
        self.compile(f, body, target)
    }

    /// Whether `callee` is the standard procedure `name`, safe to inline.
    fn inlinable(&self, callee: &Expr, name: &str) -> bool {
        matches!(callee, Expr::Free(n) if n == name)
    }

    fn call(
        &mut self,
        f: &mut Frame,
        callee: &Expr,
        args: &[Expr],
        span: Span,
        target: Target,
    ) -> Result<()> {
        if let Expr::Local(v) = callee
            && self.loops.contains(v)
        {
            return self.loop_call(f, *v, args, target);
        }
        if let Expr::Free(name) = callee
            && self.inline(f, name, args, target)?
        {
            return Ok(());
        }
        if target != Target::Return {
            let result = self.general_call(f, callee, args, span)?;
            self.deliver(f, result, target);
            return Ok(());
        }
        let argc = u8::try_from(args.len()).map_err(|_| error("too many arguments", Some(span)))?;
        {
            let proc = self.alloc(f)?;
            self.compile(f, callee, Target::Slot(proc))?;
            for arg in args {
                let s = self.alloc(f)?;
                self.compile(f, arg, Target::Slot(s))?;
            }
            f.b.emit(I::TailCall { proc, argc });
            Ok(())
        }
    }

    /// Whether a call is neither a loop nor inlined: a real call.
    fn is_general_call(&self, callee: &Expr, argc: usize) -> bool {
        match callee {
            Expr::Local(v) => !self.loops.contains(v),
            Expr::Free(name) => !Self::inlined(name, argc),
            _ => true,
        }
    }

    /// Whether `inline` handles a call to `name` with `argc` arguments.
    fn inlined(name: &str, argc: usize) -> bool {
        matches!(
            (name, argc),
            (
                "car" | "cdr" | "null?" | "pair?" | "not" | "vector-length",
                1
            ) | (
                "cons"
                    | "eq?"
                    | "<"
                    | "<="
                    | "="
                    | ">"
                    | ">="
                    | "vector-ref"
                    | "make-vector"
                    | "set-car!"
                    | "set-cdr!",
                2
            ) | ("vector-set!", 3)
                | ("+" | "*", _)
                | ("vector", 1..=MAX_INLINE_VECTOR)
        ) || (name == "-" && argc >= 1)
    }

    /// A call that is not a tail call: the header, procedure and arguments
    /// at the top of the frame. Returns the slot that receives the result,
    /// the procedure's, which stays taken until the caller gives back its
    /// slots.
    fn general_call(
        &mut self,
        f: &mut Frame,
        callee: &Expr,
        args: &[Expr],
        span: Span,
    ) -> Result<u8> {
        let argc = u8::try_from(args.len()).map_err(|_| error("too many arguments", Some(span)))?;
        let base = self.alloc(f)?;
        self.alloc(f)?;
        self.alloc(f)?;
        let proc = self.alloc(f)?;
        self.compile(f, callee, Target::Slot(proc))?;
        for arg in args {
            let s = self.alloc(f)?;
            self.compile(f, arg, Target::Slot(s))?;
        }
        f.b.emit(I::Call { base, argc });
        f.next = usize::from(proc) + 1;
        Ok(proc)
    }

    /// A call to a loop: the entry, which compiles the loop's body here, or
    /// a jump back to its head from inside it.
    fn loop_call(&mut self, f: &mut Frame, v: VarId, args: &[Expr], target: Target) -> Result<()> {
        let lambda = self.loop_lambda(v)?;
        if let Some(info) = f.loops.get(&v) {
            // A jump back: evaluate every argument before assigning any.
            let params = info.params.clone();
            let head = info.head;
            let mut temps = Vec::new();
            for arg in args {
                let t = self.alloc(f)?;
                self.compile(f, arg, Target::Slot(t))?;
                temps.push(t);
            }
            for ((&p, t), &param) in params.iter().zip(temps).zip(&self.lambdas[lambda].required) {
                f.b.emit(I::Mov { dst: p, src: t });
                if self.vars[param].boxed() {
                    f.b.emit(I::MakeBox { dst: p, src: p });
                }
            }
            f.b.jump(head);
            return Ok(());
        }
        // The entry: the parameters get slots here, and the body follows.
        let mut params = Vec::new();
        for arg in args {
            let p = self.alloc(f)?;
            self.compile(f, arg, Target::Slot(p))?;
            params.push(p);
        }
        for (&p, &param) in params.iter().zip(&self.lambdas[lambda].required) {
            f.locations.insert(param, Location::Slot(p));
            if self.vars[param].boxed() {
                f.b.emit(I::MakeBox { dst: p, src: p });
            }
        }
        let head = f.b.label();
        f.b.bind(head);
        f.loops.insert(v, Loop { head, params });
        self.compile(f, &self.lambdas[lambda].body, target)?;
        f.loops.remove(&v);
        Ok(())
    }

    /// The `lambda` a loop variable is bound to.
    fn loop_lambda(&self, v: VarId) -> Result<LambdaId> {
        fn find(e: &Expr, v: VarId, lambdas: &[Lambda]) -> Option<LambdaId> {
            match e {
                Expr::Letrec(bindings, body) => bindings
                    .iter()
                    .find_map(|(w, init)| match init {
                        Expr::Lambda(l) if *w == v => Some(*l),
                        _ => None,
                    })
                    .or_else(|| bindings.iter().find_map(|(_, i)| find(i, v, lambdas)))
                    .or_else(|| find(body, v, lambdas)),
                Expr::Let(bindings, body) => bindings
                    .iter()
                    .find_map(|(_, i)| find(i, v, lambdas))
                    .or_else(|| find(body, v, lambdas)),
                Expr::If(a, b, c) => find(a, v, lambdas)
                    .or_else(|| find(b, v, lambdas))
                    .or_else(|| find(c, v, lambdas)),
                Expr::Begin(es) => es.iter().find_map(|e| find(e, v, lambdas)),
                Expr::SetLocal(_, e) | Expr::SetGlobal(_, e) => find(e, v, lambdas),
                Expr::Call(c, args, _) => {
                    find(c, v, lambdas).or_else(|| args.iter().find_map(|a| find(a, v, lambdas)))
                }
                Expr::Lambda(l) => find(&lambdas[*l].body, v, lambdas),
                _ => None,
            }
        }
        let binder = self.vars[v].binder;
        find(&self.lambdas[binder].body, v, self.lambdas)
            .ok_or_else(|| error(format!("{}: loop not found", self.vars[v].name), None))
    }

    /// `(vector e ...)`: a vector filled with the first element, then a
    /// store for each element held in another slot.
    fn vector(&mut self, f: &mut Frame, args: &[Expr], target: Target) -> Result<()> {
        let mut elements = Vec::new();
        for arg in args {
            elements.push(self.value(f, arg)?);
        }
        // The vector is built over several instructions, so it may only go
        // straight into the target if no element is read from there.
        let dst = match target {
            Target::Slot(s) if !elements.contains(&s) => s,
            _ => self.alloc(f)?,
        };
        let index = self.alloc(f)?;
        #[expect(clippy::cast_possible_truncation, reason = "at most MAX_INLINE_VECTOR")]
        f.b.emit(I::LoadInt {
            dst: index,
            value: elements.len() as i16,
        })
        .emit(I::MakeVector {
            dst,
            len: index,
            fill: elements[0],
        });
        for (i, &value) in elements.iter().enumerate().skip(1) {
            if value != elements[0] {
                #[expect(clippy::cast_possible_truncation, reason = "at most MAX_INLINE_VECTOR")]
                f.b.emit(I::LoadInt {
                    dst: index,
                    value: i as i16,
                })
                .emit(I::VectorSet {
                    vector: dst,
                    index,
                    value,
                });
            }
        }
        self.deliver(f, dst, target);
        Ok(())
    }

    /// Compiles a call to a standard procedure as instructions, if it has
    /// them for this many arguments. Returns whether it did.
    fn inline(&mut self, f: &mut Frame, name: &str, args: &[Expr], target: Target) -> Result<bool> {
        macro_rules! unary {
            ($make:expr) => {{
                let a = self.value(f, &args[0])?;
                let dst = self.dst(f, target)?;
                f.b.emit($make(dst, a));
                self.deliver(f, dst, target);
            }};
        }
        macro_rules! binary {
            ($make:expr) => {{
                let a = self.value(f, &args[0])?;
                let b = self.value(f, &args[1])?;
                let dst = self.dst(f, target)?;
                f.b.emit($make(dst, a, b));
                self.deliver(f, dst, target);
            }};
        }
        match (name, args.len()) {
            ("car", 1) => unary!(|dst, pair| I::Car { dst, pair }),
            ("cdr", 1) => unary!(|dst, pair| I::Cdr { dst, pair }),
            ("null?", 1) => unary!(|dst, src| I::IsNull { dst, src }),
            ("pair?", 1) => unary!(|dst, src| I::IsPair { dst, src }),
            ("not", 1) => unary!(|dst, src| I::Not { dst, src }),
            ("vector-length", 1) => unary!(|dst, vector| I::VectorLength { dst, vector }),
            ("cons", 2) => binary!(|dst, car, cdr| I::Cons { dst, car, cdr }),
            ("eq?", 2) => binary!(|dst, a, b| I::Eq { dst, a, b }),
            ("<", 2) => binary!(|dst, a, b| I::Lt { dst, a, b }),
            ("<=", 2) => binary!(|dst, a, b| I::Le { dst, a, b }),
            ("=", 2) => binary!(|dst, a, b| I::NumEq { dst, a, b }),
            (">", 2) => binary!(|dst, a, b| I::Lt { dst, a: b, b: a }),
            (">=", 2) => binary!(|dst, a, b| I::Le { dst, a: b, b: a }),
            ("vector-ref", 2) => binary!(|dst, vector, index| I::VectorRef { dst, vector, index }),
            ("make-vector", 2) => binary!(|dst, len, fill| I::MakeVector { dst, len, fill }),
            ("set-car!" | "set-cdr!", 2) => {
                let pair = self.value(f, &args[0])?;
                let value = self.value(f, &args[1])?;
                f.b.emit(if name == "set-car!" {
                    I::SetCar { pair, value }
                } else {
                    I::SetCdr { pair, value }
                });
                self.load(f, Constant::Unspecified, target)?;
            }
            ("vector-set!", 3) => {
                let vector = self.value(f, &args[0])?;
                let index = self.value(f, &args[1])?;
                let value = self.value(f, &args[2])?;
                f.b.emit(I::VectorSet {
                    vector,
                    index,
                    value,
                });
                self.load(f, Constant::Unspecified, target)?;
            }
            ("vector", 1..=MAX_INLINE_VECTOR) => self.vector(f, args, target)?,
            ("+" | "*", 0) => {
                let unit = if name == "+" { 0 } else { 1 };
                self.load(f, Constant::Integer(unit), target)?;
            }
            ("+" | "*" | "-", _) if !args.is_empty() => self.arithmetic(f, name, args, target)?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// `+`, `*` and `-` with one or more arguments, folded left, with small
    /// constants as immediates.
    fn arithmetic(
        &mut self,
        f: &mut Frame,
        name: &str,
        args: &[Expr],
        target: Target,
    ) -> Result<()> {
        // With three or more arguments the running total is written before
        // later arguments are read, so it must not be the target slot, which
        // may be a variable those arguments read.
        if args.len() == 1 {
            let dst = self.dst(f, target)?;
            if name == "-" {
                // (- x) is (- 0 x).
                let x = self.value(f, &args[0])?;
                let zero = self.alloc(f)?;
                f.b.emit(I::LoadInt {
                    dst: zero,
                    value: 0,
                });
                f.b.emit(I::Sub { dst, a: zero, b: x });
            } else {
                self.compile(f, &args[0], Target::Slot(dst))?;
            }
            self.deliver(f, dst, target);
            return Ok(());
        }
        if let [first, second] = args {
            // Operands first, then the result: into the target's slot, or
            // over the first operand when that is a temporary of ours.
            let start = f.next;
            let a = self.value(f, first)?;
            let imm = small(second);
            let b = match (name, imm) {
                ("+" | "-", Some(_)) => None,
                _ => Some(self.value(f, second)?),
            };
            let dst = match target {
                Target::Slot(t) => t,
                _ if usize::from(a) >= start => a,
                _ => self.alloc(f)?,
            };
            f.b.emit(match (name, imm, b) {
                ("+", Some(imm), None) => I::AddImm { dst, a, imm },
                ("-", Some(imm), None) => I::SubImm { dst, a, imm },
                ("+", _, Some(b)) => I::Add { dst, a, b },
                ("-", _, Some(b)) => I::Sub { dst, a, b },
                (_, _, Some(b)) => I::Mul { dst, a, b },
                _ => unreachable!("an immediate only for + and -"),
            });
            self.deliver(f, dst, target);
            return Ok(());
        }
        let dst = self.alloc(f)?;
        let mut acc = self.value(f, &args[0])?;
        for arg in &args[1..] {
            let imm = small(arg);
            let instruction = match (name, imm) {
                ("+", Some(imm)) => I::AddImm { dst, a: acc, imm },
                ("-", Some(imm)) => I::SubImm { dst, a: acc, imm },
                _ => {
                    let b = self.value(f, arg)?;
                    match name {
                        "+" => I::Add { dst, a: acc, b },
                        "-" => I::Sub { dst, a: acc, b },
                        _ => I::Mul { dst, a: acc, b },
                    }
                }
            };
            f.b.emit(instruction);
            acc = dst;
        }
        self.deliver(f, dst, target);
        Ok(())
    }
}
