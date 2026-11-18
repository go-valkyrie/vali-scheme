//! Vali, an embeddable Scheme.
//!
//! Hosts depend on this crate alone; it re-exports what they need from
//! Vali's other crates (`docs/architecture.md`).
//!
//! An [`Isolate`] is a heap and the libraries loaded into it, used by one
//! thread at a time (ADR 0008). A host enters it with [`Isolate::with`],
//! which lends its context, a [`Cx`], and holds Scheme values across
//! entries as [`Handle`]s.
//!
//! ```
//! use vali::{HeapConfig, Isolate};
//!
//! let mut isolate: Isolate = Isolate::new(&HeapConfig::default());
//! let (plus, args) = isolate.with(|cx| {
//!     cx.install_primitives()?;
//!     let plus = cx.primitive("+").expect("a primitive");
//!     Ok::<_, vali::Error>((plus, cx.read("1 2 3")?))
//! })?;
//! let sum = isolate.call(&plus, &args)?;
//! assert_eq!(isolate.with(|cx| cx.write_string(&sum))?, "6");
//! # Ok::<(), vali::Error>(())
//! ```
//!
//! Rust types and functions are exposed to Scheme with [`object`] and
//! [`bridge`](macro@bridge) (ADR 0016), which work through this crate;
//! the [`bridge`](mod@bridge) module holds the conversions they use.
//!
//! Features: `eval` (on by default) brings the compiler, for compiling
//! programs, loaders, `eval` and the [`Repl`]; `prebuilt` (on by default)
//! embeds the standard libraries compiled ahead of time, which
//! [`stdlib::install`] makes the compiler use.

pub use vali_macros::{bridge, object};
pub use vali_num::{Complex, Integer, Number, Rational};
pub use vali_runtime::{
    Cx, DefaultStrategy, Error, Frame, Handle, HeapConfig, Isolate, OutOfMemory, Position,
    SchemeError, Value,
};

#[cfg(feature = "counters")]
pub use vali_runtime::Counters;

/// The numeric tower (ADR 0007).
pub use vali_num as num;

/// Exposing Rust to Scheme (ADR 0016): native libraries, the conversions
/// between Scheme values and Rust types, and Rust objects Scheme holds.
pub mod bridge {
    pub use vali_runtime::bridge::{
        BadArg, BorrowError, Bytes, Context, Eof, FromScheme, IntoFunction, IntoMethod, IntoScheme,
        NativeFunction, NativeLibrary, Obj, Param, ParamKind, Rest, Vector,
    };
    pub use vali_runtime::{NativeFn, RustObject};
}

/// Memory strategies (ADR 0003): the [`Strategy`](strategy::Strategy)
/// trait and the strategies Vali provides.
pub mod strategy {
    pub use vali_runtime::{
        Allocate, Bump, BumpHeap, Counting, CountingHeap, DeferredHeadered, DeferredSideCounts,
        NoCollection, PageSource, PreciseHeadered, PreciseSideCounts, References, Space, Strategy,
    };
}

/// Ports: the console and the port types bridges take and return.
pub mod ports {
    pub use vali_runtime::ports::*;
}

/// Where source text comes from, for errors and tools (ADR 0014).
pub mod diag {
    pub use vali_syntax::{Location, SourceId, Sources, Span};
}

/// Loading libraries (ADR 0009): the loaders a host gives the compiler,
/// and the names and identities of what they serve.
#[cfg(feature = "eval")]
pub mod load {
    pub use vali_compiler::{
        AsyncLoader, Identity, LibraryName, LoadError, Loader, NamePart, Source,
    };
}

/// Compiling programs and loading them into an isolate.
#[cfg(feature = "eval")]
pub mod compile {
    pub use vali_compiler::{
        Artifacts, CompileError, Program, compile_program, compile_program_with, load, load_with,
        standard_libraries,
    };
}

#[cfg(feature = "eval")]
pub use vali_compiler::Repl;

/// Vali's standard libraries, compiled when Vali was built.
#[cfg(feature = "prebuilt")]
pub mod stdlib {
    /// Makes the compiler load the standard libraries from their embedded
    /// artifacts instead of expanding them from source. Call it once,
    /// before compiling or starting a [`Repl`](crate::Repl).
    pub use vali_stdlib::install;
}

/// The runtime, for the code `#[bridge]` and `#[object]` generate.
#[doc(hidden)]
pub use vali_runtime as __runtime;
