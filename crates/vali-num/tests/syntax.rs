//! Reading and printing Scheme's numeric syntax.

use vali_num::{Integer, Number, ParseErrorKind, Radix, parse};

fn read(text: &str) -> Number {
    text.parse()
        .unwrap_or_else(|e| panic!("{text:?} should parse: {e}"))
}

/// Parses `text` and checks how it prints.
fn reads_as(text: &str, printed: &str) {
    assert_eq!(read(text).to_string(), printed, "reading {text:?}");
}

fn fails(text: &str, kind: ParseErrorKind) {
    match text.parse::<Number>() {
        Ok(n) => panic!("{text:?} should not parse, but read as {n}"),
        Err(e) => assert_eq!(e.kind, kind, "error for {text:?}"),
    }
}

#[test]
fn integers() {
    reads_as("0", "0");
    reads_as("-17", "-17");
    reads_as("+5", "5");
    reads_as(
        "-123456789012345678901234567890",
        "-123456789012345678901234567890",
    );
    assert!(read("42").is_exact());
    assert!(matches!(read("42"), Number::Integer(_)));
}

#[test]
fn radix_prefixes() {
    reads_as("#x1F", "31");
    reads_as("#XfF", "255");
    reads_as("#b101", "5");
    reads_as("#o17", "15");
    reads_as("#d10", "10");
    reads_as("#x-ff", "-255");
    // `e` is a digit in hexadecimal, not an exponent.
    reads_as("#x1e5", "485");
    // The radix argument applies only when there is no prefix.
    assert_eq!(parse("ff", Radix::Hexadecimal).unwrap().to_string(), "255");
    assert_eq!(parse("#d10", Radix::Hexadecimal).unwrap().to_string(), "10");
}

#[test]
fn prefixes_in_either_order_once_each() {
    reads_as("#x#e10", "16");
    reads_as("#e#x10", "16");
    reads_as("#i#x10", "16.0");
    fails("#x#x1", ParseErrorKind::DuplicatePrefix);
    fails("#e#i1", ParseErrorKind::DuplicatePrefix);
    fails("#q1", ParseErrorKind::Syntax);
}

#[test]
fn fractions_are_reduced() {
    reads_as("1/3", "1/3");
    reads_as("6/4", "3/2");
    reads_as("-1/2", "-1/2");
    reads_as("#x1/A", "1/10");
    // A fraction that divides evenly is an integer.
    reads_as("4/2", "2");
    assert!(matches!(read("4/2"), Number::Integer(_)));
    fails("1/0", ParseErrorKind::DivisionByZero);
}

#[test]
fn decimals_are_inexact_and_print_shortest() {
    reads_as("1.5", "1.5");
    reads_as("1.", "1.0");
    reads_as(".5", "0.5");
    reads_as("0.1", "0.1");
    reads_as("1e3", "1000.0");
    reads_as("1E3", "1000.0");
    reads_as("1.5e-3", "0.0015");
    reads_as("-0.0", "-0.0");
    reads_as("1e20", "100000000000000000000.0");
    reads_as("1e21", "1e21");
    reads_as("1e-6", "0.000001");
    reads_as("1e-7", "1e-7");
    reads_as("1.7976931348623157e308", "1.7976931348623157e308");
    reads_as("5e-324", "5e-324");
    assert!(!read("1.5").is_exact());
}

#[test]
fn exactness_prefixes() {
    // #e takes a decimal's exact value from its digits, not from a float.
    reads_as("#e1.5", "3/2");
    reads_as("#e1.1", "11/10");
    reads_as("#e0.1", "1/10");
    reads_as("#e1e3", "1000");
    reads_as("#e1e-3", "1/1000");
    reads_as("#e1.5e1", "15");
    // #i rounds exact values correctly.
    reads_as("#i1/3", "0.3333333333333333");
    reads_as("#i5", "5.0");
    fails("#e+inf.0", ParseErrorKind::ExactInfinityOrNan);
    fails("#e1e100001", ParseErrorKind::ExponentTooLarge);
}

#[test]
fn decimals_only_in_radix_ten() {
    fails("#x1.5", ParseErrorKind::DecimalInRadix);
    // In binary, `e` is not a digit, so `1e2` is not a number.
    fails("#b1e2", ParseErrorKind::Syntax);
}

#[test]
fn infinities_and_nan() {
    reads_as("+inf.0", "+inf.0");
    reads_as("-inf.0", "-inf.0");
    reads_as("+INF.0", "+inf.0");
    reads_as("+nan.0", "+nan.0");
    reads_as("-nan.0", "+nan.0");
    // Without a sign, these are symbols, not numbers.
    fails("inf.0", ParseErrorKind::Syntax);
}

#[test]
fn rectangular_complex() {
    reads_as("1+2i", "1+2i");
    reads_as("1-2i", "1-2i");
    reads_as("1/2+3/4i", "1/2+3/4i");
    reads_as("+i", "0+1i");
    reads_as("-i", "0-1i");
    reads_as("+2i", "0+2i");
    reads_as("1+i", "1+1i");
    reads_as("1+2I", "1+2i");
    // Exact and inexact parts are never mixed: the whole number is inexact.
    reads_as("1.5+2i", "1.5+2.0i");
    reads_as("#i1+2i", "1.0+2.0i");
    reads_as("+inf.0i", "0.0+inf.0i");
    reads_as("1-inf.0i", "1.0-inf.0i");
    reads_as("1+nan.0i", "1.0+nan.0i");
    // An exact zero imaginary part leaves a real; an inexact zero does not.
    reads_as("1+0i", "1");
    assert!(read("1+0i").is_real());
    reads_as("1.0+0i", "1.0+0.0i");
    assert!(!read("1.0+0i").is_real());
    reads_as("1-0.0i", "1.0-0.0i");
}

