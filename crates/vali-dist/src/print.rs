//! A readable form of the IR, for tests and debugging. It is not parsed
//! back: the encoding is the format (ADR 0017 #8).
//!
//! ```text
//! fun f1 loop (v0 v1) []:
//!   let v2 = null? v0
//!   if v2:
//!     return v1
//!   else:
//!     let v3 = cdr v0 @0:4:12
//!     tail v4(v3 v1) @0:4:6
//! ```
//!
//! A `let` and a `join`'s body continue at the same indentation; a join's
//! handler and the arms of an `if` are indented. Printing is iterative, so
//! deep nesting cannot overflow the stack.

use std::fmt::{self, Write as _};

use crate::ir::{Constant, Function, GlobalLink, Module, Operand, Position, Term, TermId, Value};

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Var(var) => write!(f, "v{}", var.0),
            Operand::Const(constant) => write!(f, "c{}", constant.0),
        }
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}:{}:{}", self.source, self.line, self.column)
    }
}

/// One step of printing a constant.
enum Datum<'a> {
    Constant(&'a Constant),
    /// What follows a list's car: its cdr.
    Tail(&'a Constant),
    Text(&'static str),
}

impl fmt::Display for Constant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut pending = vec![Datum::Constant(self)];
        while let Some(datum) = pending.pop() {
            match datum {
                Datum::Text(text) => f.write_str(text)?,
                Datum::Tail(Constant::EmptyList) => f.write_str(")")?,
                Datum::Tail(Constant::Pair(car, cdr)) => {
                    f.write_str(" ")?;
                    pending.push(Datum::Tail(cdr));
                    pending.push(Datum::Constant(car));
                }
                Datum::Tail(other) => {
                    f.write_str(" . ")?;
                    pending.push(Datum::Text(")"));
                    pending.push(Datum::Constant(other));
                }
                Datum::Constant(constant) => match constant {
                    Constant::Integer(n) => write!(f, "{n}")?,
                    Constant::Bignum { negative, limbs } => {
                        f.write_str(if *negative { "#bignum(-" } else { "#bignum(+" })?;
                        for limb in limbs {
                            write!(f, " #x{limb:x}")?;
                        }
                        f.write_str(")")?;
                    }
                    Constant::Ratio(numerator, denominator) => {
                        pending.push(Datum::Constant(denominator));
                        pending.push(Datum::Text("/"));
                        pending.push(Datum::Constant(numerator));
                    }
                    Constant::Flonum(x) if x.is_nan() => f.write_str("+nan.0")?,
                    Constant::Flonum(x) if x.is_infinite() => {
                        f.write_str(if *x > 0.0 { "+inf.0" } else { "-inf.0" })?;
                    }
                    Constant::Flonum(x) => write!(f, "{x:?}")?,
                    Constant::Complex(real, imaginary) => {
                        f.write_str("#complex(")?;
                        pending.push(Datum::Text(")"));
                        pending.push(Datum::Constant(imaginary));
                        pending.push(Datum::Text(" "));
                        pending.push(Datum::Constant(real));
                    }
                    Constant::Boolean(true) => f.write_str("#t")?,
                    Constant::Boolean(false) => f.write_str("#f")?,
                    Constant::Character(c) if c.is_alphanumeric() || c.is_ascii_punctuation() => {
                        write!(f, "#\\{c}")?;
                    }
                    Constant::Character(c) => write!(f, "#\\x{:x}", u32::from(*c))?,
                    Constant::EmptyList => f.write_str("()")?,
                    Constant::Unspecified => f.write_str("#<unspecified>")?,
                    Constant::String(s) => write!(f, "{s:?}")?,
                    Constant::Symbol(s) => f.write_str(s)?,
                    Constant::Keyword(s) => write!(f, "{s}:")?,
                    Constant::Pair(car, cdr) => {
                        f.write_str("(")?;
                        pending.push(Datum::Tail(cdr));
                        pending.push(Datum::Constant(car));
                    }
                    Constant::Vector(items) => {
                        f.write_str("#(")?;
                        pending.push(Datum::Text(")"));
                        for (n, item) in items.iter().enumerate().rev() {
                            pending.push(Datum::Constant(item));
                            if n > 0 {
                                pending.push(Datum::Text(" "));
                            }
                        }
                    }
                    Constant::Bytevector(bytes) => {
                        f.write_str("#u8(")?;
                        for (n, byte) in bytes.iter().enumerate() {
                            if n > 0 {
                                f.write_str(" ")?;
                            }
                            write!(f, "{byte}")?;
                        }
                        f.write_str(")")?;
                    }
                },
            }
        }
        Ok(())
    }
}

