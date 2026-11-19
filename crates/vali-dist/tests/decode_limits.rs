//! Decoding allocates in proportion to its input: a small input cannot make
//! `decode` reserve gigabytes, however its counts and nesting are chosen
//! (ADR 0017 #13).
//!
//! A global allocator tracks the bytes live at once and the largest single
//! request, so each case can say how much memory its input may cost. It is
//! one test, so the cases run one at a time and the numbers are theirs
//! alone.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use vali_dist::{Writer, decode};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static LARGEST: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Refuse what no test here should ask for, so a regression fails
        // the test instead of the machine.
        if layout.size() > 1 << 30 {
            return std::ptr::null_mut();
        }
        let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
        PEAK.fetch_max(live, Relaxed);
        LARGEST.fetch_max(layout.size(), Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > 1 << 30 {
            return std::ptr::null_mut();
        }
        if new_size >= layout.size() {
            let live = LIVE.fetch_add(new_size - layout.size(), Relaxed) + new_size - layout.size();
            PEAK.fetch_max(live, Relaxed);
        } else {
            LIVE.fetch_sub(layout.size() - new_size, Relaxed);
        }
        LARGEST.fetch_max(new_size, Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Decodes `bytes` and returns the most memory it had live at once beyond
/// what was live before, and what it made of them.
fn measure(bytes: &[u8]) -> (usize, Result<vali_dist::Module, vali_dist::DecodeError>) {
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    LARGEST.store(0, Relaxed);
    let decoded = decode(bytes);
    (PEAK.load(Relaxed) - before, decoded)
}

/// Checks that decoding `bytes` used no more than `per_byte` bytes of
/// memory for each of them, plus 4 MB, and took under `seconds`.
#[track_caller]
fn bounded(case: &str, bytes: &[u8], per_byte: usize, seconds: u64) -> bool {
    let start = std::time::Instant::now();
    let (peak, decoded) = measure(bytes);
    let took = start.elapsed();
    eprintln!(
        "{case}: {} bytes in, peak {} kB, {:?}, {}",
        bytes.len(),
        peak / 1024,
        took,
        match &decoded {
            Ok(_) => "decoded".to_owned(),
            Err(e) => format!("refused: {e}"),
        }
    );
    assert!(
        peak <= per_byte * bytes.len() + (4 << 20),
        "{case}: {} bytes took {peak} bytes of memory",
        bytes.len()
    );
    assert!(took.as_secs() < seconds, "{case}: took {took:?}");
    decoded.is_ok()
}

/// The function record's header: no name, no parameters, `free` free
/// variables, no inlined copies.
fn header(free: u64) -> Writer {
    let mut f = Writer::new();
    f.bool(false); // no name
    f.byte(0); // required
    f.byte(0); // optional
    f.bool(false); // rest
    f.uint(free);
    f.uint(0); // inlined copies
    f
}

/// A module of the functions in `records`, the first its body, with one
/// constant, `()`.
fn module(records: &[Vec<u8>]) -> Vec<u8> {
    let mut w = Writer::new();
    w.str("m");
    w.uint(0); // sources
    w.uint(1); // constants
    w.byte(8); // ()
    w.uint(0); // globals
    w.uint(0); // exports
    w.u32(0); // body
    w.usize(records.len());
    for record in records {
        w.bytes(record);
    }
    w.finish()
}

fn returns_constant(mut f: Writer) -> Vec<u8> {
    f.byte(4); // return
    f.uint(1); // c0
    f.finish()
}

/// Join points, each declaring 65,536 parameters: they take no bytes, so
/// the join points are seven bytes each and the variables over 9,000 times
/// that in memory.
fn many_join_parameters(joins: usize) -> Vec<u8> {
    let mut f = header(0);
    for _ in 0..joins {
        f.byte(1); // join
        f.uint(1 << 16); // parameters
        f.bool(false); // not recursive
        f.byte(4); // handler: return c0
        f.uint(1);
    }
    f.byte(4); // body: return c0
    f.uint(1);
    module(&[f.finish()])
}

/// Many functions, each declaring 65,536 free variables.
fn many_free_variables(functions: usize) -> Vec<u8> {
    let mut records = vec![returns_constant(header(0))];
    for _ in 1..functions {
        records.push(returns_constant(header(1 << 16)));
    }
    module(&records)
}

/// A constant that is a vector declaring all the bytes after it as
/// elements, inside one that does, and so on: each reserved room for as
/// many elements as there were bytes left, all open at once.
fn nested_vectors(length: usize) -> Vec<u8> {
    let mut w = Writer::new();
    w.str("m");
    w.uint(0); // sources
    w.uint(1); // one constant
    let mut bytes = w.finish();
    let headers = (length - bytes.len()) / 4;
    let end = bytes.len() + headers * 4;
    for _ in 0..headers {
        let after = bytes.len() + 4;
        let n = (end - after) as u64;
        // The tag, then the length in exactly three bytes.
        bytes.extend([
            14,
            (n & 0x7F) as u8 | 0x80,
            ((n >> 7) & 0x7F) as u8 | 0x80,
            (n >> 14) as u8,
        ]);
    }
    bytes
}

/// A function declaring 65,536 inlined copies, with no bytes for them.
fn many_inlined_copies() -> Vec<u8> {
    let mut f = Writer::new();
    f.bool(false);
    f.byte(0);
    f.byte(0);
    f.bool(false);
    f.uint(0);
    f.uint(1 << 16);
    module(&[f.finish()])
}

/// Counts that claim far more than the input holds, in every place the
/// decoder reads one.
fn huge_counts() -> Vec<Vec<u8>> {
    let mut all = Vec::new();
    for n in [1u64 << 20, 1 << 32, u64::MAX] {
        // Sources, constants, globals, exports and functions.
        for place in 0..5 {
            let mut w = Writer::new();
            w.str("m");
            for field in 0..5 {
                w.uint(if field == place { n } else { 0 });
            }
            all.push(w.finish());
        }
        // A bignum's limbs, a string's and bytevector's length, and a
        // vector's elements.
        for tag in [1u8, 10, 15, 14] {
            let mut w = Writer::new();
            w.str("m");
            w.uint(0);
            w.uint(1);
            w.byte(tag);
            if tag == 1 {
                w.bool(false);
            }
            w.uint(n);
            all.push(w.finish());
        }
    }
    all
}

#[test]
fn decoding_stays_in_proportion_to_its_input() {
    // 56 KB of join points took 3.1 GB, and as many free variables.
    let input = many_join_parameters(8_000);
    assert!(!bounded("join-point parameters", &input, 64, 5));
    let input = many_free_variables(8_000);
    assert!(!bounded("free variables", &input, 64, 5));
    // 64 KB of nested vectors reserved about 17 GB.
    let input = nested_vectors(200_000);
    assert!(!bounded("nested vectors", &input, 64, 5));
    assert!(!bounded("inlined copies", &many_inlined_copies(), 64, 5));
    for (n, input) in huge_counts().iter().enumerate() {
        assert!(!bounded(&format!("huge count {n}"), input, 64, 5));
    }

    // What is honest still decodes: a few join points and free variables
    // beyond one a byte, in a module as small as this.
    let mut f = header(0);
    f.byte(1);
    f.uint(3);
    f.bool(false);
    f.byte(4);
    f.uint(1);
    f.byte(4);
    f.uint(1);
    assert!(bounded("a join point", &module(&[f.finish()]), 64, 5));
    let records = [
        returns_constant(header(0)),
        returns_constant(header(2)),
        returns_constant(header(300)),
    ];
    assert!(bounded("free variables", &module(&records), 64, 5));
}
