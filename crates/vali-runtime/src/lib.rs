//! Vali's runtime: values, memory strategies, objects and tasks.
//!
//! - [`Value`]: a raw tagged word (ADR 0002).
//! - [`Allocate`] and [`Strategy`]: the memory-strategy contract (ADR 0003),
//!   split into capability traits that grow as strategies need them.
//! - [`NoCollection`]: allocate and never reclaim, the M1 baseline.
//! - [`Heap`] and [`Task`]: memory shared by an isolate's tasks, and one
//!   task's allocation buffers, with the raw object API.
//! - [`Handle`]: how Rust code holds Scheme values. Handles are `Send`,
//!   `Sync` and `'static`, and are used through their owning task.

mod handle;
mod no_collection;
mod page;
mod strategy;
mod task;
mod value;

pub use handle::{Error, Handle};
pub use no_collection::{Buffers, NoCollection};
pub use page::{Bump, HeapConfig, LARGE_OBJECT_BYTES, OutOfMemory, PAGE_BYTES, PageSource};
pub use strategy::{Allocate, References, Space, Strategy};
pub use task::{Heap, Task};
pub use value::Value;

/// The strategy used when none is named, until M1's measurements choose one.
pub type DefaultStrategy = NoCollection;
