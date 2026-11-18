//! From the resolved tree to the IR (ADR 0017), one function at a time.
//!
//! Each function's terms are built top-down. The builder keeps a hole: the
//! field of the last term placed that the next term fills, such as a
//! `let`'s body. Lowering an expression for its value leaves the hole open
//! after the `let`s that compute it; lowering one in tail position closes
//! it with a `return`, a `jump` or a tail call.
//!
//! An `if` whose value is wanted becomes a join point whose handler is
//! what follows: `join k(r): ...rest...` around an `if` whose arms jump to
//! `k`. Loops are closures here: the optimiser's contification makes a
//! named `let` a `rec join`.
//!
//! Variables follow ADR 0005 #4: every assigned variable is boxed (ADR
//! 0017 #5 leaves unboxing to the optimiser), and closures in a `letrec`
//! that capture a variable bound after them are patched with `set-free`.

use std::collections::HashMap;

use vali_dist::{
    Arity, Constant, ConstantId, Effects, FunId, Function, Global, GlobalId, GlobalLink, JoinId,
    Module, Operand, Position, Primitive, Term, TermId, Value, Var,
};
use vali_num::Number;
use vali_syntax::{Datum, DatumKind, Sources, Span};

use crate::CompileError;
use crate::ast::{Expr, Lambda, LambdaId, VarId, VarInfo};
use crate::expand::{RED_ZONE, STACK_GROWTH};

type Result<T> = std::result::Result<T, CompileError>;

/// The most elements a call to `vector` builds inline; longer ones call
/// the procedure.
const MAX_INLINE_VECTOR: usize = 8;

/// A term field not filled yet.
const HOLE: TermId = TermId(u32::MAX);

fn error(message: impl Into<String>, span: Option<Span>) -> CompileError {
    CompileError {
        message: message.into(),
        span,
        location: None,
    }
}

fn internal(message: impl Into<String>) -> CompileError {
    error(
        format!("internal error lowering to the IR: {}", message.into()),
        None,
    )
}

/// Which field of a term the next term fills.
#[derive(Clone, Copy, Debug)]
enum Field {
    LetBody,
    JoinHandler,
    JoinBody,
    Then,
    Otherwise,
}

/// Where the value of an expression in tail position goes.
#[derive(Clone, Copy, Debug)]
enum Tail {
    /// Returned from the function.
    Return,
    /// To this join point, as its one argument.
    Jump(JoinId),
}

/// One function being lowered.
#[derive(Default)]
struct Fun {
    terms: Vec<Term>,
    vars: u32,
    joins: u32,
    /// The body, once placed.
    root: Option<TermId>,
    /// Where the next term goes; `None` with a root means the code so far
    /// ends in a tail.
    hole: Option<(TermId, Field)>,
    /// Each lexical variable's operand: its value, or for a boxed
    /// variable, its box.
    env: HashMap<VarId, Operand>,
}

impl Fun {
    fn var(&mut self) -> Var {
        self.vars += 1;
        Var(self.vars - 1)
    }

    fn join(&mut self) -> JoinId {
        self.joins += 1;
        JoinId(self.joins - 1)
    }

    /// Places `term` in the hole, which closes; returns its id.
    fn place(&mut self, term: Term) -> Result<TermId> {
        let id = TermId(
            u32::try_from(self.terms.len())
                .map_err(|_| error("a function has too many terms", None))?,
        );
        self.terms.push(term);
        match self.hole.take() {
            None if self.root.is_none() => self.root = Some(id),
            None => return Err(internal("code after a tail")),
            Some((at, field)) => match (&mut self.terms[at.0 as usize], field) {
                (Term::Let { body, .. }, Field::LetBody)
                | (Term::Join { body, .. }, Field::JoinBody) => *body = id,
                (Term::Join { handler, .. }, Field::JoinHandler) => *handler = id,
                (Term::If { then, .. }, Field::Then) => *then = id,
                (Term::If { otherwise, .. }, Field::Otherwise) => *otherwise = id,
                _ => return Err(internal("a hole in the wrong kind of term")),
            },
        }
        Ok(id)
    }
}

