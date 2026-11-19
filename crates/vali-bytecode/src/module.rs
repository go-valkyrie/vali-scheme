//! Modules and functions: what the VM loads.

use std::fmt::Write;

use crate::instruction::{DecodeError, Instruction};

/// The run format's version. Any mismatch means rebuilding from source or
/// from the distribution artifact (ADR 0005 #9).
pub const FORMAT_VERSION: u32 = 5;

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
#[derive(Debug)]
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
    /// A constant that other parts of the same top-level constant refer
    /// back to, or whose own parts do. Labels are numbered from 0 in the
    /// order a pre-order walk defines them, so a [`Constant::LabelRef`]
    /// follows the label it names or sits inside it. The tree has no
    /// cycle: the heap object built from it does.
    Labelled(u32, Box<Constant>),
    /// The constant labelled `n`: the same object, not a copy.
    LabelRef(u32),
}

/// One function's code and metadata.
#[derive(Clone, Debug, PartialEq, Hash)]
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
    /// The copies of other functions inlined into it, for backtraces.
    pub inlined: Vec<Inlined>,
    /// Which copy its instructions are in, sorted by instruction as
    /// `positions` is: each covers its instruction and those after it, up
    /// to the next. Before the first, and where none says otherwise, the
    /// function's own code.
    pub scopes: Vec<Scope>,
}

/// A function's body inlined into another where it was called: the frame
/// a backtrace shows for code in it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Inlined {
    /// The name of the function copied.
    pub name: Option<Box<str>>,
    /// Where it was called: a source in the module's table, a line and a
    /// column, as a [`Position`] has.
    pub call: Option<(u16, u32, u32)>,
    /// The copy the call was in: 0 for the function's own code, `i + 1`
    /// for [`Function::inlined`] entry `i`, which must come before this
    /// one.
    pub caller: u32,
}

/// From instruction `pc` on, the code is in copy `inlined`: 0 for the
/// function's own code, `i + 1` for [`Function::inlined`] entry `i`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scope {
    pub pc: u32,
    pub inlined: u32,
}

/// Where an instruction came from: its index in the code, then a source
/// in the module's table, a line from 1 and a column from 0, counting
/// Unicode scalar values (ADR 0014 #9).
/// C layout, so debuggers can read line tables in place.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
#[derive(Clone, Debug, PartialEq, Hash)]
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
    /// A digest of what the module was made from, if its maker has one:
    /// of the IR's encoding for a library the compiler made, so telling
    /// it from another build needs none of the converted code.
    /// [`Module::fingerprint`] is this when it is set.
    pub digest: Option<u128>,
}

impl std::hash::Hash for Constant {
    /// Floats by their bits, so a NaN's payload counts. That is on purpose,
    /// for [`Module::fingerprint`], and it disagrees with the derived
    /// `PartialEq` on floats: `0.0 == -0.0` but they hash differently, and
    /// `NaN != NaN` but equal bits hash alike. So `Eq` must not be added on
    /// this basis, and nothing may key a std map on a `Constant` or a
    /// `Module` expecting hashing and equality to agree.
    ///
    /// The parts are hashed from a stack of their own, so that a deeply
    /// nested constant cannot overflow the native stack.
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let mut pending = vec![self];
        while let Some(constant) = pending.pop() {
            std::mem::discriminant(constant).hash(state);
            match constant {
                Constant::Integer(n) => n.hash(state),
                Constant::Flonum(x) => x.to_bits().hash(state),
                Constant::Boolean(b) => b.hash(state),
                Constant::Character(c) => c.hash(state),
                Constant::EmptyList | Constant::Unspecified => {}
                Constant::String(s) | Constant::Symbol(s) | Constant::Keyword(s) => s.hash(state),
                Constant::Pair(a, b) | Constant::Ratio(a, b) | Constant::Complex(a, b) => {
                    pending.push(b);
                    pending.push(a);
                }
                Constant::Vector(items) => {
                    items.len().hash(state);
                    pending.extend(items.iter().rev());
                }
                Constant::Bignum { negative, limbs } => {
                    negative.hash(state);
                    limbs.hash(state);
                }
                Constant::Bytevector(bytes) => bytes.hash(state),
                Constant::Labelled(n, inner) => {
                    n.hash(state);
                    pending.push(inner);
                }
                Constant::LabelRef(n) => n.hash(state),
            }
        }
    }
}

