//! The IR's encoding (ADR 0017 #8).
//!
//! A module is its name, sources, constants, globals, exports, body and
//! functions, each function a record with its length so a reader can skip
//! it. A function's terms follow in pre-order: a `let`'s value then its
//! body, a `join`'s parameters then its handler then its body, an `if`'s
//! test then its arms.
//!
//! Variables and join points are not written where they are bound: they
//! are numbered as they are, parameters first, then free variables, then
//! each binding in the order the terms come. An operand that is a variable
//! is written relative to the last one bound (0 is the most recent), so
//! the common case is one byte; one that is a constant is its index. A
//! jump names its join point the same way. Decoding numbers them the same
//! way, so encoding then decoding gives the module renumbered in that
//! order, and encoding that again gives the same bytes.
//!
//! [`decode`] validates what it decodes, and neither direction recurses.

use crate::bytes::{DecodeError, Reader, Writer};
use crate::ir::{
    Arity, Constant, ConstantId, FunId, Function, Global, GlobalId, GlobalLink, Inlined, JoinId,
    Module, Operand, Position, Term, TermId, Value, Var,
};
use crate::primitive::Primitive;
use crate::validate::{ValidationError, validate};

const HOLE: TermId = TermId(u32::MAX);

/// Encodes `module`, which must be valid.
pub fn encode(module: &Module) -> Result<Vec<u8>, ValidationError> {
    validate(module)?;
    let mut w = Writer::new();
    w.str(&module.name);
    w.usize(module.sources.len());
    for source in &module.sources {
        w.str(source);
    }
    w.usize(module.constants.len());
    for constant in &module.constants {
        encode_constant(&mut w, constant);
    }
    w.usize(module.globals.len());
    for global in &module.globals {
        w.str(&global.name);
        match &global.link {
            GlobalLink::Defined => w.byte(0),
            GlobalLink::Imported(library) => {
                w.byte(1);
                w.str(library);
            }
            GlobalLink::Primitive => w.byte(2),
            GlobalLink::Native(library) => {
                w.byte(3);
                w.str(library);
            }
            GlobalLink::Repl => w.byte(4),
        }
    }
    w.usize(module.exports.len());
    for (name, global) in &module.exports {
        w.str(name);
        w.u32(global.0);
    }
    w.u32(module.body.0);
    w.usize(module.functions.len());
    for function in &module.functions {
        let mut f = Writer::new();
        encode_function(&mut f, function);
        w.bytes(&f.finish());
    }
    Ok(w.finish())
}

/// Decodes and validates a module.
pub fn decode(bytes: &[u8]) -> Result<Module, DecodeError> {
    let mut r = Reader::new(bytes);
    let name = r.str()?.into();
    let n = r.count(1)?;
    let mut sources = Vec::with_capacity(n);
    for _ in 0..n {
        sources.push(r.str()?.into());
    }
    let n = r.count(1)?;
    let mut constants = Vec::with_capacity(n);
    for _ in 0..n {
        constants.push(decode_constant(&mut r)?);
    }
    let n = r.count(2)?;
    let mut globals = Vec::with_capacity(n);
    for _ in 0..n {
        let name = r.str()?.into();
        let link = match r.byte()? {
            0 => GlobalLink::Defined,
            1 => GlobalLink::Imported(r.str()?.into()),
            2 => GlobalLink::Primitive,
            3 => GlobalLink::Native(r.str()?.into()),
            4 => GlobalLink::Repl,
            b => return Err(r.error(format!("unknown variable link {b}"))),
        };
        globals.push(Global { name, link });
    }
    let n = r.count(2)?;
    let mut exports = Vec::with_capacity(n);
    for _ in 0..n {
        let name = r.str()?.into();
        exports.push((name, GlobalId(r.u32()?)));
    }
    let body = FunId(r.u32()?);
    let n = r.count(1)?;
    let mut functions = Vec::with_capacity(n);
    for _ in 0..n {
        let record = r.bytes()?;
        let start = r.offset() - record.len();
        let mut f = Reader::new(record);
        let function = decode_function(&mut f)
            .and_then(|function| f.end().map(|()| function))
            .map_err(|e| DecodeError {
                offset: start + e.offset,
                message: e.message,
            })?;
        functions.push(function);
    }
    r.end()?;
    let module = Module {
        name,
        constants,
        globals,
        exports,
        functions,
        body,
        sources,
    };
    validate(&module).map_err(|e| DecodeError {
        offset: bytes.len(),
        message: e.to_string(),
    })?;
    Ok(module)
}

