//! The IR's types (ADR 0017).

use crate::primitive::{Effects, Primitive};

/// A variable: bound once, by its function's parameters, a [`Term::Let`],
/// or a [`Term::Join`]'s parameters. Numbered within its function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Var(pub u32);

/// A join point: a continuation local to its function, only jumped to.
/// Numbered within its function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JoinId(pub u32);

/// A function, by its index in its module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FunId(pub u32);

/// A term, by its index in its function's arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TermId(pub u32);

/// A constant, by its index in its module's pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConstantId(pub u32);

/// A module-level variable, by its index in its module's table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlobalId(pub u32);

/// What an operation takes: a variable or a constant. Operands never
/// compute, so the order things happen in is the order of the `let`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operand {
    Var(Var),
    Const(ConstantId),
}

/// The arguments a function takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Arity {
    /// Arguments that must be given.
    pub required: u8,
    /// Arguments that may be omitted; an omitted one is the default object.
    pub optional: u8,
    /// Whether the rest are collected into a list.
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

    /// Whether it takes `n` arguments.
    pub fn accepts(self, n: usize) -> bool {
        let required = usize::from(self.required);
        n >= required && (self.rest || n <= required + usize::from(self.optional))
    }

    /// How many parameters bind its arguments: required, optional, and one
    /// for the rest list.
    pub fn parameters(self) -> usize {
        usize::from(self.required) + usize::from(self.optional) + usize::from(self.rest)
    }
}

/// A constant in a module's pool.
#[derive(Debug)]
pub enum Constant {
    /// An exact integer that fits in 64 bits.
    Integer(i64),
    /// An exact integer beyond 64 bits: its sign and magnitude, least
    /// significant limb first.
    Bignum {
        negative: bool,
        limbs: Vec<u64>,
    },
    /// An exact rational that is not an integer: numerator and denominator,
    /// integer constants in lowest terms.
    Ratio(Box<Constant>, Box<Constant>),
    /// A flonum.
    Flonum(f64),
    /// A complex number: real and imaginary parts, both exact or both
    /// flonums.
    Complex(Box<Constant>, Box<Constant>),
    Boolean(bool),
    Character(char),
    EmptyList,
    Unspecified,
    /// An immutable string.
    String(Box<str>),
    Symbol(Box<str>),
    /// An SRFI 88 keyword, named without its colon.
    Keyword(Box<str>),
    /// An immutable pair of constants.
    Pair(Box<Constant>, Box<Constant>),
    /// An immutable vector of constants.
    Vector(Vec<Constant>),
    /// An immutable bytevector.
    Bytevector(Vec<u8>),
}

impl Constant {
    /// Moves its parts into `out`, leaving it with none.
    fn take_parts(&mut self, out: &mut Vec<Constant>) {
        match self {
            Constant::Ratio(a, b) | Constant::Complex(a, b) | Constant::Pair(a, b) => {
                out.push(std::mem::replace(&mut **a, Constant::EmptyList));
                out.push(std::mem::replace(&mut **b, Constant::EmptyList));
            }
            Constant::Vector(items) => out.append(items),
            _ => {}
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
            Constant::Ratio(..)
            | Constant::Complex(..)
            | Constant::Pair(..)
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
                (Constant::Vector(x), Constant::Vector(y)) if x.len() == y.len() => {
                    pending.extend(x.iter().zip(y).rev());
                }
                _ => return false,
            }
        }
        true
    }
}

/// Frees nested constants iteratively: a long quoted list in an artifact
/// must not overflow the stack.
impl Drop for Constant {
    fn drop(&mut self) {
        let mut parts = Vec::new();
        self.take_parts(&mut parts);
        while let Some(mut part) = parts.pop() {
            part.take_parts(&mut parts);
        }
    }
}

/// Where a module-level variable's cell is, once linked.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GlobalLink {
    /// The module defines it.
    Defined,
    /// The library with this identity exports it under the variable's name.
    Imported(Box<str>),
    /// Vali's procedure implemented in Rust of that name, which `(vali
    /// primitive)` exports.
    Primitive,
    /// The function of that name in the native library with this identity
    /// (ADR 0016).
    Native(Box<str>),
    /// The isolate's global of that name: a REPL's variable.
    Repl,
}

/// A module-level variable: its name and where its cell is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Global {
    pub name: Box<str>,
    pub link: GlobalLink,
}

/// Where code came from: a source in the module's table, a line from 1 and
/// a column from 0 (ADR 0014 #9), and the copy of an inlined function it
/// is in, if it is in one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Position {
    pub source: u16,
    pub line: u32,
    pub column: u32,
    /// 0 for the function's own code; `i + 1` for code inlined as its
    /// function's [`Function::inlined`] entry `i`.
    pub inlined: u32,
}

/// A function's body copied into another where it was called, so that a
/// backtrace can show the frame the call would have made.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Inlined {
    /// The name of the function copied.
    pub name: Option<Box<str>>,
    /// Where it was called, which is in an earlier copy or the function's
    /// own code: its `inlined` is less than this copy's own number.
    pub call: Option<Position>,
}

