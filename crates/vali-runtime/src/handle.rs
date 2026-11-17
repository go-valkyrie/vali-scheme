//! Handles: how Rust code holds Scheme values (ADR 0003).
//!
//! A [`Handle`] is an ordinary Rust value, `Send`, `Sync`, `Clone` and
//! `'static`. A handle to a heap object points to a root slot in its
//! isolate's [`RootTable`]; the slot has its own atomic count, which only Rust
//! clones and drops change. The object itself has one ordinary reference,
//! from the slot, which only the isolate adds and removes. Using a handle
//! takes the isolate's context, which proves the isolate is not running
//! anywhere else; dropping one takes nothing, and queues the slot for the
//! isolate to release at its next safepoint. Immediates need no slot.

use std::cell::UnsafeCell;
use std::fmt;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

use vali_abi::Tag;

use crate::page::OutOfMemory;
use crate::value::Value;

/// What can go wrong using a handle or running Scheme code.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Allocation failed.
    OutOfMemory,
    /// The handle belongs to another isolate. Values move between isolates
    /// by being sent through a channel, which copies them (ADR 0008).
    WrongIsolate,
    /// The value is not of the type the operation needs.
    WrongType {
        /// What the operation needs.
        expected: &'static str,
    },
    /// An index is out of range.
    OutOfRange,
    /// Scheme code raised an error. Until Scheme has conditions, errors
    /// unwind to the host with a backtrace.
    Scheme(Box<SchemeError>),
}

/// An error raised while running Scheme code.
#[derive(Clone, Debug)]
pub struct SchemeError {
    /// What went wrong.
    pub message: String,
    /// The procedures that were running, innermost first.
    pub backtrace: Vec<String>,
    /// What Scheme code raised, if it raised something: an error object
    /// for `error`, or any value for `raise`. Errors from Rust procedures
    /// raise nothing; a handler sees them as error objects with their
    /// message.
    pub raised: Option<Handle>,
    /// Whether `%raise` raised it: an escape straight to the innermost
    /// `%catch`. Any other error from Scheme code first calls the raise
    /// procedure (scheme base) registers, where it happened, while an
    /// exception handler is installed.
    pub(crate) escaping: bool,
}

/// Errors are equal if their messages and backtraces are; what they raised
/// is not compared.
impl PartialEq for SchemeError {
    fn eq(&self, other: &SchemeError) -> bool {
        self.message == other.message && self.backtrace == other.backtrace
    }
}

impl Eq for SchemeError {}

impl Error {
    /// A Scheme error with `message` and no backtrace yet.
    pub fn scheme(message: impl Into<String>) -> Error {
        Error::Scheme(Box::new(SchemeError {
            message: message.into(),
            backtrace: Vec::new(),
            raised: None,
            escaping: false,
        }))
    }

    /// An error that raises `raised`, described by `message`.
    pub fn raise(message: impl Into<String>, raised: Handle) -> Error {
        Error::Scheme(Box::new(SchemeError {
            message: message.into(),
            backtrace: Vec::new(),
            raised: Some(raised),
            escaping: false,
        }))
    }

    /// An escape that raises `raised` straight to the innermost `%catch`,
    /// described by `message`.
    pub(crate) fn escape(message: impl Into<String>, raised: Handle) -> Error {
        Error::Scheme(Box::new(SchemeError {
            message: message.into(),
            backtrace: Vec::new(),
            raised: Some(raised),
            escaping: true,
        }))
    }

