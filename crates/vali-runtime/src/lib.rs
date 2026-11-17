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

mod code;
mod counting;
mod cx;
mod cycles;
mod epoch;
mod handle;
mod no_collection;
mod numbers;
mod objects;
mod output;
mod page;
mod page_heap;
mod primitives;
mod printer;
mod read;
mod records;
mod stack;
mod strategy;
mod stubs;
mod syntax;
mod trampoline;
mod value;
mod vm;

pub use code::{Code, ModuleId, NativeFn};
pub use counting::{
    Counting, CountingHeap, DeferredHeadered, DeferredSideCounts, PreciseHeadered,
    PreciseSideCounts,
};
pub use cx::Cx;
pub use handle::{Error, Handle, SchemeError};
pub use no_collection::{BumpHeap, NoCollection};
pub use page::{Bump, HeapConfig, LARGE_OBJECT_BYTES, OutOfMemory, PAGE_BYTES, PageSource};
pub use primitives::primitive_names;
pub use strategy::{Allocate, References, Space, Strategy};
pub use value::Value;

/// The strategy used when none is named, until M1's measurements choose one.
pub type DefaultStrategy = NoCollection;
