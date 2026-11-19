//! Hash functions for hash tables and comparators (SRFI 128, 125, 69): one
//! for each of `eq?`, `eqv?`, `equal?`, `=` on numbers, strings, strings
//! compared without case and symbols.
//!
//! Each returns a non-negative fixnum, and each agrees with the equivalence
//! it serves: values that are `equal?` have the same `equal-hash`, and so on
//! down. They are deterministic, so a hash does not change between runs.
//!
//! - **Numbers** hash by value across exactness, as `=` compares them: `2`,
//!   `2.0` and `2+0.0i` hash alike, and so do `1/2` and `0.5`.
//! - **Strings, bytevectors and symbols** hash by content. A symbol's hash is
//!   the one computed when it was interned.
//! - **Other objects** (records, procedures, ports and so on) hash by
//!   identity, through [`References::identity_hash`](crate::References).
//!   No strategy moves objects today, so that is the address; a strategy that
//!   moves them stores a hash in the object instead (ADR 0003).
//! - **Pairs and vectors** hash by content in `equal-hash`. The walk is
//!   iterative and visits at most [`NODE_BUDGET`] nodes in a fixed
//!   pre-order, so a deep or circular structure costs a bounded amount and
//!   never overflows the stack. The budget is spent in the order of the
//!   unrolled tree and the walk keeps no table of what it has seen, so
//!   `equal?` structures, which `equal?` compares as unrolled trees too,
//!   take the same walk and hash alike.

use vali_abi::{Tag, TypeCode};
use vali_bytecode::Arity;
use vali_num::Number;
use vali_num::arith;

use crate::code::NativeFn;
use crate::cx::{Cx, get};
use crate::handle::Error;
use crate::strategy::Strategy;
use crate::value::Value;
use crate::vm::wrong_type;

/// How many objects `equal-hash` looks at in one structure.
const NODE_BUDGET: usize = 128;

/// Hashes are 60 bits, so they are always non-negative fixnums.
const MASK: u64 = (1 << 60) - 1;

/// 2^63: the integral flonums in `-TWO_63..TWO_63` are the ones an `i64`
/// holds.
const TWO_63: f64 = 9_223_372_036_854_775_808.0;

/// SplitMix64's finalizer: spreads every input bit over the output.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Folds `x` into the running hash `h`; the order matters.
fn combine(h: u64, x: u64) -> u64 {
    mix(h.rotate_left(7) ^ x.wrapping_add(0x9e37_79b9_7f4a_7c15))
}