impl Constant {
    /// The `index`th constant inside this one, if it has one.
    fn part(&self, index: usize) -> Option<&Constant> {
        match self {
            Constant::Ratio(a, b) | Constant::Complex(a, b) | Constant::Pair(a, b) => {
                [&**a, &**b].get(index).copied()
            }
            Constant::Vector(items) => items.get(index),
            Constant::Labelled(_, inner) => [&**inner].get(index).copied(),
            _ => None,
        }
    }

    /// A copy of a constant with nothing inside, or `None` if it has parts.
    fn clone_leaf(&self) -> Option<Constant> {
        Some(match self {
            Constant::Integer(n) => Constant::Integer(*n),
            Constant::Bignum { negative, limbs } => Constant::Bignum {
                negative: *negative,
                limbs: limbs.clone(),
            },
            Constant::Flonum(x) => Constant::Flonum(*x),
            Constant::Boolean(b) => Constant::Boolean(*b),
            Constant::Character(c) => Constant::Character(*c),
            Constant::EmptyList => Constant::EmptyList,
            Constant::Unspecified => Constant::Unspecified,
            Constant::String(s) => Constant::String(s.clone()),
            Constant::Symbol(s) => Constant::Symbol(s.clone()),
            Constant::Keyword(s) => Constant::Keyword(s.clone()),
            Constant::Bytevector(bytes) => Constant::Bytevector(bytes.clone()),
            Constant::LabelRef(n) => Constant::LabelRef(*n),
            Constant::Ratio(..)
            | Constant::Complex(..)
            | Constant::Pair(..)
            | Constant::Labelled(..)
            | Constant::Vector(_) => {
                return None;
            }
        })
    }

    /// A constant shaped like `self` from copies of its parts, in order.
    fn rebuild(&self, mut parts: Vec<Constant>) -> Constant {
        let mut two = || {
            let b = Box::new(parts.pop().expect("two parts"));
            let a = Box::new(parts.pop().expect("two parts"));
            (a, b)
        };
        match self {
            Constant::Ratio(..) => {
                let (a, b) = two();
                Constant::Ratio(a, b)
            }
            Constant::Complex(..) => {
                let (a, b) = two();
                Constant::Complex(a, b)
            }
            Constant::Pair(..) => {
                let (a, b) = two();
                Constant::Pair(a, b)
            }
            Constant::Labelled(n, _) => {
                Constant::Labelled(*n, Box::new(parts.pop().expect("one part")))
            }
            _ => Constant::Vector(parts),
        }
    }
}

/// Copies with a stack of its own, so that a long or deeply nested
/// constant, such as a quoted list of thousands of elements, cannot
/// overflow the native stack.
impl Clone for Constant {
    fn clone(&self) -> Constant {
        // The constants being copied, each with the index of its next part
        // and the copies of its parts so far.
        let mut stack: Vec<(&Constant, usize, Vec<Constant>)> = Vec::new();
        let mut copy = match self.clone_leaf() {
            Some(leaf) => Some(leaf),
            None => {
                stack.push((self, 0, Vec::new()));
                None
            }
        };
        loop {
            let Some((node, next, done)) = stack.last_mut() else {
                return copy.expect("the root copied");
            };
            if let Some(part) = copy.take() {
                done.push(part);
            }
            match node.part(*next) {
                Some(part) => {
                    *next += 1;
                    match part.clone_leaf() {
                        Some(leaf) => copy = Some(leaf),
                        None => stack.push((part, 0, Vec::new())),
                    }
                }
                None => {
                    let (node, _, done) = stack.pop().expect("a node being copied");
                    copy = Some(node.rebuild(done));
                }
            }
        }
    }
}

