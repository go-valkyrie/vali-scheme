//! The flonum primitives of SRFI 144 (ADR 0007).
//!
//! Every argument must be a flonum, `1.0` and not `1`: anything else is a
//! type error, where the SRFI only calls it an error. Results follow C99
//! for `-0.0`, infinities and NaNs. The functions Rust leaves to the
//! platform's C library come from `vali_num::flonum`, which uses `libm`,
//! so they agree on every platform.

use vali_abi::Tag;
use vali_bytecode::Arity;
use vali_num::flonum;

use crate::code::NativeFn;
use crate::cx::Cx;
use crate::handle::Error;
use crate::records;
use crate::strategy::Strategy;
use crate::value::Value;
use crate::vm::wrong_type;

fn fl<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<f64, Error> {
    if v.tag() == Tag::Flonum {
        // SAFETY: a valid value with the flonum tag is a live flonum.
        Ok(unsafe { cx.flonum_value(v) })
    } else {
        Err(wrong_type(cx, name, "a flonum", v))
    }
}

fn out<S: Strategy>(cx: &mut Cx<S>, x: f64) -> Result<Value, Error> {
    Ok(cx.flonum_raw(x)?)
}

fn exact<S: Strategy>(cx: &mut Cx<S>, n: i64) -> Result<Value, Error> {
    Ok(cx.integer_raw(&vali_num::Integer::from(n))?)
}

fn flonum_p<S: Strategy>(_: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0].tag() == Tag::Flonum))
}

macro_rules! unary {
    ($($f:ident $name:literal $op:expr),* $(,)?) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            let op: fn(f64) -> f64 = $op;
            let x = fl(cx, $name, args[0])?;
            out(cx, op(x))
        }
    )*};
}

unary!(
    fl_abs "flabs" f64::abs,
    fl_sgn "flsgn" |x| 1.0f64.copysign(x),
    fl_numerator "flnumerator" flonum::numerator,
    fl_denominator "fldenominator" flonum::denominator,
    fl_floor "flfloor" f64::floor,
    fl_ceiling "flceiling" f64::ceil,
    fl_round "flround" flonum::round,
    fl_truncate "fltruncate" f64::trunc,
    fl_exp "flexp" f64::exp,
    fl_exp2 "flexp2" flonum::exp2,
    fl_exp_1 "flexp-1" flonum::expm1,
    fl_square "flsquare" |x| x * x,
    fl_sqrt "flsqrt" f64::sqrt,
    fl_cbrt "flcbrt" flonum::cbrt,
    fl_log "fllog" f64::ln,
    fl_log1p "fllog1+" flonum::log1p,
    fl_log2 "fllog2" flonum::log2,
    fl_log10 "fllog10" flonum::log10,
    fl_sin "flsin" f64::sin,
    fl_cos "flcos" f64::cos,
    fl_tan "fltan" f64::tan,
    fl_asin "flasin" f64::asin,
    fl_acos "flacos" f64::acos,
    fl_sinh "flsinh" flonum::sinh,
    fl_cosh "flcosh" flonum::cosh,
    fl_tanh "fltanh" flonum::tanh,
    fl_asinh "flasinh" flonum::asinh,
    fl_acosh "flacosh" flonum::acosh,
    fl_atanh "flatanh" flonum::atanh,
    fl_gamma "flgamma" flonum::gamma,
    fl_erf "flerf" flonum::erf,
    fl_erfc "flerfc" flonum::erfc,
    fl_exponent "flexponent" flonum::exponent,
);

macro_rules! binary {
    ($($f:ident $name:literal $op:expr),* $(,)?) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            let op: fn(f64, f64) -> f64 = $op;
            let (x, y) = (fl(cx, $name, args[0])?, fl(cx, $name, args[1])?);
            out(cx, op(x, y))
        }
    )*};
}