    /// Whether Scheme exception handlers see this error where it happened,
    /// through the raise procedure: whether it is neither an escape nor
    /// the heap running out.
    pub(crate) fn reaches_handlers(&self) -> bool {
        match self {
            Error::Scheme(e) => !e.escaping,
            Error::OutOfMemory => false,
            _ => true,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::OutOfMemory => f.write_str("out of memory"),
            Error::WrongIsolate => f.write_str(
                "this handle belongs to another isolate; send the value through a channel to copy it",
            ),
            Error::WrongType { expected } => write!(f, "expected {expected}"),
            Error::OutOfRange => f.write_str("index out of range"),
            Error::Scheme(e) => {
                f.write_str(&e.message)?;
                for name in &e.backtrace {
                    write!(f, "\n  in {name}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<OutOfMemory> for Error {
    fn from(_: OutOfMemory) -> Error {
        Error::OutOfMemory
    }
}

const SLOTS_PER_CHUNK: usize = 256;
const NONE: u32 = u32::MAX;

/// One root: a value held for Rust, with Rust's count of handles to it.
#[derive(Debug)]
pub(crate) struct Slot {
    /// The value. Only the owning isolate reads or writes it.
    value: AtomicU64,
    /// How many handles point here. Any thread changes it.
    count: AtomicU32,
    /// The next slot in the release stack, while this one is on it.
    next: AtomicU32,
    /// This slot's index in its table.
    index: u32,
}

#[derive(Debug, Default)]
struct Owned {
    chunks: Vec<Box<[Slot]>>,
    free: Vec<u32>,
}

/// An isolate's root slots. Slots live in chunks that never move, so handles
/// point straight at them.
pub(crate) struct RootTable {
    /// The top of the stack of slots whose count reached zero: an index, or
    /// `NONE`. Handles push onto it from any thread; the owner takes the
    /// whole stack at once, so the stack has no ABA problem.
    released: AtomicU32,
    /// Only the owning isolate touches this, through `&mut Cx`.
    owned: UnsafeCell<Owned>,
}

// SAFETY: `released` and each slot's `count` and `next` are atomics, used
// from any thread. `owned` and each slot's `value` are only used by the
// owning isolate, which `&mut Cx` makes exclusive.
unsafe impl Send for RootTable {}
// SAFETY: as above.
unsafe impl Sync for RootTable {}

impl fmt::Debug for RootTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RootTable").finish_non_exhaustive()
    }
}

impl RootTable {
    pub(crate) fn new() -> Arc<RootTable> {
        Arc::new(RootTable {
            released: AtomicU32::new(NONE),
            owned: UnsafeCell::new(Owned::default()),
        })
    }

    /// The owner's part of the table.
    ///
    /// # Safety
    ///
    /// Only the owning isolate may call this, with exclusive access to itself.
    #[allow(clippy::mut_from_ref)]
    unsafe fn owned(&self) -> &mut Owned {
        // SAFETY: passed on to the caller.
        unsafe { &mut *self.owned.get() }
    }

    /// A slot holding `v` with a count of one.
    ///
    /// # Safety
    ///
    /// As for [`RootTable::owned`].
    pub(crate) unsafe fn insert(&self, v: Value) -> NonNull<Slot> {
        // SAFETY: passed on to the caller.
        let owned = unsafe { self.owned() };
        let index = owned.free.pop().unwrap_or_else(|| {
            let start = owned.chunks.len() * SLOTS_PER_CHUNK;
            let chunk = (start..start + SLOTS_PER_CHUNK)
                .map(|i| Slot {
                    value: AtomicU64::new(Value::FALSE.raw()),
                    count: AtomicU32::new(0),
                    next: AtomicU32::new(NONE),
                    index: i as u32,
                })
                .collect();
            owned.chunks.push(chunk);
            owned
                .free
                .extend((start as u32 + 1..(start + SLOTS_PER_CHUNK) as u32).rev());
            start as u32
        });
        let slot =
            &owned.chunks[index as usize / SLOTS_PER_CHUNK][index as usize % SLOTS_PER_CHUNK];
        slot.value.store(v.raw(), Ordering::Relaxed);
        slot.count.store(1, Ordering::Relaxed);
        NonNull::from(slot)
    }

    /// Takes every released slot, frees it, and passes each value it held
    /// to `release`.
    ///
    /// # Safety
    ///
    /// As for [`RootTable::owned`].
    pub(crate) unsafe fn reclaim(&self, mut release: impl FnMut(Value)) {
        if self.released.load(Ordering::Relaxed) == NONE {
            return;
        }
        let mut index = self.released.swap(NONE, Ordering::Acquire);
        // SAFETY: passed on to the caller.
        let owned = unsafe { self.owned() };
        while index != NONE {
            let slot =
                &owned.chunks[index as usize / SLOTS_PER_CHUNK][index as usize % SLOTS_PER_CHUNK];
            let value = Value::from_raw(slot.value.swap(Value::FALSE.raw(), Ordering::Relaxed));
            release(value);
            owned.free.push(index);
            index = slot.next.load(Ordering::Relaxed);
        }
    }

    /// How many slots are in use, for tests and diagnostics.
    ///
    /// # Safety
    ///
    /// As for [`RootTable::owned`].
    pub(crate) unsafe fn live(&self) -> usize {
        // SAFETY: passed on to the caller.
        let owned = unsafe { self.owned() };
        owned.chunks.len() * SLOTS_PER_CHUNK - owned.free.len()
    }

    /// Pushes `slot`, whose count has reached zero, for the owner to free.
    fn push_released(&self, slot: &Slot) {
        let mut head = self.released.load(Ordering::Relaxed);
        loop {
            slot.next.store(head, Ordering::Relaxed);
            match self.released.compare_exchange_weak(
                head,
                slot.index,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(current) => head = current,
            }
        }
    }
}

/// A Scheme value held by Rust code.
pub struct Handle(Inner);

enum Inner {
    Immediate(Value),
    Rooted {
        table: Arc<RootTable>,
        slot: NonNull<Slot>,
        tag: Tag,
    },
}

// SAFETY: a rooted handle only touches its slot's atomic count, and its
// table's release stack, without its owning isolate; the slot lives as long as
// the table, which the handle keeps alive.
unsafe impl Send for Handle {}
// SAFETY: as above; every method on `&Handle` that reads the value goes
// through the owning isolate.
unsafe impl Sync for Handle {}

impl Handle {
    /// A handle to a fixnum, if `n` fits in 61 bits.
    pub fn fixnum(n: i64) -> Option<Handle> {
        Value::fixnum(n).map(|v| Handle(Inner::Immediate(v)))
    }

    /// A handle to a boolean.
    pub fn boolean(b: bool) -> Handle {
        Handle(Inner::Immediate(Value::boolean(b)))
    }

    /// A handle to a character.
    pub fn character(c: char) -> Handle {
        Handle(Inner::Immediate(Value::character(c)))
    }

    /// A handle to the empty list.
    pub fn empty_list() -> Handle {
        Handle(Inner::Immediate(Value::EMPTY_LIST))
    }

    pub(crate) fn immediate(v: Value) -> Handle {
        debug_assert!(!v.is_pointer());
        Handle(Inner::Immediate(v))
    }

    pub(crate) fn rooted(table: Arc<RootTable>, slot: NonNull<Slot>, tag: Tag) -> Handle {
        Handle(Inner::Rooted { table, slot, tag })
    }

    /// The value's tag, which needs no context.
    pub fn tag(&self) -> Tag {
        match &self.0 {
            Inner::Immediate(v) => v.tag(),
            Inner::Rooted { tag, .. } => *tag,
        }
    }

    /// Whether this is a pair.
    pub fn is_pair(&self) -> bool {
        self.tag() == Tag::Pair
    }

    /// The fixnum, if this is one. Needs no context.
    pub fn as_fixnum(&self) -> Option<i64> {
        self.as_immediate()?.as_fixnum()
    }

    /// The boolean, if this is one. Needs no context.
    pub fn as_boolean(&self) -> Option<bool> {
        self.as_immediate()?.as_boolean()
    }

    /// The character, if this is one. Needs no context.
    pub fn as_character(&self) -> Option<char> {
        self.as_immediate()?.as_character()
    }

    fn as_immediate(&self) -> Option<Value> {
        match &self.0 {
            Inner::Immediate(v) => Some(*v),
            Inner::Rooted { .. } => None,
        }
    }

    /// The raw value, if `table` is the owner's root table.
    pub(crate) fn value_in(&self, table: &Arc<RootTable>) -> Result<Value, Error> {
        match &self.0 {
            Inner::Immediate(v) => Ok(*v),
            Inner::Rooted {
                table: own, slot, ..
            } => {
                if !Arc::ptr_eq(own, table) {
                    return Err(Error::WrongIsolate);
                }
                // SAFETY: the slot lives as long as its table, which this
                // handle keeps alive; the caller is the owning isolate.
                let slot = unsafe { slot.as_ref() };
                Ok(Value::from_raw(slot.value.load(Ordering::Relaxed)))
            }
        }
    }
}

impl Clone for Handle {
    fn clone(&self) -> Handle {
        match &self.0 {
            Inner::Immediate(v) => Handle(Inner::Immediate(*v)),
            Inner::Rooted { table, slot, tag } => {
                // SAFETY: the slot lives as long as its table.
                let count = unsafe { slot.as_ref() }
                    .count
                    .fetch_add(1, Ordering::Relaxed);
                assert!(count < u32::MAX / 2, "too many clones of one handle");
                Handle(Inner::Rooted {
                    table: Arc::clone(table),
                    slot: *slot,
                    tag: *tag,
                })
            }
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if let Inner::Rooted { table, slot, .. } = &self.0 {
            // SAFETY: the slot lives as long as its table.
            let slot = unsafe { slot.as_ref() };
            if slot.count.fetch_sub(1, Ordering::Release) == 1 {
                fence(Ordering::Acquire);
                table.push_released(slot);
            }
        }
    }
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Inner::Immediate(v) => write!(f, "Handle({v:?})"),
            Inner::Rooted { tag, .. } => write!(f, "Handle(#<{tag:?}>)"),
        }
    }
}