/// Compares with a stack of its own, as [`Clone`] copies.
impl PartialEq for Constant {
    fn eq(&self, other: &Constant) -> bool {
        let mut pending = vec![(self, other)];
        while let Some((a, b)) = pending.pop() {
            match (a, b) {
                (Constant::Integer(x), Constant::Integer(y)) if x == y => {}
                (
                    Constant::Bignum {
                        negative: n,
                        limbs: x,
                    },
                    Constant::Bignum {
                        negative: m,
                        limbs: y,
                    },
                ) if n == m && x == y => {}
                (Constant::Flonum(x), Constant::Flonum(y)) if x == y => {}
                (Constant::Boolean(x), Constant::Boolean(y)) if x == y => {}
                (Constant::Character(x), Constant::Character(y)) if x == y => {}
                (Constant::EmptyList, Constant::EmptyList)
                | (Constant::Unspecified, Constant::Unspecified) => {}
                (Constant::String(x), Constant::String(y))
                | (Constant::Symbol(x), Constant::Symbol(y))
                | (Constant::Keyword(x), Constant::Keyword(y))
                    if x == y => {}
                (Constant::Bytevector(x), Constant::Bytevector(y)) if x == y => {}
                (Constant::Ratio(a1, a2), Constant::Ratio(b1, b2))
                | (Constant::Complex(a1, a2), Constant::Complex(b1, b2))
                | (Constant::Pair(a1, a2), Constant::Pair(b1, b2)) => {
                    pending.push((a2, b2));
                    pending.push((a1, b1));
                }
                (Constant::Labelled(n, x), Constant::Labelled(m, y)) if n == m => {
                    pending.push((x, y));
                }
                (Constant::LabelRef(x), Constant::LabelRef(y)) if x == y => {}
                (Constant::Vector(x), Constant::Vector(y)) if x.len() == y.len() => {
                    pending.extend(x.iter().zip(y).rev());
                }
                _ => return false,
            }
        }
        true
    }
}

impl Constant {
    /// Moves its parts into `out`, leaving it with none.
    fn take_parts(&mut self, out: &mut Vec<Constant>) {
        match self {
            Constant::Ratio(a, b) | Constant::Complex(a, b) | Constant::Pair(a, b) => {
                out.push(std::mem::replace(&mut **a, Constant::EmptyList));
                out.push(std::mem::replace(&mut **b, Constant::EmptyList));
            }
            Constant::Labelled(_, inner) => {
                out.push(std::mem::replace(&mut **inner, Constant::EmptyList));
            }
            Constant::Vector(items) => out.append(items),
            _ => {}
        }
    }
}

/// Frees nested constants iteratively: a long quoted list must not
/// overflow the stack.
impl Drop for Constant {
    fn drop(&mut self) {
        let mut parts = Vec::new();
        self.take_parts(&mut parts);
        while let Some(mut part) = parts.pop() {
            part.take_parts(&mut parts);
        }
    }
}

impl Module {
    /// A 128-bit digest that tells two builds of a module apart. A host
    /// that has loaded a module and dropped its bytecode can still tell
    /// whether another is the same one. It is the module's
    /// [`digest`](Module::digest) if it has one: the compiler gives each
    /// library a digest of its IR's encoding, which is the same however
    /// the library was made, from source or from its artifact. Otherwise
    /// it is a digest of the whole module, code, constants and debug
    /// information included, the same for equal modules (floating point
    /// constants by their bits), deterministic within a build of Vali and
    /// not across builds. Not a defence against a module made to collide:
    /// it guards against mixing up two builds of a library by accident.
    pub fn fingerprint(&self) -> u128 {
        use std::hash::{Hash, Hasher};

        if let Some(digest) = self.digest {
            return digest;
        }
        /// Two hashers fed alike, one keyed differently.
        struct Digest(std::hash::DefaultHasher, std::hash::DefaultHasher);
        impl Hasher for Digest {
            fn write(&mut self, bytes: &[u8]) {
                self.0.write(bytes);
                self.1.write(bytes);
            }
            fn finish(&self) -> u64 {
                self.0.finish()
            }
        }
        let mut digest = Digest(Default::default(), Default::default());
        digest.1.write_u8(0xa5);
        self.hash(&mut digest);
        (u128::from(digest.0.finish()) << 64) | u128::from(digest.1.finish())
    }
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
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
