//! Modules and functions: what the VM loads.

use std::fmt::Write;

use crate::instruction::{DecodeError, Instruction};

/// The run format's version. Any mismatch means rebuilding from source or
/// from the distribution artifact (ADR 0005 #9).
pub const FORMAT_VERSION: u32 = 4;

/// How many arguments a function takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Arity {
    /// Arguments that must be given.
    pub required: u8,
    /// Arguments that may be omitted; an omitted one holds the default
    /// object.
    pub optional: u8,
    /// Whether the remaining arguments are collected into a list.
    pub rest: bool,
}

impl Arity {
    /// Exactly `n` arguments.
    pub const fn exactly(n: u8) -> Arity {
        Arity {
            required: n,
            optional: 0,
            rest: false,
        }
    }

    /// Whether `argc` arguments are acceptable.
    pub fn accepts(self, argc: usize) -> bool {
        let required = usize::from(self.required);
        argc >= required && (self.rest || argc <= required + usize::from(self.optional))
    }

    /// How many slots the parameters take after the closure in slot 0:
    /// required, optional, and one for the rest list.
    pub fn parameter_slots(self) -> usize {
        usize::from(self.required) + usize::from(self.optional) + usize::from(self.rest)
    }
}

/// A constant in a module's constant pool. Heap constants are created in
/// each isolate when the module loads there.
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    /// An exact integer.
    Integer(i64),
    /// A flonum.
    Flonum(f64),
    /// `#t` or `#f`.
    Boolean(bool),
    /// A character.
    Character(char),
    /// The empty list.
    EmptyList,
    /// The unspecified value.
    Unspecified,
    /// An immutable string.
    String(Box<str>),
    /// A symbol.
    Symbol(Box<str>),
    /// An SRFI 88 keyword, named without its colon.
    Keyword(Box<str>),
    /// A pair of constants.
    Pair(Box<Constant>, Box<Constant>),
    /// An immutable vector of constants.
    Vector(Vec<Constant>),
    /// An exact integer beyond 64 bits: its sign and magnitude, least
    /// significant limb first.
    Bignum {
        /// Whether it is negative.
        negative: bool,
        /// The magnitude's limbs.
        limbs: Vec<u64>,
    },
    /// An immutable bytevector.
    Bytevector(Vec<u8>),
    /// An exact rational that is not an integer: its numerator and
    /// denominator, integer constants in lowest terms.
    Ratio(Box<Constant>, Box<Constant>),
    /// A complex number: its real and imaginary parts, real constants that
    /// are both exact or both flonums.
    Complex(Box<Constant>, Box<Constant>),
}

/// One function's code and metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    /// Its name, for backtraces and debuggers.
    pub name: Option<Box<str>>,
    /// The arguments it takes.
    pub arity: Arity,
    /// How many slots its frame has, slot 0 (the closure) included.
    pub slots: u16,
    /// How many free variables its closures capture.
    pub free: u16,
    /// Its instructions.
    pub code: Vec<u32>,
    /// Where its instructions came from, sorted by instruction: each
    /// position covers its instruction and those after it, up to the next
    /// (ADR 0014 #9).
    pub positions: Vec<Position>,
}

/// Where an instruction came from: its index in the code, then a source
/// in the module's table, a line from 1 and a column from 0, counting
/// Unicode scalar values (ADR 0014 #9).
/// C layout, so debuggers can read line tables in place.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    /// The instruction's index.
    pub pc: u32,
    /// The source, an index into [`Module::sources`].
    pub source: u16,
    /// The line, from 1.
    pub line: u32,
    /// The column, from 0.
    pub column: u32,
}

/// A compiled module: what one library's body compiles to.
#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    /// The module's name.
    pub name: Box<str>,
    /// Its constant pool.
    pub constants: Vec<Constant>,
    /// Its variables. Code refers to them by index; loading links each to
    /// a cell (ADR 0005 #4).
    pub variables: Vec<Variable>,
    /// The variables other modules can import, by exported name.
    pub exports: Vec<Export>,
    /// Its functions. `closure` refers to them by index.
    pub functions: Vec<Function>,
    /// The identities of the sources its functions' positions are in.
    pub sources: Vec<Box<str>>,
    /// The function that runs the module's body.
    pub body: u32,
}

/// One entry of a module's variable table.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Variable {
    /// Its name: for an imported variable, the name its library exports
    /// it under.
    pub name: Box<str>,
    /// Where its cell is.
    pub link: Link,
}

/// Where a variable's cell is, once its module is loaded.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Link {
    /// The module defines it: its instance has the cell.
    Defined,
    /// The library with this identity exports it: the cell is that
    /// library's instance's.
    Imported(Box<str>),
    /// The isolate's global of that name: a REPL's variable, or one a host
    /// defines for bytecode it builds itself.
    Global,
    /// Vali's procedure implemented in Rust of that name, which `(vali
    /// primitive)` exports. Every isolate has one table of them.
    Primitive,
    /// The function of that name in the native library with this identity
    /// (ADR 0016), as a procedure, from when the module loads. Only the
    /// module Vali generates for a native library links this way.
    Native(Box<str>),
}

/// A variable a module exports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    /// The name importers use.
    pub name: Box<str>,
    /// The variable, by index.
    pub variable: u16,
}

/// The instructions of `code`, each with the index of its first word.
pub fn instructions(
    code: &[u32],
) -> impl Iterator<Item = Result<(usize, Instruction), DecodeError>> + '_ {
    let mut pc = 0;
    let mut failed = false;
    std::iter::from_fn(move || {
        if failed || pc >= code.len() {
            return None;
        }
        match Instruction::decode(code, pc) {
            Ok((instruction, len)) => {
                let at = pc;
                pc += len;
                Some(Ok((at, instruction)))
            }
            Err(e) => {
                failed = true;
                Some(Err(e))
            }
        }
    })
}

/// The target of a branch at `pc`, `len` words long, with `offset`.
pub fn branch_target(pc: usize, len: usize, offset: i32) -> Option<usize> {
    (pc + len).checked_add_signed(offset as isize)
}

/// A readable listing of `function`, one instruction per line, with each
/// branch's absolute target.
pub fn disassemble(function: &Function) -> String {
    let mut out = String::new();
    let name = function.name.as_deref().unwrap_or("<anonymous>");
    let a = function.arity;
    let _ = writeln!(
        out,
        "{name}: arity {}{}{}, {} slots, {} free",
        a.required,
        if a.optional > 0 {
            format!(" +{}", a.optional)
        } else {
            String::new()
        },
        if a.rest { " . rest" } else { "" },
        function.slots,
        function.free,
    );
    let mut pc = 0;
    while pc < function.code.len() {
        match Instruction::decode(&function.code, pc) {
            Ok((instruction, len)) => {
                let _ = write!(out, "  {pc:04}  {instruction}");
                if let Some(target) = instruction
                    .offset()
                    .and_then(|offset| branch_target(pc, len, offset))
                {
                    let _ = write!(out, "  ; -> {target:04}");
                }
                out.push('\n');
                pc += len;
            }
            Err(e) => {
                let _ = writeln!(out, "  {pc:04}  <{e}>");
                break;
            }
        }
    }
    out
}
