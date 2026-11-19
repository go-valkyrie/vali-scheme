//! Library loading and macro expansion from a hostile artifact. The input
//! is a mode and library number (one byte: the top bit picks the mode), a
//! section number and the bytes to put in that section of the library's
//! artifact. In the first mode the library is one of Vali's own, from
//! vali-stdlib, whose artifacts are trusted; in the second it is a small
//! user library, loaded as a host's would be, which meets the checks a
//! trusted artifact skips (internal primitives, native links). The container around the section is
//! rebuilt, so a mutation reaches the section's decoder, and the macro
//! expander when the program uses what a `syntax` section exports, instead
//! of stopping at the container's checks. The other libraries keep their
//! real artifacts, from vali-stdlib.
//!
//! Seeded with each section of each of vali-stdlib's artifacts and of the
//! user library's (see
//! fuzz/README.md). A custom mutator also overwrites bytes with numbers
//! that stress counts and indices, as the deterministic test
//! `replacing_any_number_in_an_artifact_never_panics` does.

#![no_main]

use std::collections::HashMap;
use std::sync::OnceLock;

use libfuzzer_sys::{fuzz_mutator, fuzz_target};
use vali_compiler::{
    Artifacts, Identity, LibraryName, LoadError, Loader, Source, compile_program_with,
};
use vali_dist::{Artifact, Writer};
use vali_fuzz::{SECTIONS, USER_PROGRAM, UserLoader, user_artifacts};
use vali_runtime::{Cx, HeapConfig, NoCollection};

/// Uses `(scheme base)`'s macros, a `syntax-rules` macro of its own and a
/// few other libraries.
const PROGRAM: &str = "
(import (scheme base) (scheme case-lambda) (scheme write) (scheme char))
(define-syntax swap!
  (syntax-rules () ((_ a b) (let ((t a)) (set! a b) (set! b t)))))
(define-record-type point (make-point x y) point? (x px set-px!) (y py))
(define (f . xs)
  (let*-values (((a b) (values 1 2)))
    (case (length xs)
      ((0) (do ((i 0 (+ i 1))) ((= i 3) i)))
      (else (guard (e (#t (swap! a b) a)) (raise xs))))))
(define g (case-lambda ((a) a) ((a b) (+ a b))))
(parameterize () (cond ((f) => g) (else (px (make-point 1 2)))))";

/// Serves nothing: every library is Vali's own, from an artifact.
struct NoSource;

impl Loader for NoSource {
    fn load(
        &self,
        _name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok(None)
    }
}

/// The artifacts of the user library and what it was compiled against.
fn user() -> &'static [(String, Vec<u8>)] {
    static USER: OnceLock<Vec<(String, Vec<u8>)>> = OnceLock::new();
    USER.get_or_init(user_artifacts)
}

fuzz_target!(|data: &[u8]| {
    let [first, section, patch @ ..] = data else {
        return;
    };
    // The top bit of the first byte picks the mode, the rest the library.
    let (user_mode, library) = (first & 0x80 != 0, usize::from(first & 0x7f));
    let (identity, original, mut read): (String, Vec<u8>, HashMap<String, Vec<u8>>);
    let program;
    if user_mode {
        // A host's library, which is not trusted as Vali's own are.
        let own: Vec<&(String, Vec<u8>)> = user()
            .iter()
            .filter(|(i, _)| i.starts_with("lib:"))
            .collect();
        let (i, b) = own[library % own.len()];
        (identity, original) = (i.clone(), b.clone());
        read = user().iter().cloned().collect();
        program = USER_PROGRAM.to_owned();
    } else {
        let artifacts = vali_stdlib::ARTIFACTS;
        let (i, b) = artifacts[library % artifacts.len()];
        (identity, original) = (i.to_owned(), b.to_vec());
        read = artifacts
            .iter()
            .map(|(i, b)| ((*i).to_owned(), b.to_vec()))
            .collect();
        // The replaced library is imported as well, so it is loaded whether
        // or not what the program uses depends on it. An identity is
        // `vali:scheme/base` for `(scheme base)`.
        let name = identity.strip_prefix("vali:").unwrap_or(&identity);
        let name = format!("({})", name.replace('/', " "));
        program = format!("(import (prefix {name} fz:))\n{PROGRAM}");
    }
    let Ok(mut artifact) = Artifact::decode(&original, SECTIONS) else {
        return;
    };
    artifact.insert(
        SECTIONS[usize::from(*section) % SECTIONS.len()],
        true,
        patch.to_vec(),
    );
    read.insert(identity, artifact.encode());

    let artifacts = Artifacts { read, write: false };
    let compiled = if user_mode {
        compile_program_with(&program, &Identity::new("fuzz"), &UserLoader, &artifacts)
    } else {
        compile_program_with(&program, &Identity::new("fuzz"), &NoSource, &artifacts)
    };
    if let Ok(program) = compiled {
        let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
        if cx.install_primitives().is_ok() {
            // Loading validates the bytecode. The program is not run: it is
            // the same whatever the artifact says.
            let _ = vali_compiler::load(&mut cx, program);
        }
    }
});

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    // Numbers that stress counts and indices.
    const NUMBERS: [u64; 8] = [0, 1, 2, 99, 255, 1 << 20, 1 << 34, u64::MAX];
    let size = libfuzzer_sys::fuzzer_mutate(data, size, max_size);
    // Past the two bytes that pick the section: half the time, overwrite
    // some bytes with a number as the format writes it.
    if size > 4 && seed % 2 == 0 {
        let at = 2 + (seed as usize / 2) % (size - 2);
        let mut w = Writer::new();
        w.uint(NUMBERS[(seed as usize / 7) % NUMBERS.len()]);
        let number = w.finish();
        let len = number.len().min(size - at);
        data[at..at + len].copy_from_slice(&number[..len]);
    }
    size
});