/// The most free variables or join-point parameters a function may have:
/// they take no bytes each, so the input's length cannot bound them.
const MAX_BOUND_TOGETHER: u64 = 1 << 16;

fn bounded(r: &mut Reader<'_>, what: &str) -> Result<usize, DecodeError> {
    let n = r.uint()?;
    if n > MAX_BOUND_TOGETHER {
        return Err(r.error(format!("{n} {what}, more than {MAX_BOUND_TOGETHER}")));
    }
    Ok(n as usize)
}

fn encode_constant(w: &mut Writer, constant: &Constant) {
    let mut pending = vec![constant];
    while let Some(c) = pending.pop() {
        match c {
            Constant::Integer(n) => {
                w.byte(0);
                w.int(*n);
            }
            Constant::Bignum { negative, limbs } => {
                w.byte(1);
                w.bool(*negative);
                w.usize(limbs.len());
                for limb in limbs {
                    w.uint(*limb);
                }
            }
            Constant::Ratio(a, b) => {
                w.byte(2);
                pending.push(b);
                pending.push(a);
            }
            Constant::Flonum(x) => {
                w.byte(3);
                w.f64(*x);
            }
            Constant::Complex(a, b) => {
                w.byte(4);
                pending.push(b);
                pending.push(a);
            }
            Constant::Boolean(b) => w.byte(5 + u8::from(*b)),
            Constant::Character(c) => {
                w.byte(7);
                w.u32(u32::from(*c));
            }
            Constant::EmptyList => w.byte(8),
            Constant::Unspecified => w.byte(9),
            Constant::String(s) => {
                w.byte(10);
                w.str(s);
            }
            Constant::Symbol(s) => {
                w.byte(11);
                w.str(s);
            }
            Constant::Keyword(s) => {
                w.byte(12);
                w.str(s);
            }
            Constant::Pair(a, b) => {
                w.byte(13);
                pending.push(b);
                pending.push(a);
            }
            Constant::Vector(items) => {
                w.byte(14);
                w.usize(items.len());
                pending.extend(items.iter().rev());
            }
            Constant::Bytevector(bytes) => {
                w.byte(15);
                w.bytes(bytes);
            }
            Constant::Labelled(n, inner) => {
                w.byte(16);
                w.u32(*n);
                pending.push(inner);
            }
            Constant::LabelRef(n) => {
                w.byte(17);
                w.u32(*n);
            }
        }
    }
}

/// A constant whose parts are still being decoded.
enum Open {
    Ratio,
    Complex,
    Pair,
    Vector(usize),
    Labelled(u32),
}

fn decode_constant(r: &mut Reader<'_>) -> Result<Constant, DecodeError> {
    let mut open: Vec<(Open, Vec<Constant>)> = Vec::new();
    loop {
        let leaf = match r.byte()? {
            0 => Constant::Integer(r.int()?),
            1 => {
                let negative = r.bool()?;
                let n = r.count(1)?;
                let mut limbs = Vec::with_capacity(n);
                for _ in 0..n {
                    limbs.push(r.uint()?);
                }
                Constant::Bignum { negative, limbs }
            }
            2 => {
                open.push((Open::Ratio, Vec::with_capacity(2)));
                continue;
            }
            3 => Constant::Flonum(r.f64()?),
            4 => {
                open.push((Open::Complex, Vec::with_capacity(2)));
                continue;
            }
            5 => Constant::Boolean(false),
            6 => Constant::Boolean(true),
            7 => {
                let n = r.u32()?;
                Constant::Character(
                    char::from_u32(n)
                        .ok_or_else(|| r.error(format!("{n:#x} is not a character")))?,
                )
            }
            8 => Constant::EmptyList,
            9 => Constant::Unspecified,
            10 => Constant::String(r.str()?.into()),
            11 => Constant::Symbol(r.str()?.into()),
            12 => Constant::Keyword(r.str()?.into()),
            13 => {
                open.push((Open::Pair, Vec::with_capacity(2)));
                continue;
            }
            14 => {
                let n = r.count(1)?;
                if n > 0 {
                    open.push((Open::Vector(n), Vec::with_capacity(n)));
                    continue;
                }
                Constant::Vector(Vec::new())
            }
            15 => Constant::Bytevector(r.bytes()?.to_vec()),
            16 => {
                let n = r.u32()?;
                open.push((Open::Labelled(n), Vec::with_capacity(1)));
                continue;
            }
            17 => Constant::LabelRef(r.u32()?),
            b => return Err(r.error(format!("unknown constant tag {b}"))),
        };
        // Deliver it to what is waiting for it, closing what fills up.
        let mut value = leaf;
        loop {
            let Some((kind, parts)) = open.last_mut() else {
                return Ok(value);
            };
            parts.push(value);
            let needed = match kind {
                Open::Vector(n) => *n,
                Open::Labelled(_) => 1,
                _ => 2,
            };
            if parts.len() < needed {
                break;
            }
            let (kind, mut parts) = open.pop().expect("open");
            value = match kind {
                Open::Vector(_) => Constant::Vector(parts),
                Open::Labelled(n) => {
                    Constant::Labelled(n, Box::new(parts.pop().expect("one part")))
                }
                _ => {
                    let b = Box::new(parts.pop().expect("two parts"));
                    let a = Box::new(parts.pop().expect("two parts"));
                    match kind {
                        Open::Ratio => Constant::Ratio(a, b),
                        Open::Complex => Constant::Complex(a, b),
                        _ => Constant::Pair(a, b),
                    }
                }
            };
        }
    }
}

