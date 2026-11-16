//! Vali's test harness.
//!
//! Runs Scheme test files, the regression list (one test per requirement,
//! failing until the code it covers exists), vendored compliance suites,
//! determinism checks (ADR 0001) and runs under each memory strategy
//! (ADR 0003).
