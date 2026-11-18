//! A read-eval-print loop (R7RS 5.7) a host can embed: an isolate whose
//! interaction environment, `(scheme repl)`'s, persists from one input to
//! the next. It starts with all of R7RS-small imported.
//!
//! Each input is read as data, and each datum is evaluated in the
//! interaction environment through `eval`, so definitions persist,
//! including those of names imported or defined before, and `(import set
//! ...)` adds to what the environment imports. The `vali` command's REPL is
//! this, over standard input.

use std::sync::Arc;

use vali_runtime::{Error, Handle, HeapConfig, Isolate, Strategy};
use vali_syntax::{LexErrorKind, ParseErrorKind, ReadErrorKind, SourceId};

use crate::loader::{Identity, Loader};

/// The program a REPL runs first: a procedure evaluating one datum in the
/// interaction environment, returning the values it returns as a list,
/// leaving out the unspecified value, which `define` returns.
const BOOT: &str = "
(import (scheme base) (scheme eval) (scheme repl))
(define (%repl-eval datum)
  (call-with-values
    (lambda () (eval datum (interaction-environment)))
    (lambda values
      (let loop ((values values) (shown '()))
        (cond ((null? values) (reverse shown))
              ((eq? (car values) (if #f #f)) (loop (cdr values) shown))
              (else (loop (cdr values) (cons (car values) shown))))))))";

/// What a REPL imports from the start, besides `(scheme base)`: the rest
/// of R7RS-small, as most Schemes' REPLs do. Reading the command line, the
/// clock and files still needs a host that grants them (ADR 0009 #6).
const STARTING_IMPORTS: &str = "(import (scheme case-lambda) (scheme char) (scheme complex)
                                       (scheme cxr) (scheme eval) (scheme file) (scheme inexact)
                                       (scheme lazy) (scheme process-context) (scheme read)
                                       (scheme repl) (scheme time) (scheme write))";

/// A REPL: an isolate, and the procedure that evaluates in its interaction
/// environment.
pub struct Repl<S: Strategy> {
    isolate: Isolate<S>,
    eval: Handle,
}

impl<S: Strategy> std::fmt::Debug for Repl<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repl").finish_non_exhaustive()
    }
}

impl<S: Strategy> Repl<S> {
    /// A REPL in a fresh isolate set up by `config`. `loader`, if any,
    /// serves the libraries `(import ...)` asks for beyond Vali's own, and
    /// what evaluated code includes.
    pub fn new(
        config: &HeapConfig,
        loader: Option<Arc<dyn Loader + Send + Sync>>,
    ) -> Result<Repl<S>, Error> {
        let identity = Identity::new("vali:repl");
        let fetcher = crate::eval::Fetcher(loader.as_deref());
        let program = crate::compile_program(BOOT, &identity, &fetcher)
            .map_err(|e| Error::scheme(format!("starting the REPL: {e}")))?;
        let mut isolate = Isolate::<S>::new(config);
        let eval = isolate.with(|cx| {
            cx.install_primitives()?;
            let id = match loader {
                Some(loader) => crate::load_with(cx, program, loader)?,
                None => crate::load(cx, program)?,
            };
            cx.run(id)?;
            Ok::<_, Error>(
                cx.variable(id, "%repl-eval")
                    .expect("the REPL's program defines %repl-eval"),
            )
        })?;
        let mut repl = Repl { isolate, eval };
        repl.eval(STARTING_IMPORTS)?;
        Ok(repl)
    }

    /// The REPL's isolate, for the host to set up further: to give it Rust
    /// procedures or native libraries, or to drain its console output.
    pub fn isolate(&mut self) -> &mut Isolate<S> {
        &mut self.isolate
    }

    /// Reads `text` and evaluates each datum in it, in order, in the
    /// interaction environment. Returns what each value printed as, as
    /// `write` prints it, leaving out unspecified values; or the first
    /// error, after which the rest of `text` is not evaluated.
    pub fn eval(&mut self, text: &str) -> Result<Vec<String>, Error> {
        let eval = &self.eval;
        self.isolate.with(|cx| {
            let mut shown = Vec::new();
            for datum in cx.read(text)? {
                let mut values = cx.call(eval, &[datum])?;
                while values.is_pair() {
                    let value = values.car(cx)?;
                    shown.push(cx.write_string(&value)?);
                    values = values.cdr(cx)?;
                }
            }
            Ok(shown)
        })
    }

    /// Whether `text` ends in the middle of a datum, so a line-by-line REPL
    /// should read more before evaluating it: a bracket left open, a string
    /// or comment left unterminated, or a prefix with nothing after it.
    pub fn is_incomplete(text: &str) -> bool {
        let read = vali_syntax::read(text, SourceId(0));
        let end = text.trim_end().len();
        read.errors.iter().any(|e| match e.kind {
            ReadErrorKind::Parse(ParseErrorKind::Unclosed)
            | ReadErrorKind::Lex(
                LexErrorKind::UnterminatedString
                | LexErrorKind::UnterminatedBlockComment
                | LexErrorKind::UnterminatedIdentifier,
            ) => true,
            ReadErrorKind::Parse(ParseErrorKind::MissingDatum) => usize::from(e.range.end()) >= end,
            _ => false,
        })
    }
}