/// Numbers variables and join points as they are bound, in encoding
/// order.
struct Numbering {
    vars: Vec<Option<u32>>,
    next_var: u32,
    joins: Vec<Option<u32>>,
    next_join: u32,
}

impl Numbering {
    fn bind(&mut self, v: Var) {
        self.vars[v.0 as usize] = Some(self.next_var);
        self.next_var += 1;
    }

    fn bind_join(&mut self, j: JoinId) {
        self.joins[j.0 as usize] = Some(self.next_join);
        self.next_join += 1;
    }

    fn operand(&self, w: &mut Writer, x: Operand) {
        match x {
            Operand::Var(v) => {
                let n = self.vars[v.0 as usize].expect("bound before use in a valid module");
                w.uint(u64::from(self.next_var - 1 - n) << 1);
            }
            Operand::Const(c) => w.uint((u64::from(c.0) << 1) | 1),
        }
    }

    fn operands(&self, w: &mut Writer, xs: &[Operand]) {
        w.usize(xs.len());
        for &x in xs {
            self.operand(w, x);
        }
    }
}

fn encode_position(w: &mut Writer, position: Option<Position>) {
    match position {
        None => w.byte(0),
        Some(p) => {
            // Flag 2 is a position in an inlined copy, which follows.
            w.byte(if p.inlined == 0 { 1 } else { 2 });
            w.uint(u64::from(p.source));
            w.u32(p.line);
            w.u32(p.column);
            if p.inlined != 0 {
                w.u32(p.inlined);
            }
        }
    }
}

fn encode_function(w: &mut Writer, f: &Function) {
    match &f.name {
        Some(name) => {
            w.bool(true);
            w.str(name);
        }
        None => w.bool(false),
    }
    w.byte(f.arity.required);
    w.byte(f.arity.optional);
    w.bool(f.arity.rest);
    w.usize(f.free.len());
    w.usize(f.inlined.len());
    for copy in &f.inlined {
        match &copy.name {
            Some(name) => {
                w.bool(true);
                w.str(name);
            }
            None => w.bool(false),
        }
        encode_position(w, copy.call);
    }
    let mut n = Numbering {
        vars: vec![None; f.vars as usize],
        next_var: 0,
        joins: vec![None; f.joins as usize],
        next_join: 0,
    };
    for &v in f.params.iter().chain(&f.free) {
        n.bind(v);
    }
    let mut pending = vec![f.body];
    while let Some(id) = pending.pop() {
        match f.term(id) {
            Term::Let {
                var,
                value,
                body,
                position,
            } => {
                w.byte(0);
                encode_value(w, &n, value);
                encode_position(w, *position);
                n.bind(*var);
                pending.push(*body);
            }
            Term::Join {
                join,
                params,
                recursive,
                handler,
                body,
            } => {
                w.byte(1);
                w.usize(params.len());
                w.bool(*recursive);
                n.bind_join(*join);
                for &p in params {
                    n.bind(p);
                }
                pending.push(*body);
                pending.push(*handler);
            }
            Term::Jump { join, args } => {
                w.byte(2);
                let j = n.joins[join.0 as usize].expect("bound before use in a valid module");
                w.u32(n.next_join - 1 - j);
                n.operands(w, args);
            }
            Term::If {
                test,
                then,
                otherwise,
            } => {
                w.byte(3);
                n.operand(w, *test);
                pending.push(*otherwise);
                pending.push(*then);
            }
            Term::Return(x) => {
                w.byte(4);
                n.operand(w, *x);
            }
            Term::TailCall {
                callee,
                args,
                position,
            } => {
                w.byte(5);
                n.operand(w, *callee);
                n.operands(w, args);
                encode_position(w, *position);
            }
            Term::TailCallDirect {
                function,
                args,
                position,
            } => {
                w.byte(6);
                w.u32(function.0);
                n.operands(w, args);
                encode_position(w, *position);
            }
        }
    }
}