fn list(operands: &[Operand]) -> String {
    let mut out = String::new();
    for (n, operand) in operands.iter().enumerate() {
        if n > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{operand}");
    }
    out
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Operand(operand) => write!(f, "{operand}"),
            Value::Primitive { op, args } => write!(f, "{op} {}", list(args)),
            Value::Call { callee, args } => write!(f, "call {callee}({})", list(args)),
            Value::Closure { function, free } => {
                write!(f, "closure f{} [{}]", function.0, list(free))
            }
            Value::Global(global) => write!(f, "global g{}", global.0),
            Value::SetGlobal(global, value) => write!(f, "set-global g{} {value}", global.0),
            Value::Box(value) => write!(f, "box {value}"),
            Value::Unbox(value) => write!(f, "unbox {value}"),
            Value::SetBox(target, value) => write!(f, "set-box {target} {value}"),
            Value::SetFree {
                closure,
                index,
                value,
            } => write!(f, "set-free {closure} {index} {value}"),
        }
    }
}

fn located(position: &Option<Position>) -> String {
    position.map_or_else(String::new, |position| format!(" {position}"))
}

/// One step of printing a function's body.
enum Line {
    Term(TermId, usize),
    Text(&'static str, usize),
}

fn indent(f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
    write!(f, "{:width$}", "", width = depth * 2)
}

impl Function {
    fn print(&self, n: usize, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fun f{n}")?;
        if let Some(name) = &self.name {
            write!(f, " {name}")?;
        }
        f.write_str(" (")?;
        let required = usize::from(self.arity.required);
        let optional = required + usize::from(self.arity.optional);
        for (i, param) in self.params.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            if i == required && optional > required {
                f.write_str("#!optional ")?;
            }
            if i == optional && self.arity.rest {
                f.write_str("#!rest ")?;
            }
            write!(f, "v{}", param.0)?;
        }
        let free: Vec<Operand> = self.free.iter().copied().map(Operand::Var).collect();
        writeln!(f, ") [{}]:", list(&free))?;
        let mut lines = vec![Line::Term(self.body, 1)];
        while let Some(line) = lines.pop() {
            let (id, depth) = match line {
                Line::Text(text, depth) => {
                    indent(f, depth)?;
                    writeln!(f, "{text}")?;
                    continue;
                }
                Line::Term(id, depth) => (id, depth),
            };
            indent(f, depth)?;
            match self.term(id) {
                Term::Let {
                    var,
                    value,
                    body,
                    position,
                } => {
                    writeln!(f, "let v{} = {value}{}", var.0, located(position))?;
                    lines.push(Line::Term(*body, depth));
                }
                Term::Join {
                    join,
                    params,
                    recursive,
                    handler,
                    body,
                } => {
                    let params: Vec<Operand> = params.iter().copied().map(Operand::Var).collect();
                    let rec = if *recursive { "rec " } else { "" };
                    writeln!(f, "{rec}join j{}({}):", join.0, list(&params))?;
                    lines.push(Line::Term(*body, depth));
                    lines.push(Line::Term(*handler, depth + 1));
                }
                Term::Jump { join, args } => writeln!(f, "jump j{}({})", join.0, list(args))?,
                Term::If {
                    test,
                    then,
                    otherwise,
                } => {
                    writeln!(f, "if {test}:")?;
                    lines.push(Line::Term(*otherwise, depth + 1));
                    lines.push(Line::Text("else:", depth));
                    lines.push(Line::Term(*then, depth + 1));
                }
                Term::Return(value) => writeln!(f, "return {value}")?,
                Term::TailCall {
                    callee,
                    args,
                    position,
                } => writeln!(f, "tail {callee}({}){}", list(args), located(position))?,
            }
        }
        Ok(())
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "module {}", self.name)?;
        for (n, source) in self.sources.iter().enumerate() {
            writeln!(f, "source {n} {source:?}")?;
        }
        for (n, constant) in self.constants.iter().enumerate() {
            writeln!(f, "c{n} = {constant}")?;
        }
        for (n, global) in self.globals.iter().enumerate() {
            write!(f, "g{n} = {} ", global.name)?;
            match &global.link {
                GlobalLink::Defined => writeln!(f, "defined")?,
                GlobalLink::Imported(library) => writeln!(f, "imported {library}")?,
                GlobalLink::Primitive => writeln!(f, "primitive")?,
                GlobalLink::Native(library) => writeln!(f, "native {library}")?,
                GlobalLink::Repl => writeln!(f, "repl")?,
            }
        }
        for (name, global) in &self.exports {
            writeln!(f, "export {name} = g{}", global.0)?;
        }
        writeln!(f, "body f{}", self.body.0)?;
        for (n, function) in self.functions.iter().enumerate() {
            writeln!(f)?;
            function.print(n, f)?;
        }
        Ok(())
    }
}
