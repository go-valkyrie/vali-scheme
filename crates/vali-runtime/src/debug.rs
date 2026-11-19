//! What Vali's GDB and LLDB extensions read (ADR 0004 #6): a description of
//! each function, and of the memory strategy's object layout, in C layout,
//! so the extensions can walk Scheme frames and print Scheme values without
//! knowing how Rust lays out Vali's own types.
//!
//! The extensions, in `tools/debugger/`, find the innermost Scheme frame
//! through a [`DebugState`] the VM keeps for each isolate, reached from the
//! exported symbol [`VALI_DEBUG_STATES`], which needs no debug information;
//! where the build has full debug information they read the interpreter's
//! `fp` and `pc` locals instead. From a frame, slot 0 is its closure, whose first word points to its function's
//! `Code`, which starts with a [`DebugFunction`]; that points to its
//! strategy's [`DebugLayout`]. Changing anything here changes
//! [`DEBUG_VERSION`], which the extensions check.

use crate::vm;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use vali_abi::immediate::{self, Marker, Subtag};
use vali_abi::{Layout, Tag, TypeCode};
use vali_bytecode::Position;

/// The version of these structures, which the extensions check. Bumped
/// whenever any of them, or what they describe, changes.
pub const DEBUG_VERSION: u32 = 3;

/// A string the extensions read: a pointer to UTF-8 bytes and their length.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DebugStr {
    /// The bytes.
    pub ptr: *const u8,
    /// How many.
    pub len: usize,
}

// SAFETY: a view of immutable bytes, kept alive by whatever holds the
// view, and only ever read.
unsafe impl Send for DebugStr {}
// SAFETY: as above.
unsafe impl Sync for DebugStr {}

