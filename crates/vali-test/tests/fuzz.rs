//! Random bytecode, validated and run under every strategy: valid
//! bytecode may fail, but never panic (ADR 0005 #7). The nightly fuzz
//! target in `fuzz/` runs the same generator under AddressSanitizer.

use vali_test::fuzz::{XorShift, check, module};

/// How many random modules to try. Each seed is its own case, so a
/// failure names the seed that reproduces it.
const CASES: u64 = 10_000;

#[test]
fn random_bytecode_never_panics() {
    let mut failures = Vec::new();
    let mut ran = 0;
    for seed in 1..=CASES {
        let module = module(&mut XorShift::new(seed));
        match check(&module) {
            Ok(true) => ran += 1,
            Ok(false) => {}
            Err(e) => failures.push(format!("seed {seed}: {e}")),
        }
    }
    eprintln!("{ran} of {CASES} random modules were valid and ran");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Most random modules are invalid; enough must be valid to run, or
    // the test only exercises the validator.
    assert!(
        ran * 10 >= CASES,
        "only {ran} of {CASES} random modules were valid"
    );
}
