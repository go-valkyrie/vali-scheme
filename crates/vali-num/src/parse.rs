//! Parsing R7RS numeric syntax (R7RS-small, section 7.1.1), as the reader and
//! `string->number` do.

use dashu_int::{IBig, UBig};
use dashu_ratio::RBig;

use crate::complex::Complex;
use crate::rational::{exact_from_f64, from_rbig};
use crate::{Number, Radix};

/// The largest power of ten an exact decimal may be scaled by, in either
/// direction. `#e1e100000` is accepted; `#e1e100001` is an implementation
/// restriction, so that untrusted text cannot ask for an integer with
/// billions of digits.
pub const MAX_EXACT_DECIMAL_EXPONENT: u64 = 100_000;

/// Why text is not a number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// What went wrong.
    pub kind: ParseErrorKind,
    /// The byte offset in the text where it went wrong.
    pub offset: usize,
}

/// The kinds of [`ParseError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseErrorKind {
    /// The text does not match R7RS's numeric syntax here.
    Syntax,
    /// A radix or exactness prefix appears twice.
    DuplicatePrefix,
    /// A decimal point or exponent in a radix other than 10.
    DecimalInRadix,
    /// `#e` applied to an infinity or NaN, which have no exact value.
    ExactInfinityOrNan,
    /// An exact division by zero, such as `1/0`.
    DivisionByZero,
    /// An exact decimal scaled by more than [`MAX_EXACT_DECIMAL_EXPONENT`].
    ExponentTooLarge,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.kind {
            ParseErrorKind::Syntax => "invalid numeric syntax",
            ParseErrorKind::DuplicatePrefix => "radix or exactness prefix given twice",
            ParseErrorKind::DecimalInRadix => "decimal point or exponent outside radix 10",
            ParseErrorKind::ExactInfinityOrNan => "an infinity or NaN cannot be exact",
            ParseErrorKind::DivisionByZero => "exact division by zero",
            ParseErrorKind::ExponentTooLarge => "exponent too large for an exact number",
        };
        write!(f, "{what} at byte {}", self.offset)
    }
}

impl std::error::Error for ParseError {}

/// Parses `text` as an R7RS number.
///
/// `radix` applies when the text has no `#b`, `#o`, `#d` or `#x` prefix, as
/// for `string->number`. The syntax covers:
///
/// - radix and exactness prefixes in either order (`#x#e10`, `#e#x10`);
/// - integers, fractions (`1/3`) and decimals with exponents (`1.5e-3`),
///   decimals only in radix 10;
/// - `+inf.0`, `-inf.0`, `+nan.0` and `-nan.0`;
/// - complex numbers in rectangular form (`1+2i`, `-i`, `+inf.0i`) and polar
///   form (`magnitude@angle`, angle in radians).
///
/// Letters are case-insensitive. Exactness follows R7RS: a decimal or an
/// infinity or NaN makes the whole number inexact unless `#e` is given, and
/// `#i` makes any number inexact.
pub fn parse(text: &str, radix: Radix) -> Result<Number, ParseError> {
    let mut parser = Parser {
        s: text.as_bytes(),
        pos: 0,
        radix: radix.value(),
        exactness: Exactness::Default,
    };
    parser.prefixes()?;
    parser.complex()
}

impl std::str::FromStr for crate::Number {
    type Err = ParseError;

    /// Parses `text` as an R7RS number in radix 10 unless it says otherwise.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        parse(text, Radix::Decimal)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Exactness {
    Default,
    Exact,
    Inexact,
}

/// A real part as parsed, before exactness is settled for the whole number.
enum Real {
    Exact(RBig),
    Inexact(f64),
}

impl Real {
    fn to_f64(&self) -> f64 {
        match self {
            Real::Exact(q) => q.to_f64().value(),
            Real::Inexact(x) => *x,
        }
    }

    fn is_exact_zero(&self) -> bool {
        matches!(self, Real::Exact(q) if *q == RBig::ZERO)
    }

