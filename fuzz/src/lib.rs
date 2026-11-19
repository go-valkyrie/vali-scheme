//! What the `library` target and the seeds example share: the sections of
//! an artifact, and a small user library to mutate, which is loaded as a
//! host's library is (not as one of Vali's own, whose artifacts are
//! trusted and skip some of the checks).

use vali_compiler::{
    Artifacts, Identity, LibraryName, LoadError, Loader, Source, compile_program_with,
};

/// The sections a library's artifact has, in the order the second byte of
/// the `library` target's input picks them.
pub const SECTIONS: &[&str] = &["build", "source", "library", "syntax", "invoke", "depends"];

/// Serves the user library `(fuzz lib)` from its source, as a host would.
pub struct UserLoader;

const USER_LIBRARY: &str = "
(define-library (fuzz lib)
  (import (scheme base))
  (export greet twice count-args make-box box? unbox set-box!)
  (begin
    (define-record-type box (make-box v) box? (v unbox set-box!))
    (define (helper x) (string-append \"hi, \" x))
    (define (greet x) (helper x))
    (define-syntax twice
      (syntax-rules () ((_ e) (begin e e))))
    (define-syntax count-args
      (lambda (stx)
        (syntax-case stx ()
          ((_ a ...) (datum->syntax stx (length (syntax->datum #'(a ...))))))))))";

/// Uses what `(fuzz lib)` exports, macros included.
pub const USER_PROGRAM: &str = "
(import (scheme base) (fuzz lib))
(define n 0)
(twice (set! n (+ n 1)))
(define b (make-box 1))
(set-box! b (count-args 1 2 3))
(list (greet \"you\") n (unbox b) (box? b))";

impl Loader for UserLoader {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        Ok((name.to_string() == "(fuzz lib)")
            .then(|| Source::scheme(Identity::new("lib:(fuzz lib)"), USER_LIBRARY)))
    }
}

/// The artifacts of `(fuzz lib)` and whatever it was compiled against, as
/// the compiler writes them.
pub fn user_artifacts() -> Vec<(String, Vec<u8>)> {
    compile_program_with(
        USER_PROGRAM,
        &Identity::new("main"),
        &UserLoader,
        &Artifacts {
            write: true,
            ..Artifacts::default()
        },
    )
    .expect("the user library compiles")
    .artifacts
    .into_iter()
    .map(|(identity, bytes)| (identity.as_str().to_owned(), bytes))
    .collect()
}

/// The user library's own artifacts: those of the libraries a host serves,
/// not of Vali's, which every program is compiled against.
pub fn user_library_artifacts() -> Vec<(String, Vec<u8>)> {
    user_artifacts()
        .into_iter()
        .filter(|(identity, _)| identity.starts_with("lib:"))
        .collect()
}