binary!(
    fl_absdiff "flabsdiff" |x, y| (x - y).abs(),
    fl_posdiff "flposdiff" flonum::positive_difference,
    fl_hypot "flhypot" flonum::hypot,
    fl_expt "flexpt" f64::powf,
    fl_quotient "flquotient" flonum::quotient,
    fl_remainder "flremainder" flonum::remainder,
    fl_adjacent "fladjacent" flonum::adjacent,
    fl_copysign "flcopysign" f64::copysign,
);

macro_rules! predicates {
    ($($f:ident $name:literal $test:expr),* $(,)?) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            let test: fn(f64) -> bool = $test;
            Ok(Value::boolean(test(fl(cx, $name, args[0])?)))
        }
    )*};
}

predicates!(
    fl_zero "flzero?" |x| x == 0.0,
    fl_positive "flpositive?" |x| x > 0.0,
    fl_negative "flnegative?" |x| x < 0.0,
    fl_integer "flinteger?" |x| x.is_finite() && x.fract() == 0.0,
    fl_finite "flfinite?" f64::is_finite,
    fl_infinite "flinfinite?" f64::is_infinite,
    fl_nan "flnan?" f64::is_nan,
    fl_normalized "flnormalized?" flonum::is_normalized,
    fl_denormalized "fldenormalized?" flonum::is_denormalized,
);

/// `flodd?` and `fleven?`: the argument must be an integral flonum.
fn parity<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<bool, Error> {
    let x = fl(cx, name, v)?;
    if x.is_finite() && x.fract() == 0.0 {
        Ok(x % 2.0 != 0.0)
    } else {
        Err(wrong_type(cx, name, "an integral flonum", v))
    }
}
fn fl_odd<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(parity(cx, "flodd?", args[0])?))
}
fn fl_even<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(!parity(cx, "fleven?", args[0])?))
}

fn fl_unordered<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let (x, y) = (
        fl(cx, "flunordered?", args[0])?,
        fl(cx, "flunordered?", args[1])?,
    );
    Ok(Value::boolean(x.is_nan() || y.is_nan()))
}

/// A chain of comparisons over flonums, every one of them checked. A NaN
/// makes every comparison false.
fn chain<S: Strategy>(
    cx: &Cx<S>,
    name: &str,
    args: &[Value],
    holds: fn(f64, f64) -> bool,
) -> Result<Value, Error> {
    let mut all = Vec::with_capacity(args.len());
    for &a in args {
        all.push(fl(cx, name, a)?);
    }
    Ok(Value::boolean(all.windows(2).all(|w| holds(w[0], w[1]))))
}

macro_rules! comparisons {
    ($($f:ident $name:literal $op:tt),* $(,)?) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            chain(cx, $name, args, |a, b| a $op b)
        }
    )*};
}
comparisons!(
    fl_eq "fl=?" ==, fl_lt "fl<?" <, fl_gt "fl>?" >, fl_le "fl<=?" <=, fl_ge "fl>=?" >=,
);

/// A fold of flonums from `start`.
fn fold<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    start: f64,
    args: &[Value],
    op: fn(f64, f64) -> f64,
) -> Result<Value, Error> {
    let mut acc = start;
    for &a in args {
        acc = op(acc, fl(cx, name, a)?);
    }
    out(cx, acc)
}

/// Like [`fold`], but starting from the first argument, and a single
/// argument is `unary` of it.
fn reduce<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    args: &[Value],
    unary: fn(f64) -> f64,
    op: fn(f64, f64) -> f64,
) -> Result<Value, Error> {
    let first = fl(cx, name, args[0])?;
    if args.len() == 1 {
        return out(cx, unary(first));
    }
    let mut acc = first;
    for &a in &args[1..] {
        acc = op(acc, fl(cx, name, a)?);
    }
    out(cx, acc)
}

