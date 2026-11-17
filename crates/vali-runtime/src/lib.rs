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

mod cx;
mod handle;
mod no_collection;
mod page;
mod strategy;
mod value;

pub use cx::Cx;
pub use handle::{Error, Handle};
pub use no_collection::{BumpHeap, NoCollection};
pub use page::{Bump, HeapConfig, LARGE_OBJECT_BYTES, OutOfMemory, PAGE_BYTES, PageSource};
pub use strategy::{Allocate, References, Space, Strategy};
pub use value::Value;

/// The strategy used when none is named, until M1's measurements choose one.
pub type DefaultStrategy = NoCollection;