/// What a function's body is made of. Terms live in their function's
/// arena; a term refers to the ones it contains by index, and each is
/// contained by exactly one other, or is the body.
#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    /// `let var = value in body`.
    Let {
        var: Var,
        value: Value,
        body: TermId,
        /// Where `value` came from, if it can fail or call.
        position: Option<Position>,
    },
    /// `join join(params) = handler in body`, or with `recursive`, a loop:
    /// `handler` may jump to `join` too. `body` and `handler` may jump to
    /// it; nothing else can see it.
    Join {
        join: JoinId,
        params: Vec<Var>,
        recursive: bool,
        handler: TermId,
        body: TermId,
    },
    /// Continues at `join`, its parameters bound to `args`.
    Jump { join: JoinId, args: Vec<Operand> },
    /// Continues with `then` unless `test` is `#f`, otherwise with
    /// `otherwise`.
    If {
        test: Operand,
        then: TermId,
        otherwise: TermId,
    },
    /// Returns `value` from the function.
    Return(Operand),
    /// Calls `callee` with `args` in place of the function: its result is
    /// the function's.
    TailCall {
        callee: Operand,
        args: Vec<Operand>,
        position: Option<Position>,
    },
    /// Calls `function` with `args` in place of the function, as
    /// [`Value::CallDirect`] calls it.
    TailCallDirect {
        function: FunId,
        args: Vec<Operand>,
        position: Option<Position>,
    },
}

/// What a [`Term::Let`] binds its variable to.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// An operand, as it is.
    Operand(Operand),
    /// A primitive operation the format defines, on `args`.
    Primitive { op: Primitive, args: Vec<Operand> },
    /// Calls `callee` with `args`; what follows the `let` is where it
    /// returns to.
    Call { callee: Operand, args: Vec<Operand> },
    /// Calls the module's function `function`, which has no free variables
    /// and takes `args`, without a procedure check or a closure load
    /// (ADR 0017 #5): what calls to a library's own procedures become.
    CallDirect { function: FunId, args: Vec<Operand> },
    /// A new closure of `function`, capturing `free` as its free
    /// variables, in order.
    Closure { function: FunId, free: Vec<Operand> },
    /// Reads a module-level variable.
    Global(GlobalId),
    /// Assigns a module-level variable the module defines, or a REPL's.
    SetGlobal(GlobalId, Operand),
    /// A new box holding the operand: an assigned variable that must
    /// outlive a frame, or that a continuation could copy.
    Box(Operand),
    /// The value in a box.
    Unbox(Operand),
    /// Puts the second operand into the box the first holds.
    SetBox(Operand, Operand),
    /// Sets free variable `index` of the closure `closure` to `value`: how
    /// `letrec` ties closures that refer to each other.
    SetFree {
        closure: Operand,
        index: u32,
        value: Operand,
    },
}

impl Value {
    /// What computing it may do.
    pub fn effects(&self) -> Effects {
        match self {
            Value::Operand(_) => Effects::PURE,
            Value::Primitive { op, .. } => op.effects(),
            Value::Call { .. } | Value::CallDirect { .. } => Effects::ALL,
            Value::Closure { .. } | Value::Box(_) => Effects::ALLOC,
            Value::Global(_) => Effects::READ | Effects::RAISE,
            Value::Unbox(_) => Effects::READ,
            Value::SetGlobal(..) | Value::SetBox(..) => Effects::WRITE,
            Value::SetFree { .. } => Effects::WRITE | Effects::RAISE,
        }
    }
}

/// One function: its parameters, free variables, and body.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    /// Its name, for backtraces and debuggers.
    pub name: Option<Box<str>>,
    pub arity: Arity,
    /// The variables its parameters bind, `arity.parameters()` of them.
    pub params: Vec<Var>,
    /// The variables its free variables bind, in its closures' order.
    pub free: Vec<Var>,
    /// How many variables and join points it numbers.
    pub vars: u32,
    pub joins: u32,
    /// Its terms, the arena its body and their parts are in.
    pub terms: Vec<Term>,
    pub body: TermId,
    /// The copies of other functions inlined into it, which its positions
    /// say they are in.
    pub inlined: Vec<Inlined>,
}

impl Function {
    /// The term `id`.
    pub fn term(&self, id: TermId) -> &Term {
        &self.terms[id.0 as usize]
    }
}

/// What one library's or program's body compiles to.
#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    /// The unit's identity.
    pub name: Box<str>,
    pub constants: Vec<Constant>,
    pub globals: Vec<Global>,
    /// The globals other modules import, by the names they import them by.
    pub exports: Vec<(Box<str>, GlobalId)>,
    pub functions: Vec<Function>,
    /// The function that runs the module's body: no parameters, no free
    /// variables.
    pub body: FunId,
    /// The identities of the sources positions are in.
    pub sources: Vec<Box<str>>,
}
