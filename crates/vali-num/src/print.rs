//! Printing numbers in Scheme's syntax, as `number->string` does.

use crate::{Number, Radix};

/// The number in `radix`, or `None` for an inexact number in a radix other
/// than 10, which R7RS leaves unspecified.
pub(crate) fn to_string_radix(n: &Number, radix: Radix) -> Option<String> {
    match n {
        Number::Integer(i) => Some(i.to_string_radix(radix)),
        Number::Rational(r) => Some(r.to_string_radix(radix)),
        Number::Real(x) => (radix == Radix::Decimal).then(|| format_flonum(*x)),
        Number::Complex(c) => c.to_string_radix(radix),
    }
}

impl Number {
    /// The number in `radix`, as Scheme's `number->string` writes it.
    /// Returns `None` for an inexact number in a radix other than 10, which
    /// R7RS leaves unspecified.
    pub fn to_string_radix(&self, radix: Radix) -> Option<String> {
        to_string_radix(self, radix)
    }
}

impl std::fmt::Display for Number {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Number::Integer(i) => std::fmt::Display::fmt(i, f),
            Number::Rational(r) => std::fmt::Display::fmt(r, f),
            Number::Real(x) => f.write_str(&format_flonum(*x)),
            Number::Complex(c) => std::fmt::Display::fmt(c, f),
        }
    }
}

/// A flonum in Scheme syntax: the shortest digits that read back as the
/// same value, always with a decimal point or an exponent so that it reads
/// back as inexact.
///
/// Values from 10⁻⁶ up to, but not including, 10²¹ are written positionally
/// (`0.000001`, `123.0`); others in scientific form (`1e21`, `1.5e-7`), the
/// same thresholds JavaScript uses.
pub(crate) fn format_flonum(x: f64) -> String {
    if x.is_nan() {
        return "+nan.0".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "+inf.0" } else { "-inf.0" }.to_owned();
    }
    // Rust's `{:e}` gives the shortest digits that round-trip, for example
    // `-1.2345e2`, `1e-7` or `-0e0`.
    let scientific = format!("{x:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("`{:e}` always writes an exponent");
    let exponent: i32 = exponent.parse().expect("`{:e}` writes a decimal exponent");
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();

    let mut out = String::from(sign);
    if (-6..21).contains(&exponent) {
        // `point` is where the decimal point goes, counted in digits.
        let point = exponent + 1;
        let len = digits.len() as i32;
        if point <= 0 {
            out.push_str("0.");
            out.extend(std::iter::repeat_n('0', (-point) as usize));
            out.push_str(&digits);
        } else if point >= len {
            out.push_str(&digits);
            out.extend(std::iter::repeat_n('0', (point - len) as usize));
            out.push_str(".0");
        } else {
            out.push_str(&digits[..point as usize]);
            out.push('.');
            out.push_str(&digits[point as usize..]);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push_str(&exponent.to_string());
    }
    out
}