impl DebugStr {
    /// A view of `s`, which must outlive it.
    pub(crate) fn of(s: &str) -> DebugStr {
        DebugStr {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }
}

/// A strategy's object layout: what the extensions need to print values.
/// Offsets are in bytes from a tagged value, as `vali_abi::Layout` gives
/// them.
#[repr(C)]
#[derive(Debug)]
pub struct DebugLayout {
    /// [`DEBUG_VERSION`].
    pub version: u32,
    /// How many low bits a tag takes (ADR 0002).
    pub tag_bits: u32,
    /// A pair's `car` and `cdr`.
    pub car: i32,
    pub cdr: i32,
    /// A flonum's `f64`.
    pub flonum: i32,
    /// A closure's pointer to its `Code`, which starts with a
    /// [`DebugFunction`].
    pub closure_code: i32,
    /// A symbol's name, a string.
    pub symbol_name: i32,
    /// A string's bytevector of UTF-8.
    pub string_bytes: i32,
    /// A bytevector's length; its bytes follow that word.
    pub bytevector_length: i32,
    /// The header of an object tagged "other", whose low byte is its type
    /// code.
    pub other_header: i32,
    /// A vector's length, then its elements, one word each.
    pub vector_length: i32,
    /// A record's type descriptor; its fields follow, one word each.
    pub record_type: i32,
    /// The encoding of frames and values, which the extensions read here
    /// instead of copying it. These are 64-bit words, from `entry` on, in
    /// the order of the extensions' `Layout.ENCODING`.
    ///
    /// The position word of the outermost frame, whose return leaves the
    /// VM.
    pub entry: u64,
    /// The flag in a position word whose caller is sealed in a
    /// continuation.
    pub underflow: u64,
    /// The shift and mask of the resume address in a position word.
    pub resume_shift: u64,
    pub resume_mask: u64,
    /// The shift and mask of the call's `base` in a position word.
    pub base_shift: u64,
    pub base_mask: u64,
    /// The tags (ADR 0002).
    pub tag_fixnum: u64,
    pub tag_pair: u64,
    pub tag_closure: u64,
    pub tag_symbol: u64,
    pub tag_string: u64,
    pub tag_flonum: u64,
    pub tag_other: u64,
    pub tag_immediate: u64,
    /// How an immediate is laid out: the shift and mask of its subtag, and
    /// the shift of its payload.
    pub subtag_shift: u64,
    pub subtag_mask: u64,
    pub payload_shift: u64,
    /// The subtags.
    pub subtag_character: u64,
    pub subtag_boolean: u64,
    pub subtag_empty_list: u64,
    pub subtag_eof: u64,
    pub subtag_unspecified: u64,
    pub subtag_default: u64,
    pub subtag_marker: u64,
    /// The internal markers.
    pub marker_unbound: u64,
    pub marker_uninitialised: u64,
    pub marker_tombstone: u64,
    /// The type codes of the objects the extensions look inside.
    pub type_vector: u64,
    pub type_bytevector: u64,
}

impl DebugLayout {
    /// The description of `layout`.
    pub(crate) const fn of(layout: Layout) -> DebugLayout {
        DebugLayout {
            version: DEBUG_VERSION,
            tag_bits: Tag::BITS,
            car: layout.car(),
            cdr: layout.cdr(),
            flonum: layout.flonum(),
            closure_code: layout.closure_code(),
            symbol_name: layout.symbol_name(),
            string_bytes: layout.string_bytes(),
            bytevector_length: layout.length(TypeCode::Bytevector),
            other_header: -(Tag::Other as i32),
            vector_length: layout.length(TypeCode::Vector),
            record_type: layout.record_type(),
            entry: vm::ENTRY,
            underflow: vm::UNDERFLOW,
            resume_shift: vm::POSITION_RESUME_SHIFT as u64,
            resume_mask: (1 << vm::POSITION_RESUME_BITS) - 1,
            base_shift: vm::POSITION_BASE_SHIFT as u64,
            base_mask: (1 << vm::POSITION_BASE_BITS) - 1,
            tag_fixnum: Tag::Fixnum as u64,
            tag_pair: Tag::Pair as u64,
            tag_closure: Tag::Closure as u64,
            tag_symbol: Tag::Symbol as u64,
            tag_string: Tag::String as u64,
            tag_flonum: Tag::Flonum as u64,
            tag_other: Tag::Other as u64,
            tag_immediate: Tag::Immediate as u64,
            subtag_shift: immediate::SUBTAG_SHIFT as u64,
            subtag_mask: (1 << immediate::SUBTAG_BITS) - 1,
            payload_shift: immediate::PAYLOAD_SHIFT as u64,
            subtag_character: Subtag::Character as u64,
            subtag_boolean: Subtag::Boolean as u64,
            subtag_empty_list: Subtag::EmptyList as u64,
            subtag_eof: Subtag::Eof as u64,
            subtag_unspecified: Subtag::Unspecified as u64,
            subtag_default: Subtag::Default as u64,
            subtag_marker: Subtag::Marker as u64,
            marker_unbound: Marker::Unbound as u64,
            marker_uninitialised: Marker::Uninitialised as u64,
            marker_tombstone: Marker::Tombstone as u64,
            type_vector: TypeCode::Vector as u64,
            type_bytevector: TypeCode::Bytevector as u64,
        }
    }
}

/// A function, as the extensions see it: the first field of every `Code`.
#[repr(C)]
#[derive(Debug)]
pub struct DebugFunction {
    /// [`DEBUG_VERSION`].
    pub version: u32,
    /// How many slots its frames have; 0 for a procedure implemented in
    /// Rust, which has no Scheme frame.
    pub slots: u32,
    /// Its strategy's layout.
    pub layout: *const DebugLayout,
    /// Its name.
    pub name: DebugStr,
    /// Its line table, sorted by instruction (ADR 0014 #9), and how many
    /// entries it has.
    pub positions: *const Position,
    pub positions_len: usize,
    /// The sources its positions name, by index.
    pub sources: *const DebugStr,
    pub sources_len: usize,
}

impl DebugFunction {
    /// A function with no Scheme frames or positions: one implemented in
    /// Rust, or one the VM handles itself.
    pub(crate) fn native(layout: &'static DebugLayout, name: &str) -> DebugFunction {
        DebugFunction {
            version: DEBUG_VERSION,
            slots: 0,
            layout,
            name: DebugStr::of(name),
            positions: std::ptr::null(),
            positions_len: 0,
            sources: std::ptr::null(),
            sources_len: 0,
        }
    }
}

/// Where an isolate's interpreter is, for a debugger to read.
///
/// The VM keeps one per isolate, and links them in a list that the exported
/// [`VALI_DEBUG_STATES`] heads, so the extensions find it with one symbol
/// and no debug information. `fp` and `pc` are the interpreter's, stored
/// before each call that can leave its loop and stop a debugger in native
/// code: Rust procedures, raising an error, growing the stack. Not at
/// every instruction, so a debugger stopped inside the loop itself, or in
/// an allocation, sees where it last left it.
/// `fp` is 0 while the isolate runs no Scheme code.
#[repr(C)]
#[derive(Debug)]
pub struct DebugState {
    /// [`DEBUG_VERSION`].
    pub version: u32,
    reserved: u32,
    /// The next isolate's record.
    pub next: AtomicPtr<DebugState>,
    /// The operating system's id for the thread running the isolate, in
    /// the debuggers' terms (Linux's tid, macOS's thread id), or 0.
    pub thread: AtomicU64,
    /// The frame pointer of the innermost Scheme frame.
    pub fp: AtomicU64,
    /// The instruction it is at, an index into its function's code.
    pub pc: AtomicU64,
}

/// The first isolate's [`DebugState`]; the extensions look this symbol up.
/// A host that strips its symbols must keep it.
#[unsafe(no_mangle)]
pub static VALI_DEBUG_STATES: AtomicPtr<DebugState> = AtomicPtr::new(std::ptr::null_mut());

/// Serialises changes to the list, which are rare: isolates being made and
/// dropped. A debugger reads it without the lock, from a stopped process.
static STATES: Mutex<()> = Mutex::new(());

/// An isolate's entry in the list, removed when it drops.
pub(crate) struct Registered(NonNull<DebugState>);

// SAFETY: the record is only atomics, and lives until this drops.
unsafe impl Send for Registered {}
// SAFETY: as above.
unsafe impl Sync for Registered {}

impl std::fmt::Debug for Registered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Registered")
    }
}

