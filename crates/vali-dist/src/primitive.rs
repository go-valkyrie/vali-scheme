//! Primitive operations and their effects (ADR 0017 #3 and #4).

use std::fmt;
use std::ops::BitOr;

/// What an operation may do, beyond computing its result. The empty set is
/// pure: an operation with no effects can be removed if its result is
/// unused, merged with an equal one, or moved anywhere its operands are in
/// scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Effects(u8);

impl Effects {
    /// No effects.
    pub const PURE: Effects = Effects(0);
    /// Allocates a new object, so two results are not `eq?`.
    pub const ALLOC: Effects = Effects(1);
    /// Reads mutable state.
    pub const READ: Effects = Effects(1 << 1);
    /// Writes mutable state.
    pub const WRITE: Effects = Effects(1 << 2);
    /// May raise an exception.
    pub const RAISE: Effects = Effects(1 << 3);
    /// May capture a continuation, which can return through it again.
    pub const CAPTURE: Effects = Effects(1 << 4);
    /// Every effect: what an unknown call may do.
    pub const ALL: Effects = Effects(0b1_1111);

    /// Whether every effect in `other` is in this set.
    pub const fn contains(self, other: Effects) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether the set is empty.
    pub const fn is_pure(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for Effects {
    type Output = Effects;

    fn bitor(self, other: Effects) -> Effects {
        Effects(self.0 | other.0)
    }
}

impl fmt::Display for Effects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_pure() {
            return f.write_str("pure");
        }
        let names = [
            (Effects::ALLOC, "alloc"),
            (Effects::READ, "read"),
            (Effects::WRITE, "write"),
            (Effects::RAISE, "may-raise"),
            (Effects::CAPTURE, "may-capture"),
        ];
        let mut first = true;
        for (effect, name) in names {
            if self.contains(effect) {
                if !first {
                    f.write_str(" ")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

macro_rules! primitives {
    ($($name:ident = $spelling:literal, $arity:literal, $effects:expr;)*) => {
        /// An operation on values that the format defines. Each takes a
        /// fixed number of operands; the compiler writes a call with more,
        /// such as `(+ a b c)`, as a chain. Each is encoded as its place in
        /// this list, so new ones only ever go at the end.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Primitive {
            $(
                #[doc = concat!("`", $spelling, "`")]
                $name,
            )*
        }

        impl Primitive {
            /// Every primitive operation.
            pub const ALL: &'static [Primitive] = &[$(Primitive::$name),*];

            /// Its name in the format: the standard procedure it computes.
            pub const fn name(self) -> &'static str {
                match self {
                    $(Primitive::$name => $spelling,)*
                }
            }

            /// The primitive operation named `name`, if there is one.
            pub fn named(name: &str) -> Option<Primitive> {
                match name {
                    $($spelling => Some(Primitive::$name),)*
                    _ => None,
                }
            }

            /// Its number in the encoding.
            pub fn code(self) -> u8 {
                Primitive::ALL
                    .iter()
                    .position(|&p| p == self)
                    .expect("every primitive is listed") as u8
            }

            /// The primitive numbered `code`, if there is one.
            pub fn from_code(code: u8) -> Option<Primitive> {
                Primitive::ALL.get(usize::from(code)).copied()
            }

            /// How many operands it takes.
            pub const fn arity(self) -> usize {
                match self {
                    $(Primitive::$name => $arity,)*
                }
            }

            /// What it may do.
            pub const fn effects(self) -> Effects {
                match self {
                    $(Primitive::$name => $effects,)*
                }
            }
        }
    };
}

const fn union(a: Effects, b: Effects) -> Effects {
    Effects(a.0 | b.0)
}

const RAISE: Effects = Effects::RAISE;
const NUMERIC: Effects = union(Effects::ALLOC, Effects::RAISE);
const ACCESS: Effects = union(Effects::READ, Effects::RAISE);
const MUTATE: Effects = union(Effects::WRITE, Effects::RAISE);

primitives! {
    Add = "+", 2, NUMERIC;
    Sub = "-", 2, NUMERIC;
    Mul = "*", 2, NUMERIC;
    Lt = "<", 2, RAISE;
    Le = "<=", 2, RAISE;
    NumEq = "=", 2, RAISE;
    Eq = "eq?", 2, Effects::PURE;
    Not = "not", 1, Effects::PURE;
    Cons = "cons", 2, Effects::ALLOC;
    Car = "car", 1, ACCESS;
    Cdr = "cdr", 1, ACCESS;
    SetCar = "set-car!", 2, MUTATE;
    SetCdr = "set-cdr!", 2, MUTATE;
    IsNull = "null?", 1, Effects::PURE;
    IsPair = "pair?", 1, Effects::PURE;
    MakeVector = "make-vector", 2, NUMERIC;
    VectorRef = "vector-ref", 2, ACCESS;
    VectorSet = "vector-set!", 3, MUTATE;
    VectorLength = "vector-length", 1, RAISE;
}

impl fmt::Display for Primitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