/// The constants worth sharing in the pool.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Integer(i64),
    Boolean(bool),
    Character(char),
    EmptyList,
    Unspecified,
    String(Box<str>),
    Symbol(Box<str>),
    Keyword(Box<str>),
}

impl Key {
    fn of(constant: &Constant) -> Option<Key> {
        Some(match constant {
            Constant::Integer(n) => Key::Integer(*n),
            Constant::Boolean(b) => Key::Boolean(*b),
            Constant::Character(c) => Key::Character(*c),
            Constant::EmptyList => Key::EmptyList,
            Constant::Unspecified => Key::Unspecified,
            Constant::String(s) => Key::String(s.clone()),
            Constant::Symbol(s) => Key::Symbol(s.clone()),
            Constant::Keyword(s) => Key::Keyword(s.clone()),
            _ => return None,
        })
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
        DatumKind::Keyword(k) => Constant::Keyword(k.as_str().into()),
        DatumKind::Labelled(..) | DatumKind::LabelRef(_) => {
            return Err(error(
                "datum labels in constants are not supported yet",
                Some(d.span),
            ));
        }
        _ => return Err(error("unsupported constant", Some(d.span))),
    })
}

/// Whether a call to the `(vali primitive)` procedure `name` with `argc`
/// arguments is lowered to primitive operations.
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

