//! A bound on how deep `Debug` output goes, so that formatting a deeply
//! nested value with `{:?}` cannot overflow the stack (ADR 0014 #2).

use std::cell::Cell;
use std::fmt;

/// How many levels of nesting `Debug` prints. Deeper parts print as `...`.
const DEBUG_DEPTH: usize = 200;

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Restores the depth when the formatting of one level ends, even by a
/// panic.
struct Level(usize);

impl Drop for Level {
    fn drop(&mut self) {
        DEPTH.set(self.0);
    }
}

/// Runs `body`, one level deeper, or writes `...` instead past the limit.
pub(crate) fn bounded(
    f: &mut fmt::Formatter<'_>,
    body: impl FnOnce(&mut fmt::Formatter<'_>) -> fmt::Result,
) -> fmt::Result {
    let depth = DEPTH.get();
    if depth >= DEBUG_DEPTH {
        return f.write_str("...");
    }
    DEPTH.set(depth + 1);
    let _level = Level(depth);
    body(f)
}