fn fl_add<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    fold(cx, "fl+", 0.0, args, |a, b| a + b)
}
fn fl_mul<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    fold(cx, "fl*", 1.0, args, |a, b| a * b)
}
fn fl_sub<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    reduce(cx, "fl-", args, |x| -x, |a, b| a - b)
}
fn fl_div<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    reduce(cx, "fl/", args, |x| 1.0 / x, |a, b| a / b)
}
/// `(flmax x ...)`: -infinity for no arguments. A NaN loses to a number.
fn fl_max<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    fold(cx, "flmax", f64::NEG_INFINITY, args, f64::max)
}
fn fl_min<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    fold(cx, "flmin", f64::INFINITY, args, f64::min)
}

fn fl_fma<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "fl+*";
    let (x, y, z) = (
        fl(cx, n, args[0])?,
        fl(cx, n, args[1])?,
        fl(cx, n, args[2])?,
    );
    out(cx, flonum::fma(x, y, z))
}

/// `(flatan x)` or `(flatan y x)`.
fn fl_atan<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let y = fl(cx, "flatan", args[0])?;
    match args.get(1).filter(|&&v| v != Value::DEFAULT) {
        None => out(cx, y.atan()),
        Some(&v) => {
            let x = fl(cx, "flatan", v)?;
            out(cx, y.atan2(x))
        }
    }
}

fn fl_integer_fraction<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let x = fl(cx, "flinteger-fraction", args[0])?;
    let (i, f) = flonum::integer_fraction(x);
    let (i, f) = (cx.flonum_raw(i)?, cx.flonum_raw(f)?);
    records::values(cx, &[i, f])
}

fn fl_integer_exponent<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let x = fl(cx, "flinteger-exponent", args[0])?;
    exact(cx, flonum::integer_exponent(x).into())
}

fn fl_normalized_fraction_exponent<S: Strategy>(
    cx: &mut Cx<S>,
    args: &[Value],
) -> Result<Value, Error> {
    let x = fl(cx, "flnormalized-fraction-exponent", args[0])?;
    let (f, e) = flonum::normalized_fraction_exponent(x);
    let (f, e) = (
        cx.flonum_raw(f)?,
        cx.integer_raw(&vali_num::Integer::from(e))?,
    );
    records::values(cx, &[f, e])
}

fn fl_sign_bit<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let x = fl(cx, "flsign-bit", args[0])?;
    exact(cx, flonum::sign_bit(x))
}

fn fl_remquo<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "flremquo";
    let (x, y) = (fl(cx, n, args[0])?, fl(cx, n, args[1])?);
    let (r, q) = flonum::remainder_quotient(x, y);
    let (r, q) = (
        cx.flonum_raw(r)?,
        cx.integer_raw(&vali_num::Integer::from(q))?,
    );
    records::values(cx, &[r, q])
}

fn fl_log_gamma<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let x = fl(cx, "flloggamma", args[0])?;
    let (v, sign) = flonum::log_gamma(x);
    let (v, sign) = (cx.flonum_raw(v)?, cx.flonum_raw(sign)?);
    records::values(cx, &[v, sign])
}

fn fl_first_bessel<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "flfirst-bessel";
    let (order, x) = (cx.integer_arg(n, args[0])?, fl(cx, n, args[1])?);
    let r = flonum::bessel_first(&order, x)
        .map_err(|_| Error::scheme(format!("{n}: order too large for that argument")))?;
    out(cx, r)
}
fn fl_second_bessel<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "flsecond-bessel";
    let (order, x) = (cx.integer_arg(n, args[0])?, fl(cx, n, args[1])?);
    let r = flonum::bessel_second(&order, x)
        .map_err(|_| Error::scheme(format!("{n}: order too large for that argument")))?;
    out(cx, r)
}

/// `(make-flonum x n)`: `x` times 2 to the exact integer `n`.
fn make_flonum<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let n = "make-flonum";
    let (x, e) = (fl(cx, n, args[0])?, cx.integer_arg(n, args[1])?);
    out(cx, flonum::make_flonum(x, &e))
}

const fn at_least(n: u8) -> Arity {
    Arity {
        required: n,
        optional: 0,
        rest: true,
    }
}