impl Registered {
    /// A new record, first in the list.
    pub(crate) fn new() -> Registered {
        let state = NonNull::from(Box::leak(Box::new(DebugState {
            version: DEBUG_VERSION,
            reserved: 0,
            next: AtomicPtr::new(std::ptr::null_mut()),
            thread: AtomicU64::new(0),
            fp: AtomicU64::new(0),
            pc: AtomicU64::new(0),
        })));
        let _guard = STATES.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: just made, and not shared until it is linked.
        unsafe {
            state
                .as_ref()
                .next
                .store(VALI_DEBUG_STATES.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        VALI_DEBUG_STATES.store(state.as_ptr(), Ordering::Release);
        Registered(state)
    }

    /// The record, which lives as long as `self`.
    pub(crate) fn state(&self) -> *const DebugState {
        self.0.as_ptr()
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        let _guard = STATES.lock().unwrap_or_else(|e| e.into_inner());
        let mine = self.0.as_ptr();
        // SAFETY: every record in the list is alive: each is unlinked here
        // before it is freed.
        unsafe {
            let next = (*mine).next.load(Ordering::Relaxed);
            let mut link = &VALI_DEBUG_STATES;
            loop {
                let here = link.load(Ordering::Relaxed);
                if here.is_null() {
                    break;
                }
                if here == mine {
                    link.store(next, Ordering::Release);
                    break;
                }
                link = &(*here).next;
            }
            drop(Box::from_raw(mine));
        }
    }
}

/// The id of the calling thread, as the debuggers give it, or 0.
pub(crate) fn thread_id() -> u64 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `gettid` takes no arguments and cannot fail.
        unsafe { libc::syscall(libc::SYS_gettid) as u64 }
    }
    #[cfg(target_os = "macos")]
    {
        let mut id = 0u64;
        // SAFETY: a null thread means the calling one; `id` is valid.
        unsafe { libc::pthread_threadid_np(0, &mut id) };
        id
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    //! The extensions read this module's structures from Python. This test
    //! decodes a real `DebugLayout` with `tools/debugger/vali_debug.py` and
    //! compares every field, so a change here that the extensions have not
    //! followed fails here, on every machine with a `python3`, and not only
    //! in the debugger tests.

    use super::*;
    use crate::NoCollection;
    use crate::code::Code;
    use std::collections::BTreeMap;
    use std::process::Command;

    const SCRIPT: &str = r#"
import sys
sys.path.insert(0, sys.argv[2])
import vali_debug
data = bytes.fromhex(sys.argv[1])
class T(vali_debug.Target):
    def read(self, address, size):
        return data[address:address + size]
layout = vali_debug.Layout(T(), 0)
for name in ["version", "tag_bits"] + layout.FIELDS + layout.ENCODING:
    print("%s=%d" % (name, getattr(layout, name)))
for code, name in sorted(vali_debug.TYPE_NAMES.items()):
    print("type %d=%s" % (code, name))
print("DEBUG_VERSION=%d" % vali_debug.DEBUG_VERSION)
print("symbol=%s" % vali_debug.STATE_SYMBOL)
for name, offset in sorted(vali_debug.STATE_FIELDS.items()):
    print("state %s=%d" % (name, offset))
"#;

    fn expected(l: &DebugLayout) -> Vec<(&'static str, u64)> {
        let signed = |n: i32| n as i64 as u64;
        vec![
            ("version", l.version.into()),
            ("tag_bits", l.tag_bits.into()),
            ("car", signed(l.car)),
            ("cdr", signed(l.cdr)),
            ("flonum", signed(l.flonum)),
            ("closure_code", signed(l.closure_code)),
            ("symbol_name", signed(l.symbol_name)),
            ("string_bytes", signed(l.string_bytes)),
            ("bytevector_length", signed(l.bytevector_length)),
            ("other_header", signed(l.other_header)),
            ("vector_length", signed(l.vector_length)),
            ("record_type", signed(l.record_type)),
            ("entry", l.entry),
            ("underflow", l.underflow),
            ("resume_shift", l.resume_shift),
            ("resume_mask", l.resume_mask),
            ("base_shift", l.base_shift),
            ("base_mask", l.base_mask),
            ("tag_fixnum", l.tag_fixnum),
            ("tag_pair", l.tag_pair),
            ("tag_closure", l.tag_closure),
            ("tag_symbol", l.tag_symbol),
            ("tag_string", l.tag_string),
            ("tag_flonum", l.tag_flonum),
            ("tag_other", l.tag_other),
            ("tag_immediate", l.tag_immediate),
            ("subtag_shift", l.subtag_shift),
            ("subtag_mask", l.subtag_mask),
            ("payload_shift", l.payload_shift),
            ("subtag_character", l.subtag_character),
            ("subtag_boolean", l.subtag_boolean),
            ("subtag_empty_list", l.subtag_empty_list),
            ("subtag_eof", l.subtag_eof),
            ("subtag_unspecified", l.subtag_unspecified),
            ("subtag_default", l.subtag_default),
            ("subtag_marker", l.subtag_marker),
            ("marker_unbound", l.marker_unbound),
            ("marker_uninitialised", l.marker_uninitialised),
            ("marker_tombstone", l.marker_tombstone),
            ("type_vector", l.type_vector),
            ("type_bytevector", l.type_bytevector),
        ]
    }

    /// Whether the list `VALI_DEBUG_STATES` heads holds `state`.
    fn listed(state: *const DebugState) -> bool {
        let mut here = VALI_DEBUG_STATES.load(Ordering::Acquire);
        while !here.is_null() {
            if std::ptr::eq(here, state) {
                return true;
            }
            // SAFETY: the list only holds live records, and the caller
            // holds the ones it checks.
            here = unsafe { (*here).next.load(Ordering::Acquire) };
        }
        false
    }

    #[test]
    fn isolates_are_listed_while_they_live() {
        let first = Registered::new();
        let second = Registered::new();
        let (a, b) = (first.state(), second.state());
        assert!(listed(a) && listed(b));
        drop(first);
        assert!(!listed(a) && listed(b));
        drop(second);
        assert!(!listed(b));
    }

    #[test]
    fn extension_decodes_the_layout() {
        let layout = &Code::<NoCollection>::DEBUG_LAYOUT;
        // SAFETY: `DebugLayout` is `repr(C)` with no padding: 32-bit words,
        // then 64-bit ones from an 8-aligned offset.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (layout as *const DebugLayout).cast::<u8>(),
                std::mem::size_of::<DebugLayout>(),
            )
        };
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/debugger");
        let output = match Command::new("python3")
            .args(["-I", "-c", SCRIPT, &hex, dir])
            .output()
        {
            Ok(output) => output,
            Err(_) if std::env::var_os("CI").is_none() => {
                eprintln!("python3 is not installed; skipping");
                return;
            }
            Err(e) => panic!("python3 is required in CI: {e}"),
        };
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "vali_debug.py failed:\n{}{text}",
            String::from_utf8_lossy(&output.stderr)
        );
        let seen: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
        let want = expected(layout);
        let words = (std::mem::size_of::<DebugLayout>() - 48) / 8;
        assert_eq!(
            want.len(),
            2 + 10 + words,
            "a DebugLayout field is missing from this test"
        );
        for (name, value) in &want {
            // Python prints the offsets signed.
            let shown = match *name {
                "car" | "cdr" | "flonum" | "closure_code" | "symbol_name" | "string_bytes"
                | "bytevector_length" | "other_header" | "vector_length" | "record_type" => {
                    (*value as i64).to_string()
                }
                _ => value.to_string(),
            };
            assert_eq!(
                seen.get(name).copied(),
                Some(shown.as_str()),
                "vali_debug.py reads {name} differently"
            );
        }
        assert_eq!(
            seen.get("DEBUG_VERSION").copied(),
            Some(DEBUG_VERSION.to_string().as_str())
        );
        // Where it reads the VM's per-isolate record, and the symbol it
        // finds that through.
        use std::mem::offset_of;
        assert_eq!(seen.get("symbol").copied(), Some("VALI_DEBUG_STATES"));
        for (name, offset) in [
            ("version", offset_of!(DebugState, version)),
            ("next", offset_of!(DebugState, next)),
            ("thread", offset_of!(DebugState, thread)),
            ("fp", offset_of!(DebugState, fp)),
            ("pc", offset_of!(DebugState, pc)),
        ] {
            let key = format!("state {name}");
            assert_eq!(
                seen.get(key.as_str()).copied(),
                Some(offset.to_string().as_str()),
                "vali_debug.py reads the record's {name} differently"
            );
        }
        // The names it prints for type codes.
        let types: Vec<(u8, &str)> = text
            .lines()
            .filter_map(|l| l.strip_prefix("type ")?.split_once('='))
            .map(|(code, name)| (code.parse().unwrap(), name))
            .collect();
        assert!(!types.is_empty());
        for (code, name) in types {
            let known = TypeCode::from_u8(code)
                .unwrap_or_else(|| panic!("vali_debug.py names unassigned type code {code}"));
            let rust = match known {
                TypeCode::Closure => "procedure".to_string(),
                other => format!("{other:?}").to_lowercase(),
            };
            let squashed: String = name.chars().filter(|c| *c != '-').collect();
            assert_eq!(
                squashed, rust,
                "vali_debug.py calls type code {code} {name}, Rust {known:?}"
            );
        }
    }
}