fn bytes_hash(seed: u64, bytes: &[u8]) -> u64 {
    // FNV-1a, finished by `mix`.
    let h = bytes.iter().fold(0xcbf2_9ce4_8422_2325 ^ seed, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    mix(h)
}

fn int_hash(n: i64) -> u64 {
    mix(n as u64 ^ 0x1357_9bdf)
}

/// A flonum's hash. An integral one hashes as the integer it equals.
fn real_hash(x: f64) -> u64 {
    if x.is_nan() {
        mix(0x7ff8_0000_0000_0001)
    } else if x == x.trunc() && (-TWO_63..TWO_63).contains(&x) {
        int_hash(x as i64)
    } else {
        mix(x.to_bits() ^ 0x2468_ace0)
    }
}

fn limbs_hash(negative: bool, limbs: &[u64]) -> u64 {
    limbs
        .iter()
        .fold(u64::from(negative), |h, &l| combine(h, l))
}

/// The hash of an exact `n` that is not a fixnum-sized integer: a flonum
/// that equals `n` exactly must hash alike, so a value a flonum can hold is
/// hashed as that flonum.
fn exact_hash(n: &Number, approx: f64, by_parts: impl FnOnce() -> u64) -> u64 {
    if approx.is_finite() && arith::num_eq(&Number::Real(approx), n) {
        real_hash(approx)
    } else {
        by_parts()
    }
}

/// A number's hash, consistent with `=`.
fn number_hash(n: &Number) -> u64 {
    match n {
        Number::Integer(i) => match i.to_i64() {
            Some(small) => int_hash(small),
            None => exact_hash(n, i.to_f64(), || {
                let (negative, limbs) = i.limbs();
                limbs_hash(negative, limbs)
            }),
        },
        Number::Rational(r) => exact_hash(n, r.to_f64(), || {
            let (numerator, denominator) = (r.numerator(), r.denominator());
            let ((nn, nl), (dn, dl)) = (numerator.limbs(), denominator.limbs());
            combine(limbs_hash(nn, nl), limbs_hash(dn, dl))
        }),
        Number::Real(x) => real_hash(*x),
        Number::Complex(c) => {
            let im = c.imag_part();
            let re = number_hash(&c.real_part());
            if arith::is_zero(&im) {
                // `1+0.0i` is `=` to `1`.
                re
            } else {
                combine(re, number_hash(&im) ^ 0xa5a5_a5a5)
            }
        }
        _ => 0,
    }
}

fn finish(h: u64) -> Result<Value, Error> {
    Ok(Value::fixnum((h & MASK) as i64).expect("60 bits fit a fixnum"))
}

impl<S: Strategy> Cx<S> {
    /// `eq?`'s hash of `v`: its word, or for a heap object its identity hash.
    fn eq_hash(&self, v: Value) -> u64 {
        if v.is_pointer() {
            mix(S::identity_hash(v))
        } else {
            mix(v.raw())
        }
    }

    /// `eqv?`'s hash of `v`: a number's value, else as `eq?`.
    fn eqv_hash(&self, v: Value) -> u64 {
        if let Some(n) = v.as_fixnum() {
            return int_hash(n);
        }
        match self.number(v) {
            Some(n) => number_hash(&n),
            None => self.eq_hash(v),
        }
    }

    /// The hash of `v` as `equal?` sees an object that is not a pair or a
    /// vector, which is by content for a string or bytevector.
    ///
    /// # Safety
    ///
    /// `v` must be a live value.
    unsafe fn leaf_hash(&self, v: Value) -> u64 {
        // SAFETY: passed on to the caller; each accessor is used on its type.
        unsafe {
            match v.tag() {
                Tag::String => bytes_hash(0x51, self.string_str(v).as_bytes()),
                Tag::Symbol => mix(get(v, Self::LAYOUT.symbol_hash())),
                _ => match self.header_type(v) {
                    Some(TypeCode::Bytevector) => bytes_hash(0xb7, self.bytevector_bytes(v)),
                    _ => self.eqv_hash(v),
                },
            }
        }
    }

    /// `equal?`'s hash of `v`, over at most [`NODE_BUDGET`] objects.
    fn equal_hash(&self, v: Value) -> u64 {
        // SAFETY: a live value.
        let compound = v.is_pair() || unsafe { self.header_type(v) } == Some(TypeCode::Vector);
        if !compound {
            // A string, a number, a symbol hash as `string-hash`,
            // `number-hash` and `symbol-hash` do, which `default-hash`
            // (SRFI 128) must match.
            // SAFETY: as above.
            return unsafe { self.leaf_hash(v) };
        }
        let mut h = 0x0123_4567_89ab_cdef_u64;
        let mut budget = NODE_BUDGET;
        let mut work = vec![v];
        while budget > 0 {
            let Some(v) = work.pop() else { break };
            budget -= 1;
            // SAFETY: values reachable from the argument are live.
            unsafe {
                if v.is_pair() {
                    h = combine(h, 0x11);
                    work.push(self.cdr(v));
                    work.push(self.car(v));
                } else if self.header_type(v) == Some(TypeCode::Vector) {
                    let n = self.vector_length(v);
                    h = combine(combine(h, 0x22), n as u64);
                    for i in (0..n.min(budget)).rev() {
                        work.push(self.vector_ref(v, i));
                    }
                } else {
                    h = combine(h, self.leaf_hash(v));
                }
            }
        }
        h
    }
}

fn eq_hash<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    finish(cx.eq_hash(args[0]))
}

fn eqv_hash<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    finish(cx.eqv_hash(args[0]))
}

fn equal_hash<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    finish(cx.equal_hash(args[0]))
}

fn number_hash_primitive<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    match cx.number(args[0]) {
        Some(n) => finish(number_hash(&n)),
        None => Err(wrong_type(cx, "number-hash", "a number", args[0])),
    }
}

fn string_hash<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if args[0].tag() != Tag::String {
        return Err(wrong_type(cx, "string-hash", "a string", args[0]));
    }
    // SAFETY: a live string.
    finish(bytes_hash(
        0x51,
        unsafe { cx.string_str(args[0]) }.as_bytes(),
    ))
}

fn string_ci_hash<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if args[0].tag() != Tag::String {
        return Err(wrong_type(cx, "string-ci-hash", "a string", args[0]));
    }
    // SAFETY: a live string.
    let text = crate::strings::fold_string(unsafe { cx.string_str(args[0]) });
    finish(bytes_hash(0x51, text.as_bytes()))
}

fn symbol_hash<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if args[0].tag() != Tag::Symbol {
        return Err(wrong_type(cx, "symbol-hash", "a symbol", args[0]));
    }
    // SAFETY: a live symbol.
    finish(mix(unsafe { get(args[0], Cx::<S>::LAYOUT.symbol_hash()) }))
}

/// The hash procedures, each taking one value.
pub(crate) fn table<S: Strategy>() -> Vec<(&'static str, Arity, NativeFn<S>)> {
    let one = Arity::exactly(1);
    vec![
        ("%eq-hash", one, eq_hash),
        ("%eqv-hash", one, eqv_hash),
        ("%equal-hash", one, equal_hash),
        ("%number-hash", one, number_hash_primitive),
        ("%string-hash", one, string_hash),
        ("%string-ci-hash", one, string_ci_hash),
        ("%symbol-hash", one, symbol_hash),
    ]
}