/// `(not x)`, as `x`.
fn negated(e: &Expr) -> Option<&Expr> {
    match e {
        Expr::Call(callee, args, _) if matches!(&**callee, Expr::Free(n) if n == "not") => {
            match &args[..] {
                [x] => Some(x),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Lowers the `lambda` `body`, and every function inside it, to a module
/// named `name` that exports every variable it defines.
pub(crate) fn lower(
    vars: &[VarInfo],
    lambdas: &[Lambda],
    sources: &Sources,
    body: LambdaId,
    name: &str,
) -> Result<Module> {
    let mut lowerer = Lowerer {
        vars,
        lambdas,
        sources,
        constants: Vec::new(),
        keys: HashMap::new(),
        globals: Vec::new(),
        global_index: HashMap::new(),
        functions: Vec::new(),
        source_table: Vec::new(),
        spans: Vec::new(),
    };
    let body = lowerer.function(body)?;
    let exports = lowerer
        .globals
        .iter()
        .enumerate()
        .filter(|(_, global)| global.link == GlobalLink::Defined)
        .map(|(n, global)| (global.name.clone(), GlobalId(n as u32)))
        .collect();
    let functions = lowerer
        .functions
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| internal("a function was never finished"))?;
    Ok(Module {
        name: name.into(),
        constants: lowerer.constants,
        globals: lowerer.globals,
        exports,
        functions,
        body,
        sources: lowerer.source_table,
    })
}

struct Lowerer<'a> {
    vars: &'a [VarInfo],
    lambdas: &'a [Lambda],
    sources: &'a Sources,
    constants: Vec<Constant>,
    keys: HashMap<Key, ConstantId>,
    globals: Vec<Global>,
    global_index: HashMap<Global, GlobalId>,
    /// Every function, `None` while it is being lowered.
    functions: Vec<Option<Function>>,
    source_table: Vec<Box<str>>,
    /// The spans of the calls being lowered, one inside another.
    spans: Vec<Span>,
}

impl Lowerer<'_> {
    fn constant(&mut self, constant: Constant) -> Result<Operand> {
        let key = Key::of(&constant);
        if let Some(id) = key.as_ref().and_then(|key| self.keys.get(key)) {
            return Ok(Operand::Const(*id));
        }
        let id = ConstantId(
            u32::try_from(self.constants.len())
                .map_err(|_| error("a module has too many constants", None))?,
        );
        self.constants.push(constant);
        if let Some(key) = key {
            self.keys.insert(key, id);
        }
        Ok(Operand::Const(id))
    }

    fn unspecified(&mut self) -> Result<Operand> {
        self.constant(Constant::Unspecified)
    }

    fn global(&mut self, name: &str, link: GlobalLink) -> Result<GlobalId> {
        let global = Global {
            name: name.into(),
            link,
        };
        if let Some(id) = self.global_index.get(&global) {
            return Ok(*id);
        }
        let id = GlobalId(
            u32::try_from(self.globals.len())
                .map_err(|_| error("a module has too many variables", None))?,
        );
        self.globals.push(global.clone());
        self.global_index.insert(global, id);
        Ok(id)
    }

    /// Where the call being lowered is, if its source is known.
    fn position(&mut self) -> Option<Position> {
        let span = *self.spans.last()?;
        let (identity, line, column) = self.sources.line_column(span)?;
        let source = match self.source_table.iter().position(|s| **s == *identity) {
            Some(n) => n,
            None => {
                self.source_table.push((*identity).into());
                self.source_table.len() - 1
            }
        };
        Some(Position {
            source: u16::try_from(source).ok()?,
            line,
            column,
            inlined: 0,
        })
    }

    /// Binds a new variable to `value`, leaving the hole in its body.
    fn bind(&mut self, f: &mut Fun, value: Value) -> Result<Operand> {
        let var = f.var();
        let position = if value.effects().contains(Effects::RAISE) {
            self.position()
        } else {
            None
        };
        let id = f.place(Term::Let {
            var,
            value,
            body: HOLE,
            position,
        })?;
        f.hole = Some((id, Field::LetBody));
        Ok(Operand::Var(var))
    }

    /// Delivers `value` to `tail`, closing the hole.
    fn deliver(f: &mut Fun, value: Operand, tail: Tail) -> Result<()> {
        f.place(match tail {
            Tail::Return => Term::Return(value),
            Tail::Jump(join) => Term::Jump {
                join,
                args: vec![value],
            },
        })?;
        Ok(())
    }

    /// Lowers `lambda` `l` to a function of its own.
    fn function(&mut self, l: LambdaId) -> Result<FunId> {
        let id = FunId(
            u32::try_from(self.functions.len())
                .map_err(|_| error("a module has too many functions", None))?,
        );
        self.functions.push(None);
        let lambdas = self.lambdas;
        let lambda = &lambdas[l];
        let mut f = Fun::default();
        let mut free = Vec::new();
        for &v in &lambda.free {
            let var = f.var();
            free.push(var);
            f.env.insert(v, Operand::Var(var));
        }
        let required = u8::try_from(lambda.required.len())
            .map_err(|_| error("a procedure takes too many arguments", None))?;
        let mut params = Vec::new();
        for _ in lambda.required.iter().chain(&lambda.rest) {
            params.push(f.var());
        }
        for (&v, &param) in lambda.required.iter().chain(&lambda.rest).zip(&params) {
            let operand = if self.vars[v].boxed() {
                self.bind(&mut f, Value::Box(Operand::Var(param)))?
            } else {
                Operand::Var(param)
            };
            f.env.insert(v, operand);
        }
        self.tail(&mut f, &lambda.body, Tail::Return)?;
        let body = f.root.ok_or_else(|| internal("a function with no body"))?;
        if f.hole.is_some() {
            return Err(internal("a function body that does not end"));
        }
        self.functions[id.0 as usize] = Some(Function {
            name: lambda.name.as_deref().map(Into::into),
            arity: Arity {
                required,
                optional: 0,
                rest: lambda.rest.is_some(),
            },
            params,
            free,
            vars: f.vars,
            joins: f.joins,
            terms: f.terms,
            body,
            inlined: Vec::new(),
        });
        Ok(id)
    }

    /// The operand for lexical variable `v`: its value, or its box.
    fn location(&self, f: &Fun, v: VarId) -> Result<Operand> {
        f.env
            .get(&v)
            .copied()
            .ok_or_else(|| internal(format!("{} has no location", self.vars[v].name)))
    }

    fn local(&mut self, f: &mut Fun, v: VarId) -> Result<Operand> {
        let operand = self.location(f, v)?;
        if self.vars[v].boxed() {
            self.bind(f, Value::Unbox(operand))
        } else {
            Ok(operand)
        }
    }

    fn value(&mut self, f: &mut Fun, e: &Expr) -> Result<Operand> {
        stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || self.value_here(f, e))
    }

    fn tail(&mut self, f: &mut Fun, e: &Expr, tail: Tail) -> Result<()> {
        stacker::maybe_grow(RED_ZONE, STACK_GROWTH, || self.tail_here(f, e, tail))
    }

    fn values(&mut self, f: &mut Fun, es: &[Expr]) -> Result<Vec<Operand>> {
        es.iter().map(|e| self.value(f, e)).collect()
    }

    fn value_here(&mut self, f: &mut Fun, e: &Expr) -> Result<Operand> {
        match e {
            Expr::Unspecified => self.unspecified(),
            Expr::Const(d) => {
                let c = constant(d)?;
                self.constant(c)
            }
            Expr::Local(v) => self.local(f, *v),
            Expr::Global(name) => {
                let g = self.global(name, GlobalLink::Defined)?;
                self.bind(f, Value::Global(g))
            }
            Expr::Free(name) => {
                let g = self.global(name, GlobalLink::Primitive)?;
                self.bind(f, Value::Global(g))
            }
            Expr::ReplGlobal(name) => {
                let g = self.global(name, GlobalLink::Repl)?;
                self.bind(f, Value::Global(g))
            }
            Expr::Imported(library, name) => {
                let g = self.global(name, GlobalLink::Imported(library.as_str().into()))?;
                self.bind(f, Value::Global(g))
            }
            Expr::SetLocal(v, value) => {
                let x = self.value(f, value)?;
                if !self.vars[*v].boxed() {
                    return Err(internal("an assigned variable must be boxed"));
                }
                let cell = self.location(f, *v)?;
                self.bind(f, Value::SetBox(cell, x))?;
                self.unspecified()
            }
            Expr::SetGlobal(name, value) | Expr::SetFree(name, value) => {
                let x = self.value(f, value)?;
                let link = match e {
                    Expr::SetGlobal(..) => GlobalLink::Defined,
                    _ => GlobalLink::Repl,
                };
                let g = self.global(name, link)?;
                self.bind(f, Value::SetGlobal(g, x))?;
                self.unspecified()
            }
            Expr::If(c, t, otherwise) => {
                if let Some(x) = negated(c) {
                    return self.if_value(f, x, otherwise, t);
                }
                self.if_value(f, c, t, otherwise)
            }
            Expr::Begin(es) => {
                let (last, init) = es.split_last().ok_or_else(|| internal("an empty begin"))?;
                for e in init {
                    self.value(f, e)?;
                }
                self.value(f, last)
            }
            Expr::Lambda(l) => self.closure(f, *l),
            Expr::Let(bindings, body) => {
                self.let_(f, bindings)?;
                self.value(f, body)
            }
            Expr::Letrec(bindings, body) => {
                self.letrec(f, bindings)?;
                self.value(f, body)
            }
            Expr::Call(callee, args, span) => {
                self.spans.push(*span);
                let result = self.call(f, callee, args, None);
                self.spans.pop();
                result?.ok_or_else(|| internal("a call for its value ended in a tail"))
            }
        }
    }

    fn tail_here(&mut self, f: &mut Fun, e: &Expr, tail: Tail) -> Result<()> {
        match e {
            Expr::If(c, t, otherwise) => {
                let (c, t, otherwise) = match negated(c) {
                    Some(x) => (x, &**otherwise, &**t),
                    None => (&**c, &**t, &**otherwise),
                };
                let test = self.value(f, c)?;
                self.if_tail(f, test, t, otherwise, tail)
            }
            Expr::Begin(es) => {
                let (last, init) = es.split_last().ok_or_else(|| internal("an empty begin"))?;
                for e in init {
                    self.value(f, e)?;
                }
                self.tail(f, last, tail)
            }
            Expr::Let(bindings, body) => {
                self.let_(f, bindings)?;
                self.tail(f, body, tail)
            }
            Expr::Letrec(bindings, body) => {
                self.letrec(f, bindings)?;
                self.tail(f, body, tail)
            }
            Expr::Call(callee, args, span) => {
                self.spans.push(*span);
                let result = self.call(f, callee, args, Some(tail));
                self.spans.pop();
                match result? {
                    None => Ok(()),
                    Some(_) => Err(internal("a tail call left a value")),
                }
            }
            _ => {
                let x = self.value(f, e)?;
                Self::deliver(f, x, tail)
            }
        }
    }

    /// `if test then t else otherwise`, each arm in tail position.
    fn if_tail(
        &mut self,
        f: &mut Fun,
        test: Operand,
        t: &Expr,
        otherwise: &Expr,
        tail: Tail,
    ) -> Result<()> {
        let id = f.place(Term::If {
            test,
            then: HOLE,
            otherwise: HOLE,
        })?;
        f.hole = Some((id, Field::Then));
        self.tail(f, t, tail)?;
        f.hole = Some((id, Field::Otherwise));
        self.tail(f, otherwise, tail)
    }

    /// An `if` whose value is wanted: a join point for what follows, and
    /// arms that jump to it.
    fn if_value(&mut self, f: &mut Fun, c: &Expr, t: &Expr, otherwise: &Expr) -> Result<Operand> {
        let test = self.value(f, c)?;
        let (join, result) = self.open_join(f)?;
        self.if_tail(f, test, t, otherwise, Tail::Jump(join.1))?;
        f.hole = Some((join.0, Field::JoinHandler));
        Ok(result)
    }

    /// Places `join k(r)`, leaving the hole in its body; returns the term
    /// and the join point, and `r`.
    fn open_join(&mut self, f: &mut Fun) -> Result<((TermId, JoinId), Operand)> {
        let join = f.join();
        let result = f.var();
        let id = f.place(Term::Join {
            join,
            params: vec![result],
            recursive: false,
            handler: HOLE,
            body: HOLE,
        })?;
        f.hole = Some((id, Field::JoinBody));
        Ok(((id, join), Operand::Var(result)))
    }

    fn let_(&mut self, f: &mut Fun, bindings: &[(VarId, Expr)]) -> Result<()> {
        let inits = bindings
            .iter()
            .map(|(_, init)| self.value(f, init))
            .collect::<Result<Vec<_>>>()?;
        for ((v, _), init) in bindings.iter().zip(inits) {
            let operand = if self.vars[*v].boxed() {
                self.bind(f, Value::Box(init))?
            } else {
                init
            };
            f.env.insert(*v, operand);
        }
        Ok(())
    }

    /// `letrec*`: boxes first for assigned variables, then each init in
    /// order. A closure that captured a variable before it had its value
    /// gets it with `set-free` once it has.
    fn letrec(&mut self, f: &mut Fun, bindings: &[(VarId, Expr)]) -> Result<()> {
        for (v, _) in bindings {
            let unspecified = self.unspecified()?;
            let operand = if self.vars[*v].boxed() {
                self.bind(f, Value::Box(unspecified))?
            } else {
                unspecified
            };
            f.env.insert(*v, operand);
        }
        let mut closures: Vec<Option<(Operand, LambdaId)>> = Vec::new();
        for (v, init) in bindings {
            let x = self.value(f, init)?;
            if self.vars[*v].boxed() {
                let cell = self.location(f, *v)?;
                self.bind(f, Value::SetBox(cell, x))?;
                closures.push(None);
                continue;
            }
            f.env.insert(*v, x);
            closures.push(match init {
                Expr::Lambda(l) => Some((x, *l)),
                _ => None,
            });
            // The new closure is last, and ties to itself before it
            // patches the earlier ones: the IR validator requires a
            // closure's self-tie to come before any other use of it.
            for &(closure, l) in closures.iter().rev().flatten() {
                if let Some(index) = self.lambdas[l].free.iter().position(|w| w == v) {
                    self.bind(
                        f,
                        Value::SetFree {
                            closure,
                            index: index as u32,
                            value: x,
                        },
                    )?;
                }
            }
        }
        Ok(())
    }

    /// A new closure of `l`, capturing its free variables' locations.
    fn closure(&mut self, f: &mut Fun, l: LambdaId) -> Result<Operand> {
        let function = self.function(l)?;
        let free = self.lambdas[l]
            .free
            .iter()
            .map(|&v| self.location(f, v))
            .collect::<Result<Vec<_>>>()?;
        self.bind(f, Value::Closure { function, free })
    }

    /// A call: for its value with no `tail`, giving it back; in tail
    /// position otherwise, closing the hole.
    fn call(
        &mut self,
        f: &mut Fun,
        callee: &Expr,
        args: &[Expr],
        tail: Option<Tail>,
    ) -> Result<Option<Operand>> {
        if let Expr::Free(name) = callee
            && inlined(name, args.len())
        {
            let x = self.inline(f, name, args)?;
            return match tail {
                None => Ok(Some(x)),
                Some(tail) => Self::deliver(f, x, tail).map(|()| None),
            };
        }
        let callee = self.value(f, callee)?;
        let args = self.values(f, args)?;
        match tail {
            Some(Tail::Return) => {
                let position = self.position();
                f.place(Term::TailCall {
                    callee,
                    args,
                    position,
                })?;
                Ok(None)
            }
            Some(Tail::Jump(join)) => {
                let x = self.bind(f, Value::Call { callee, args })?;
                Self::deliver(f, x, Tail::Jump(join)).map(|()| None)
            }
            None => self.bind(f, Value::Call { callee, args }).map(Some),
        }
    }

    fn primitive(&mut self, f: &mut Fun, op: Primitive, args: Vec<Operand>) -> Result<Operand> {
        self.bind(f, Value::Primitive { op, args })
    }

    /// A call to a standard procedure that `inlined` accepts, as
    /// primitive operations.
    fn inline(&mut self, f: &mut Fun, name: &str, args: &[Expr]) -> Result<Operand> {
        match (name, args.len()) {
            (">" | ">=", 2) => {
                let a = self.value(f, &args[0])?;
                let b = self.value(f, &args[1])?;
                let op = if name == ">" {
                    Primitive::Lt
                } else {
                    Primitive::Le
                };
                self.primitive(f, op, vec![b, a])
            }
            ("set-car!" | "set-cdr!" | "vector-set!", _) => {
                let op = Primitive::named(name).ok_or_else(|| internal(name))?;
                let args = self.values(f, args)?;
                self.primitive(f, op, args)?;
                self.unspecified()
            }
            ("vector", n) => {
                let elements = self.values(f, args)?;
                let len = self.constant(Constant::Integer(n as i64))?;
                let vector = self.primitive(f, Primitive::MakeVector, vec![len, elements[0]])?;
                for (i, &element) in elements.iter().enumerate().skip(1) {
                    if element != elements[0] {
                        let index = self.constant(Constant::Integer(i as i64))?;
                        self.primitive(f, Primitive::VectorSet, vec![vector, index, element])?;
                    }
                }
                Ok(vector)
            }
            ("+" | "*", 0) => {
                let unit = if name == "+" { 0 } else { 1 };
                self.constant(Constant::Integer(unit))
            }
            // Negation, as -1 times x rather than 0 minus x: (- 0.0) is
            // -0.0.
            ("-", 1) => {
                let x = self.value(f, &args[0])?;
                let minus_one = self.constant(Constant::Integer(-1))?;
                self.primitive(f, Primitive::Mul, vec![minus_one, x])
            }
            ("+" | "*", 1) => self.value(f, &args[0]),
            ("+" | "*" | "-", _) => {
                let op = Primitive::named(name).ok_or_else(|| internal(name))?;
                let mut total = self.value(f, &args[0])?;
                for arg in &args[1..] {
                    let x = self.value(f, arg)?;
                    total = self.primitive(f, op, vec![total, x])?;
                }
                Ok(total)
            }
            _ => {
                let op = Primitive::named(name).ok_or_else(|| internal(name))?;
                let args = self.values(f, args)?;
                self.primitive(f, op, args)
            }
        }
    }
}
