//! Vali's runtime: values, memory strategies, objects and handles.
//!
//! - [`Value`]: a raw tagged word (ADR 0002).
//! - [`Allocate`] and [`Strategy`]: the memory-strategy contract (ADR 0003),
//!   split into capability traits that grow as strategies need them.
//! - [`NoCollection`]: allocate and never reclaim, the M1 baseline.
//! - [`Cx`]: an isolate's context, holding its heap and root table, with
//!   the raw object API. One thread at a time uses it (ADR 0008).
//! - [`Handle`]: how Rust code holds Scheme values. Handles are `Send`,
//!   `Sync` and `'static`, and are used through their isolate's context.

mod asan;
pub mod bridge;
mod code;
mod continuations;
mod counters;
mod counting;
mod cx;
mod cycles;
pub mod debug;
mod epoch;
mod handle;
mod isolate;
mod no_collection;
mod numbers;
mod objects;
mod page;
mod page_heap;
mod pool;
pub mod ports;
mod primitives;
mod printer;
mod read;
mod records;
mod rust_objects;
mod stack;
mod strategy;
mod strings;
mod stubs;
mod syntax;
mod task;
mod trampoline;
mod value;
mod vm;

pub use bridge::{NativeFunction, NativeLibrary, Obj};
pub use code::{Code, ModuleId, NativeFn};
pub use counters::Counters;
pub use counting::{
    Counting, CountingHeap, DeferredHeadered, DeferredSideCounts, PreciseHeadered,
    PreciseSideCounts,
};
pub use cx::Cx;
pub use handle::{Error, Frame, Handle, Position, SchemeError};
pub use isolate::Isolate;
pub use no_collection::{BumpHeap, NoCollection};
pub use page::{
    Bump, HeapConfig, LARGE_OBJECT_BYTES, OutOfMemory, PAGE_BYTES, PAGE_HEADER_BYTES, PageSource,
};
pub use primitives::{FEATURES, primitive_names};
pub use rust_objects::RustObject;
pub use strategy::{Allocate, References, Space, Strategy};
pub use vali_macros::{bridge, object};
pub use value::Value;

/// The strategy used when none is named, until M1's measurements choose one.
pub type DefaultStrategy = NoCollection;