#[test]
fn polar_complex() {
    // An exact zero angle leaves the magnitude unchanged.
    reads_as("2@0", "2");
    reads_as("#e1@0", "1");
    reads_as("1.5@0", "1.5");
    // An inexact zero angle gives an inexact complex number.
    reads_as("1@0.0", "1.0+0.0i");
    // Any other angle uses the portable cos and sin.
    let half_pi = std::f64::consts::FRAC_PI_2;
    let n = read("1@1.5707963267948966");
    let Number::Complex(c) = &n else {
        panic!("expected a complex number, got {n}")
    };
    assert!(c.real_part().eqv(&Number::Real(libm::cos(half_pi))));
    assert!(c.imag_part().eqv(&Number::Real(libm::sin(half_pi))));
    // #e asks for the exact value of the inexact result.
    assert!(read("#e1@1").is_exact());
    fails("1@", ParseErrorKind::Syntax);
}

#[test]
fn not_numbers() {
    for text in [
        "", "-", "+", ".", "...", "1e", "1e+", "abc", "1/", "/2", "1.5/2", "1+2", "i", "1i", "2i",
        "--1", "1..2", "#", "#x", "1 ",
    ] {
        fails(text, ParseErrorKind::Syntax);
    }
}

#[test]
fn errors_point_at_the_problem() {
    let e = "12x".parse::<Number>().unwrap_err();
    assert_eq!(e.offset, 2);
    let e = "1/0".parse::<Number>().unwrap_err();
    assert_eq!((e.kind, e.offset), (ParseErrorKind::DivisionByZero, 2));
}

#[test]
fn printing_in_other_radixes() {
    assert_eq!(
        Number::from(255)
            .to_string_radix(Radix::Hexadecimal)
            .unwrap(),
        "ff"
    );
    assert_eq!(
        Number::from(-255)
            .to_string_radix(Radix::Hexadecimal)
            .unwrap(),
        "-ff"
    );
    assert_eq!(read("1/3").to_string_radix(Radix::Binary).unwrap(), "1/11");
    assert_eq!(read("1+2i").to_string_radix(Radix::Octal).unwrap(), "1+2i");
    // R7RS leaves inexact numbers in other radixes unspecified.
    assert_eq!(read("1.5").to_string_radix(Radix::Hexadecimal), None);
    assert_eq!(read("1.5+2i").to_string_radix(Radix::Binary), None);
}

#[test]
fn eqv_treats_every_nan_alike_and_signed_zeros_apart() {
    let nan = read("+nan.0");
    assert!(nan.eqv(&read("-nan.0")));
    // A NaN with another bit pattern, as some CPUs produce.
    let other_nan = Number::Real(f64::from_bits(0xfff8_0000_0000_0001));
    assert!(nan.eqv(&other_nan));
    assert!(!read("0.0").eqv(&read("-0.0")));
    assert!(read("0.0").eqv(&read("0.0")));
}

#[test]
fn eqv_respects_exactness() {
    assert!(!read("1").eqv(&read("1.0")));
    assert!(read("1/2").eqv(&read("2/4")));
    assert!(read("1+2i").eqv(&read("1+2i")));
    assert!(!read("1+2i").eqv(&read("1.0+2.0i")));
    assert!(read("+nan.0i").eqv(&read("0.0-nan.0i")));
}

#[test]
fn predicates() {
    assert!(read("2.0").is_integer());
    assert!(!read("2.5").is_integer());
    assert!(!read("+inf.0").is_integer());
    assert!(!read("+inf.0").is_rational());
    assert!(read("1/2").is_rational());
    assert!(read("1.5").is_rational());
    assert!(read("1.5").is_real());
    assert!(!read("1+2i").is_integer());
}

#[test]
fn rationals_from_parts() {
    let half = Number::from_ratio(Integer::from(2), Integer::from(4)).unwrap();
    assert_eq!(half.to_string(), "1/2");
    let Number::Rational(r) = half else {
        panic!("2/4 should be a rational")
    };
    assert_eq!(r.numerator(), Integer::from(1));
    assert_eq!(r.denominator(), Integer::from(2));
    assert!(matches!(
        Number::from_ratio(Integer::from(6), Integer::from(-3)),
        Some(Number::Integer(_))
    ));
    assert!(Number::from_ratio(Integer::from(1), Integer::ZERO).is_none());
}

#[test]
fn integer_conversions() {
    assert_eq!(Integer::from(i64::MIN).to_i64(), Some(i64::MIN));
    assert_eq!(Integer::from(u64::MAX).to_i64(), None);
    assert_eq!(Integer::from(u64::MAX).to_u64(), Some(u64::MAX));
    assert_eq!(Integer::from(i128::MAX).to_i128(), Some(i128::MAX));
    assert_eq!(Integer::from(3).to_f64(), 3.0);
}

/// Every flonum prints as text that reads back as exactly the same value.
#[test]
fn flonums_round_trip() {
    let mut samples = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.1,
        1.0 / 3.0,
        123456789.0,
        1e21,
        1e-7,
        9.999999999999999e20,
        f64::MAX,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::EPSILON,
        std::f64::consts::PI,
    ];
    // Plus a spread of bit patterns from a fixed pseudo-random sequence.
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..20_000 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let x = f64::from_bits(state);
        if x.is_finite() {
            samples.push(x);
        }
    }
    for x in samples {
        let printed = Number::Real(x).to_string();
        let back = read(&printed);
        assert!(
            back.eqv(&Number::Real(x)),
            "{x:e} printed as {printed} and read back as {back}"
        );
    }
}