    fn negate(self) -> Real {
        match self {
            Real::Exact(q) => Real::Exact(-q),
            Real::Inexact(x) => Real::Inexact(-x),
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
    radix: u32,
    exactness: Exactness,
}

impl<'a> Parser<'a> {
    fn error(&self, kind: ParseErrorKind) -> ParseError {
        ParseError {
            kind,
            offset: self.pos,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).map(u8::to_ascii_lowercase)
    }

    fn at_end(&self) -> bool {
        self.pos == self.s.len()
    }

    fn rest_eq(&self, lowercase: &str) -> bool {
        self.s[self.pos..].eq_ignore_ascii_case(lowercase.as_bytes())
    }

    fn rest_starts_with(&self, lowercase: &str) -> bool {
        let rest = &self.s[self.pos..];
        rest.len() >= lowercase.len()
            && rest[..lowercase.len()].eq_ignore_ascii_case(lowercase.as_bytes())
    }

    /// `<prefix R>`: a radix and an exactness, each at most once, in either
    /// order.
    fn prefixes(&mut self) -> Result<(), ParseError> {
        let mut radix_seen = false;
        let mut exactness_seen = false;
        while self.peek() == Some(b'#') {
            let marker = self.s.get(self.pos + 1).map(u8::to_ascii_lowercase);
            let (is_radix, seen) = match marker {
                Some(b'b' | b'o' | b'd' | b'x') => (true, radix_seen),
                Some(b'e' | b'i') => (false, exactness_seen),
                _ => {
                    self.pos += 1;
                    return Err(self.error(ParseErrorKind::Syntax));
                }
            };
            if seen {
                return Err(self.error(ParseErrorKind::DuplicatePrefix));
            }
            match marker {
                Some(b'b') => self.radix = 2,
                Some(b'o') => self.radix = 8,
                Some(b'd') => self.radix = 10,
                Some(b'x') => self.radix = 16,
                Some(b'e') => self.exactness = Exactness::Exact,
                _ => self.exactness = Exactness::Inexact,
            }
            if is_radix {
                radix_seen = true;
            } else {
                exactness_seen = true;
            }
            self.pos += 2;
        }
        Ok(())
    }

    /// `<complex R>`.
    fn complex(&mut self) -> Result<Number, ParseError> {
        // `+i` and `-i` on their own.
        if let Some(im) = self.unit_imaginary() {
            return self.rectangular(Real::Exact(RBig::ZERO), im);
        }
        let (first, signed) = self.real()?;
        if self.at_end() {
            return Ok(self.finish_real(first));
        }
        match self.peek() {
            // A pure imaginary such as `+2i`; R7RS requires the sign.
            Some(b'i') if self.pos + 1 == self.s.len() => {
                if !signed {
                    return Err(self.error(ParseErrorKind::Syntax));
                }
                self.pos += 1;
                self.rectangular(Real::Exact(RBig::ZERO), first)
            }
            Some(b'@') => {
                self.pos += 1;
                let (angle, _) = self.real()?;
                self.expect_end()?;
                self.polar(first, angle)
            }
            Some(b'+' | b'-') => {
                let im = match self.unit_imaginary() {
                    Some(im) => im,
                    None => {
                        let (im, _) = self.real()?;
                        if self.peek() != Some(b'i') {
                            return Err(self.error(ParseErrorKind::Syntax));
                        }
                        self.pos += 1;
                        self.expect_end()?;
                        im
                    }
                };
                self.rectangular(first, im)
            }
            _ => Err(self.error(ParseErrorKind::Syntax)),
        }
    }

    /// `+i` or `-i` as the rest of the text: an imaginary part of ±1.
    fn unit_imaginary(&mut self) -> Option<Real> {
        let im = if self.rest_eq("+i") {
            RBig::ONE
        } else if self.rest_eq("-i") {
            RBig::NEG_ONE
        } else {
            return None;
        };
        self.pos = self.s.len();
        Some(Real::Exact(im))
    }

    fn expect_end(&self) -> Result<(), ParseError> {
        if self.at_end() {
            Ok(())
        } else {
            Err(self.error(ParseErrorKind::Syntax))
        }
    }

    /// `<real R>`. Returns the value and whether it had an explicit sign.
    fn real(&mut self) -> Result<(Real, bool), ParseError> {
        for (text, value) in [
            ("+inf.0", f64::INFINITY),
            ("-inf.0", f64::NEG_INFINITY),
            ("+nan.0", f64::NAN),
            ("-nan.0", f64::NAN),
        ] {
            if self.rest_starts_with(text) {
                if self.exactness == Exactness::Exact {
                    return Err(self.error(ParseErrorKind::ExactInfinityOrNan));
                }
                self.pos += text.len();
                return Ok((Real::Inexact(value), true));
            }
        }
        let (negative, signed) = match self.peek() {
            Some(b'+') => (false, true),
            Some(b'-') => (true, true),
            _ => (false, false),
        };
        if signed {
            self.pos += 1;
        }
        let value = self.ureal()?;
        Ok((if negative { value.negate() } else { value }, signed))
    }

    /// `<ureal R>`: an unsigned integer, fraction or decimal.
    fn ureal(&mut self) -> Result<Real, ParseError> {
        let start = self.pos;
        let integer = self.digits(self.radix);
        match self.peek() {
            Some(b'/') => {
                if integer.is_empty() {
                    return Err(self.error(ParseErrorKind::Syntax));
                }
                self.pos += 1;
                let denominator_start = self.pos;
                let denominator = self.digits(self.radix);
                if denominator.is_empty() {
                    return Err(self.error(ParseErrorKind::Syntax));
                }
                let numerator = self.uinteger(integer);
                let denominator = self.uinteger(denominator);
                if denominator == IBig::ZERO {
                    self.pos = denominator_start;
                    return Err(self.error(ParseErrorKind::DivisionByZero));
                }
                Ok(Real::Exact(RBig::from_parts_signed(numerator, denominator)))
            }
            Some(b'.') if self.radix != 10 => Err(self.error(ParseErrorKind::DecimalInRadix)),
            Some(b'.') => self.decimal(start, integer),
            Some(b'e') if self.radix == 10 && !integer.is_empty() && self.exponent_follows() => {
                self.decimal(start, integer)
            }
            _ if integer.is_empty() => Err(self.error(ParseErrorKind::Syntax)),
            _ => Ok(Real::Exact(RBig::from(self.uinteger(integer)))),
        }
    }

    /// Whether the text at the cursor is `e` followed by an optional sign
    /// and a digit.
    fn exponent_follows(&self) -> bool {
        let mut i = self.pos + 1;
        if matches!(self.s.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        self.s.get(i).is_some_and(u8::is_ascii_digit)
    }

    /// `<decimal 10>`, with the cursor on the `.` or `e` after `integer`.
    fn decimal(&mut self, start: usize, integer: &'a str) -> Result<Real, ParseError> {
        let mut fraction = "";
        if self.peek() == Some(b'.') {
            self.pos += 1;
            fraction = self.digits(10);
        }
        if integer.is_empty() && fraction.is_empty() {
            self.pos = start;
            return Err(self.error(ParseErrorKind::Syntax));
        }
        let mut exponent = "";
        let mut exponent_negative = false;
        if self.peek() == Some(b'e') {
            if !self.exponent_follows() {
                return Err(self.error(ParseErrorKind::Syntax));
            }
            self.pos += 1;
            if let Some(sign @ (b'+' | b'-')) = self.peek() {
                exponent_negative = sign == b'-';
                self.pos += 1;
            }
            exponent = self.digits(10);
        }

        if self.exactness == Exactness::Exact {
            return self.exact_decimal(integer, fraction, exponent, exponent_negative);
        }
        // Rust's parser rounds correctly; give it a normalised spelling.
        let text = format!(
            "{}.{}e{}{}",
            if integer.is_empty() { "0" } else { integer },
            if fraction.is_empty() { "0" } else { fraction },
            if exponent_negative { "-" } else { "" },
            if exponent.is_empty() { "0" } else { exponent },
        );
        let value: f64 = text.parse().expect("a normalised decimal always parses");
        Ok(Real::Inexact(value))
    }

    /// The exact value of a decimal, as `#e` asks for: `#e1.1` is `11/10`.
    fn exact_decimal(
        &self,
        integer: &str,
        fraction: &str,
        exponent: &str,
        exponent_negative: bool,
    ) -> Result<Real, ParseError> {
        let too_large = || self.error(ParseErrorKind::ExponentTooLarge);
        let exponent: i64 = if exponent.is_empty() {
            0
        } else {
            exponent.parse().map_err(|_| too_large())?
        };
        let exponent = if exponent_negative {
            -exponent
        } else {
            exponent
        };
        // The value is digits × 10^scale.
        let scale = exponent
            .checked_sub(fraction.len() as i64)
            .ok_or_else(too_large)?;
        if scale.unsigned_abs() > MAX_EXACT_DECIMAL_EXPONENT {
            return Err(too_large());
        }
        let digits = self.uinteger(&format!("{integer}{fraction}"));
        let power = UBig::from(10u8).pow(scale.unsigned_abs() as usize);
        Ok(Real::Exact(if scale >= 0 {
            RBig::from(digits * IBig::from(power))
        } else {
            RBig::from_parts(digits, power)
        }))
    }

    /// Consumes digits valid in `radix` and returns them.
    fn digits(&mut self, radix: u32) -> &'a str {
        let text: &'a [u8] = self.s;
        let start = self.pos;
        while text
            .get(self.pos)
            .is_some_and(|&c| (c as char).is_digit(radix))
        {
            self.pos += 1;
        }
        std::str::from_utf8(&text[start..self.pos]).expect("ASCII digits are UTF-8")
    }

    /// The value of digits already checked to be valid in the radix.
    fn uinteger(&self, digits: &str) -> IBig {
        IBig::from_str_radix(digits, self.radix).expect("digits were checked for the radix")
    }

    /// Settles the exactness of a lone real.
    fn finish_real(&self, real: Real) -> Number {
        match (real, self.exactness) {
            (Real::Exact(q), Exactness::Inexact) => Number::Real(q.to_f64().value()),
            (Real::Exact(q), _) => from_rbig(q),
            (Real::Inexact(x), _) => Number::Real(x),
        }
    }

    /// `re + im·i`. Exact only if both parts are, and `#i` was not given.
    fn rectangular(&self, re: Real, im: Real) -> Result<Number, ParseError> {
        Ok(match (re, im, self.exactness) {
            (Real::Exact(re), Real::Exact(im), Exactness::Default | Exactness::Exact) => {
                Complex::exact(re, im)
            }
            (re, im, _) => Complex::inexact(re.to_f64(), im.to_f64()),
        })
    }

    /// `magnitude@angle`. An exact zero angle leaves the magnitude as it is;
    /// any other angle gives an inexact result, since its cosine and sine are
    /// irrational, unless `#e` asks for the exact value of that result.
    fn polar(&self, magnitude: Real, angle: Real) -> Result<Number, ParseError> {
        if angle.is_exact_zero() {
            return Ok(self.finish_real(magnitude));
        }
        let (m, a) = (magnitude.to_f64(), angle.to_f64());
        let (re, im) = (m * libm::cos(a), m * libm::sin(a));
        if self.exactness != Exactness::Exact {
            return Ok(Complex::inexact(re, im));
        }
        match (exact_from_f64(re), exact_from_f64(im)) {
            (Some(re), Some(im)) => Ok(Complex::exact(re, im)),
            _ => Err(self.error(ParseErrorKind::ExactInfinityOrNan)),
        }
    }
}