/// The flonum procedures implemented in Rust.
pub(crate) fn table<S: Strategy>() -> Vec<(&'static str, Arity, NativeFn<S>)> {
    let one = Arity::exactly(1);
    let two = Arity::exactly(2);
    vec![
        ("flonum?", one, flonum_p),
        ("fl=?", at_least(1), fl_eq),
        ("fl<?", at_least(1), fl_lt),
        ("fl>?", at_least(1), fl_gt),
        ("fl<=?", at_least(1), fl_le),
        ("fl>=?", at_least(1), fl_ge),
        ("flunordered?", two, fl_unordered),
        ("flinteger?", one, fl_integer),
        ("flzero?", one, fl_zero),
        ("flpositive?", one, fl_positive),
        ("flnegative?", one, fl_negative),
        ("flodd?", one, fl_odd),
        ("fleven?", one, fl_even),
        ("flfinite?", one, fl_finite),
        ("flinfinite?", one, fl_infinite),
        ("flnan?", one, fl_nan),
        ("flnormalized?", one, fl_normalized),
        ("fldenormalized?", one, fl_denormalized),
        ("flmax", at_least(0), fl_max),
        ("flmin", at_least(0), fl_min),
        ("fl+", at_least(0), fl_add),
        ("fl*", at_least(0), fl_mul),
        ("fl+*", Arity::exactly(3), fl_fma),
        ("fl-", at_least(1), fl_sub),
        ("fl/", at_least(1), fl_div),
        ("flabs", one, fl_abs),
        ("flabsdiff", two, fl_absdiff),
        ("flposdiff", two, fl_posdiff),
        ("flsgn", one, fl_sgn),
        ("flnumerator", one, fl_numerator),
        ("fldenominator", one, fl_denominator),
        ("flfloor", one, fl_floor),
        ("flceiling", one, fl_ceiling),
        ("flround", one, fl_round),
        ("fltruncate", one, fl_truncate),
        ("flexp", one, fl_exp),
        ("flexp2", one, fl_exp2),
        ("flexp-1", one, fl_exp_1),
        ("flsquare", one, fl_square),
        ("flsqrt", one, fl_sqrt),
        ("flcbrt", one, fl_cbrt),
        ("flhypot", two, fl_hypot),
        ("flexpt", two, fl_expt),
        ("fllog", one, fl_log),
        ("fllog1+", one, fl_log1p),
        ("fllog2", one, fl_log2),
        ("fllog10", one, fl_log10),
        ("flsin", one, fl_sin),
        ("flcos", one, fl_cos),
        ("fltan", one, fl_tan),
        ("flasin", one, fl_asin),
        ("flacos", one, fl_acos),
        (
            "flatan",
            Arity {
                required: 1,
                optional: 1,
                rest: false,
            },
            fl_atan,
        ),
        ("flsinh", one, fl_sinh),
        ("flcosh", one, fl_cosh),
        ("fltanh", one, fl_tanh),
        ("flasinh", one, fl_asinh),
        ("flacosh", one, fl_acosh),
        ("flatanh", one, fl_atanh),
        ("flquotient", two, fl_quotient),
        ("flremainder", two, fl_remainder),
        ("flremquo", two, fl_remquo),
        ("flgamma", one, fl_gamma),
        ("flloggamma", one, fl_log_gamma),
        ("flfirst-bessel", two, fl_first_bessel),
        ("flsecond-bessel", two, fl_second_bessel),
        ("flerf", one, fl_erf),
        ("flerfc", one, fl_erfc),
        ("fladjacent", two, fl_adjacent),
        ("flcopysign", two, fl_copysign),
        ("make-flonum", two, make_flonum),
        ("flinteger-fraction", one, fl_integer_fraction),
        ("flexponent", one, fl_exponent),
        ("flinteger-exponent", one, fl_integer_exponent),
        (
            "flnormalized-fraction-exponent",
            one,
            fl_normalized_fraction_exponent,
        ),
        ("flsign-bit", one, fl_sign_bit),
    ]
}
