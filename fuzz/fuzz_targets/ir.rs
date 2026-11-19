//! Bytes decoded as an IR module, then taken down the path a loaded
//! library takes: validated by the decoder, optimised, converted to
//! run-format bytecode and validated again. Whatever the bytes say, this
//! must end in an error or a valid module: never a panic, and never memory
//! in proportion to anything but the input's size, which the fuzzer's
//! `-rss_limit_mb` and `-malloc_limit_mb` enforce.
//!
//! Seeded with the `invoke` section of each of vali-stdlib's artifacts (see
//! fuzz/README.md), which is an IR module.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(mut module) = vali_dist::decode(data) else {
        return;
    };
    // The decoder validates, so what it returns must encode, and decode to
    // the same module again.
    let encoded = vali_dist::encode(&module).expect("a decoded module is valid");
    let again = vali_dist::decode(&encoded).expect("an encoded module decodes");
    assert!(again == module, "encoding a decoded module changed it");

    if let Ok(plain) = vali_bytecode::convert(&module) {
        let _ = vali_bytecode::validate(&plain);
    }
    // The optimiser keeps a module well formed. Inlining can push a valid
    // module past a size limit, which is no optimiser bug: conversion then
    // refuses it, so only the structure is checked here.
    vali_opt::optimise(&mut module);
    vali_dist::validate_structure(&module).expect("the optimiser left a malformed module");
    if let Ok(optimised) = vali_bytecode::convert(&module) {
        let _ = vali_bytecode::validate(&optimised);
    }
});