fn encode_value(w: &mut Writer, n: &Numbering, value: &Value) {
    match value {
        Value::Operand(x) => {
            w.byte(0);
            n.operand(w, *x);
        }
        Value::Primitive { op, args } => {
            w.byte(1);
            w.byte(op.code());
            for &x in args {
                n.operand(w, x);
            }
        }
        Value::Call { callee, args } => {
            w.byte(2);
            n.operand(w, *callee);
            n.operands(w, args);
        }
        Value::Closure { function, free } => {
            w.byte(3);
            w.u32(function.0);
            n.operands(w, free);
        }
        Value::Global(g) => {
            w.byte(4);
            w.u32(g.0);
        }
        Value::SetGlobal(g, x) => {
            w.byte(5);
            w.u32(g.0);
            n.operand(w, *x);
        }
        Value::Box(x) => {
            w.byte(6);
            n.operand(w, *x);
        }
        Value::Unbox(x) => {
            w.byte(7);
            n.operand(w, *x);
        }
        Value::SetBox(a, b) => {
            w.byte(8);
            n.operand(w, *a);
            n.operand(w, *b);
        }
        Value::SetFree {
            closure,
            index,
            value,
        } => {
            w.byte(9);
            n.operand(w, *closure);
            w.u32(*index);
            n.operand(w, *value);
        }
        Value::CallDirect { function, args } => {
            w.byte(10);
            w.u32(function.0);
            n.operands(w, args);
        }
    }
}

/// The state of decoding one function.
struct Decoder {
    terms: Vec<Term>,
    vars: u32,
    joins: u32,
    /// The fields the next terms fill, the next on top.
    holes: Vec<(TermId, u8)>,
}

impl Decoder {
    fn var(&mut self) -> Var {
        self.vars += 1;
        Var(self.vars - 1)
    }

    fn operand(&self, r: &mut Reader<'_>) -> Result<Operand, DecodeError> {
        let n = r.uint()?;
        if n & 1 == 1 {
            let c = u32::try_from(n >> 1).map_err(|_| r.error("a constant index overflows"))?;
            return Ok(Operand::Const(ConstantId(c)));
        }
        let back = n >> 1;
        if back >= u64::from(self.vars) {
            return Err(r.error("a variable before the first"));
        }
        Ok(Operand::Var(Var(self.vars - 1 - back as u32)))
    }

    fn operands(&self, r: &mut Reader<'_>) -> Result<Vec<Operand>, DecodeError> {
        let n = r.count(1)?;
        (0..n).map(|_| self.operand(r)).collect()
    }

    fn position(r: &mut Reader<'_>) -> Result<Option<Position>, DecodeError> {
        let flag = r.byte()?;
        if flag == 0 {
            return Ok(None);
        }
        if flag > 2 {
            return Err(r.error(format!("{flag} is not a position flag")));
        }
        let (source, line, column) = (r.u16()?, r.u32()?, r.u32()?);
        let inlined = if flag == 2 {
            let n = r.u32()?;
            if n == 0 {
                return Err(r.error("an inlined position in no inlined copy"));
            }
            n
        } else {
            0
        };
        Ok(Some(Position {
            source,
            line,
            column,
            inlined,
        }))
    }

    fn value(&mut self, r: &mut Reader<'_>) -> Result<Value, DecodeError> {
        Ok(match r.byte()? {
            0 => Value::Operand(self.operand(r)?),
            1 => {
                let code = r.byte()?;
                let op = Primitive::from_code(code)
                    .ok_or_else(|| r.error(format!("unknown primitive {code}")))?;
                let args = (0..op.arity())
                    .map(|_| self.operand(r))
                    .collect::<Result<_, _>>()?;
                Value::Primitive { op, args }
            }
            2 => {
                let callee = self.operand(r)?;
                Value::Call {
                    callee,
                    args: self.operands(r)?,
                }
            }
            3 => {
                let function = FunId(r.u32()?);
                Value::Closure {
                    function,
                    free: self.operands(r)?,
                }
            }
            4 => Value::Global(GlobalId(r.u32()?)),
            5 => {
                let g = GlobalId(r.u32()?);
                Value::SetGlobal(g, self.operand(r)?)
            }
            6 => Value::Box(self.operand(r)?),
            7 => Value::Unbox(self.operand(r)?),
            8 => {
                let a = self.operand(r)?;
                Value::SetBox(a, self.operand(r)?)
            }
            9 => {
                let closure = self.operand(r)?;
                let index = r.u32()?;
                Value::SetFree {
                    closure,
                    index,
                    value: self.operand(r)?,
                }
            }
            10 => {
                let function = FunId(r.u32()?);
                Value::CallDirect {
                    function,
                    args: self.operands(r)?,
                }
            }
            b => return Err(r.error(format!("unknown value tag {b}"))),
        })
    }

