//! Random bytecode, from the fuzzer's input, validated and run under every
//! memory strategy (vali_test::fuzz). Under AddressSanitizer, this finds
//! memory errors that do not crash, as well as panics.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vali_test::fuzz::{Bytes, check, module};

fuzz_target!(|data: &[u8]| {
    let module = module(&mut Bytes(data));
    if let Err(e) = check(&module) {
        panic!("{e}");
    }
});