    /// Adds `term`, filling the next hole.
    fn place(&mut self, r: &Reader<'_>, term: Term) -> Result<TermId, DecodeError> {
        let id = TermId(u32::try_from(self.terms.len()).map_err(|_| r.error("too many terms"))?);
        self.terms.push(term);
        if let Some((at, field)) = self.holes.pop() {
            match (&mut self.terms[at.0 as usize], field) {
                (Term::Let { body, .. }, 0) | (Term::Join { body, .. }, 2) => *body = id,
                (Term::Join { handler, .. }, 1) => *handler = id,
                (Term::If { then, .. }, 3) => *then = id,
                (Term::If { otherwise, .. }, 4) => *otherwise = id,
                _ => unreachable!("holes name their own fields"),
            }
        }
        Ok(id)
    }
}

fn decode_function(r: &mut Reader<'_>) -> Result<Function, DecodeError> {
    let name = if r.bool()? {
        Some(r.str()?.into())
    } else {
        None
    };
    let arity = Arity {
        required: r.byte()?,
        optional: r.byte()?,
        rest: r.bool()?,
    };
    let free_count = bounded(r, "free variables")?;
    let inlined_count = bounded(r, "inlined copies")?;
    let mut inlined = Vec::with_capacity(inlined_count);
    for _ in 0..inlined_count {
        let name = if r.bool()? {
            Some(r.str()?.into())
        } else {
            None
        };
        let call = Decoder::position(r)?;
        inlined.push(Inlined { name, call });
    }
    let mut d = Decoder {
        terms: Vec::new(),
        vars: 0,
        joins: 0,
        holes: Vec::new(),
    };
    let params: Vec<Var> = (0..arity.parameters()).map(|_| d.var()).collect();
    let free: Vec<Var> = (0..free_count).map(|_| d.var()).collect();
    let mut first = true;
    while first || !d.holes.is_empty() {
        first = false;
        match r.byte()? {
            0 => {
                let value = d.value(r)?;
                let position = Decoder::position(r)?;
                let var = d.var();
                let id = d.place(
                    r,
                    Term::Let {
                        var,
                        value,
                        body: HOLE,
                        position,
                    },
                )?;
                d.holes.push((id, 0));
            }
            1 => {
                let n = bounded(r, "join-point parameters")?;
                let recursive = r.bool()?;
                if d.vars as usize + n > u32::MAX as usize {
                    return Err(r.error("too many variables"));
                }
                d.joins += 1;
                let join = JoinId(d.joins - 1);
                let params = (0..n).map(|_| d.var()).collect();
                let id = d.place(
                    r,
                    Term::Join {
                        join,
                        params,
                        recursive,
                        handler: HOLE,
                        body: HOLE,
                    },
                )?;
                d.holes.push((id, 2));
                d.holes.push((id, 1));
            }
            2 => {
                let back = r.u32()?;
                if back >= d.joins {
                    return Err(r.error("a join point before the first"));
                }
                let join = JoinId(d.joins - 1 - back);
                let args = d.operands(r)?;
                d.place(r, Term::Jump { join, args })?;
            }
            3 => {
                let test = d.operand(r)?;
                let id = d.place(
                    r,
                    Term::If {
                        test,
                        then: HOLE,
                        otherwise: HOLE,
                    },
                )?;
                d.holes.push((id, 4));
                d.holes.push((id, 3));
            }
            4 => {
                let x = d.operand(r)?;
                d.place(r, Term::Return(x))?;
            }
            5 => {
                let callee = d.operand(r)?;
                let args = d.operands(r)?;
                let position = Decoder::position(r)?;
                d.place(
                    r,
                    Term::TailCall {
                        callee,
                        args,
                        position,
                    },
                )?;
            }
            6 => {
                let function = FunId(r.u32()?);
                let args = d.operands(r)?;
                let position = Decoder::position(r)?;
                d.place(
                    r,
                    Term::TailCallDirect {
                        function,
                        args,
                        position,
                    },
                )?;
            }
            b => return Err(r.error(format!("unknown term tag {b}"))),
        }
    }
    Ok(Function {
        name,
        arity,
        params,
        free,
        vars: d.vars,
        joins: d.joins,
        terms: d.terms,
        body: TermId(0),
        inlined,
    })
}
