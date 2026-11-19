"""Vali's Scheme frames and values, read from a stopped process's memory.

Shared by the GDB extension (vali_gdb.py) and the LLDB one (vali_lldb.py),
which supply a `Target` that reads memory, and the interpreter's `fp` and
`pc` locals, read from DWARF (ADR 0004 #6).

What is read here is described by crates/vali-runtime/src/debug.rs, in C
layout: each function's `Code` starts with a `DebugFunction`, which points
to its memory strategy's `DebugLayout`. Both carry DEBUG_VERSION. The layout also carries the encoding of position
words, tags and immediates, which is read from it rather than copied here;
the few constants below are what is needed to reach the first layout, and
are checked against it, so a mismatch is an error, not a wrong answer.
"""

import struct

DEBUG_VERSION = 2

# Needed before a layout can be read, and checked against it: a closure's
# tag, and its pointer to its Code. Closures always have a header, so the
# pointer is the same under every strategy
# (vali_abi::Layout::closure_code).
CLOSURE_TAG = 0b010
CLOSURE_CODE = 8 - CLOSURE_TAG

# The frame header (docs/bytecode.md) is the saved frame pointer, the return
# address, then the caller's position word, which the layout describes.

TYPE_NAMES = {
    1: "pair", 3: "flonum", 5: "procedure", 7: "symbol", 9: "string",
    11: "vector", 13: "bytevector", 15: "box", 17: "bignum", 19: "ratnum",
    21: "exact-complex", 23: "inexact-complex", 25: "record",
    27: "record-type", 29: "keyword", 31: "code", 33: "continuation",
    35: "stack-segment", 37: "weak-pair", 39: "ephemeron",
}

CHARACTER_NAMES = {
    0x0: "null", 0x7: "alarm", 0x8: "backspace", 0x9: "tab", 0xA: "newline",
    0xD: "return", 0x1B: "escape", 0x20: "space", 0x7F: "delete",
}



class Target:
    """Reads the stopped process's memory. Subclasses supply `read`."""

    def read(self, address, size):
        raise NotImplementedError

    def u64(self, address):
        return struct.unpack("<Q", self.read(address, 8))[0]

    def u32(self, address):
        return struct.unpack("<I", self.read(address, 4))[0]

    def u16(self, address):
        return struct.unpack("<H", self.read(address, 2))[0]

    def i32(self, address):
        return struct.unpack("<i", self.read(address, 4))[0]

    def f64(self, address):
        return struct.unpack("<d", self.read(address, 8))[0]

    def text(self, address, size, limit=200):
        data = self.read(address, min(size, limit)).decode("utf-8", "replace")
        return data + ("..." if size > limit else "")


def wrap(address):
    """An address, wrapped to 64 bits, as Rust's offsets wrap."""
    return address % (1 << 64)


class Layout:
    """A strategy's DebugLayout: offsets from a tagged value, in bytes."""

    # The 64-bit words after the offsets, in the order of the Rust struct
    # (crates/vali-runtime/src/debug.rs).
    ENCODING = [
        "entry", "underflow", "resume_shift", "resume_mask", "base_shift",
        "base_mask", "tag_fixnum", "tag_pair", "tag_closure", "tag_symbol",
        "tag_string", "tag_flonum", "tag_other", "tag_immediate",
        "subtag_shift", "subtag_mask", "payload_shift", "subtag_character",
        "subtag_boolean", "subtag_empty_list", "subtag_eof",
        "subtag_unspecified", "subtag_default", "subtag_marker",
        "marker_unbound", "marker_uninitialised", "marker_tombstone",
        "type_vector", "type_bytevector",
    ]

    FIELDS = ["car", "cdr", "flonum", "closure_code", "symbol_name",
              "string_bytes", "bytevector_length", "other_header",
              "vector_length", "record_type"]

    def __init__(self, target, address):
        self.version = target.u32(address)
        if self.version != DEBUG_VERSION:
            raise ValueError(
                f"Vali debug layout version {self.version}; "
                f"this extension reads {DEBUG_VERSION}")
        self.tag_bits = target.u32(address + 4)
        for i, name in enumerate(self.FIELDS):
            setattr(self, name, target.i32(address + 8 + 4 * i))
        start = address + 8 + 4 * len(self.FIELDS)
        start += -start % 8
        for i, name in enumerate(self.ENCODING):
            setattr(self, name, target.u64(start + 8 * i))
        if self.tag_bits != 3 or self.tag_closure != CLOSURE_TAG:
            raise ValueError("Vali debug layout: unexpected tag encoding")
        if self.closure_code != CLOSURE_CODE:
            raise ValueError("Vali debug layout: unexpected closure layout")

    def immediate_name(self, word):
        """A fixed immediate's name, or None."""
        subtag = (word >> self.subtag_shift) & self.subtag_mask
        names = {
            self.subtag_empty_list: "()", self.subtag_eof: "#<eof>",
            self.subtag_unspecified: "#<unspecified>",
            self.subtag_default: "#<default>",
        }
        return names.get(subtag)

    def marker_name(self, payload):
        names = {
            self.marker_unbound: "#<unbound>",
            self.marker_uninitialised: "#<uninitialised>",
            self.marker_tombstone: "#<tombstone>",
        }
        return names.get(payload, "#<marker %d>" % payload)


class Function:
    """A DebugFunction: a function's name, frame size and line table."""

    def __init__(self, target, address):
        self.version = target.u32(address)
        if self.version != DEBUG_VERSION:
            raise ValueError(
                f"Vali debug record version {self.version}; "
                f"this extension reads {DEBUG_VERSION}")
        self.slots = target.u32(address + 4)
        self.layout = Layout(target, target.u64(address + 8))
        self.name = target.text(target.u64(address + 16), target.u64(address + 24))
        self.positions = target.u64(address + 32)
        self.positions_len = target.u64(address + 40)
        self.sources = target.u64(address + 48)
        self.sources_len = target.u64(address + 56)
        self.target = target

    def position(self, pc):
        """Where the instruction at `pc` came from, as `source:line:column`
        with the column from 1, or None."""
        found = None
        for i in range(self.positions_len):
            entry = self.positions + 16 * i
            if self.target.u32(entry) > pc:
                break
            found = entry
        if found is None:
            return None
        t = self.target
        source = t.u16(found + 4)
        if source >= self.sources_len:
            return None
        name = self.sources + 16 * source
        return "%s:%d:%d" % (t.text(t.u64(name), t.u64(name + 8)),
                             t.u32(found + 8), t.u32(found + 12) + 1)


def function_of(target, closure):
    """The Function of the closure `closure`, a tagged value."""
    if closure & 0b111 != CLOSURE_TAG:
        raise ValueError("slot 0 does not hold a closure: %#x" % closure)
    function = Function(target, target.u64(wrap(closure + CLOSURE_CODE)))
    return function


class Frame:
    """One Scheme frame: its frame pointer, its function, and the
    instruction it is at, or waiting in."""

    def __init__(self, target, fp, pc):
        self.target = target
        self.fp = fp
        self.pc = pc
        self.function = function_of(target, self.slot(0))
        # The base of the call it waits in, whose callee's frame header
        # fills three of its slots, if that callee is a Scheme frame.
        self.call_base = None

    def slot(self, i):
        return self.target.u64(self.fp - 8 * (i + 1))

    def describe(self):
        where = self.function.position(self.pc)
        return self.function.name + (" (%s)" % where if where else "")


def frames(target, fp, pc, limit=1000):
    """The Scheme frames from the innermost, at `fp` and `pc`, out to the
    host's call into the VM."""
    result = []
    call_base = None
    while len(result) < limit:
        try:
            frame = Frame(target, fp, pc)
        except (ValueError, MemoryError):
            break
        frame.call_base = call_base
        result.append(frame)
        layout = frame.function.layout
        position = target.u64(fp + 16)
        if position == layout.entry:
            break
        # The caller waits at the call before where it resumes, and the
        # call's window starts at its base.
        pc = max(((position >> layout.resume_shift) & layout.resume_mask) - 1, 0)
        call_base = (position >> layout.base_shift) & layout.base_mask
        fp = target.u64(fp)
    return result


def show(target, layout, word, depth=4):
    """`word` as Scheme's `write` would show it, as far as can be read."""
    try:
        return _show(target, layout, word, depth)
    except Exception:
        return "#<unreadable %#x>" % word


def _signed(word):
    return word - (1 << 64) if word >= 1 << 63 else word


def _show(target, layout, word, depth):
    tag = word & ((1 << layout.tag_bits) - 1)
    if tag == layout.tag_fixnum:
        return str(_signed(word) >> layout.tag_bits)
    if tag == layout.tag_immediate:
        subtag = (word >> layout.subtag_shift) & layout.subtag_mask
        payload = word >> layout.payload_shift
        if subtag == layout.subtag_character:
            name = CHARACTER_NAMES.get(payload)
            return "#\\" + (name if name else chr(payload))
        if subtag == layout.subtag_boolean:
            return "#t" if payload else "#f"
        if subtag == layout.subtag_marker:
            return layout.marker_name(payload)
        return layout.immediate_name(word) or "#<immediate %#x>" % word
    if depth == 0:
        return "..."
    if tag == layout.tag_pair:
        return _show_list(target, layout, word, depth)
    if tag == layout.tag_closure:
        return "#<procedure %s>" % function_of(target, word).name
    if tag == layout.tag_symbol:
        name = target.u64(wrap(word + layout.symbol_name))
        return _string_text(target, layout, name)
    if tag == layout.tag_string:
        text = _string_text(target, layout, word)
        return '"%s"' % text.replace("\\", "\\\\").replace('"', '\\"')
    if tag == layout.tag_flonum:
        return _show_flonum(target.f64(wrap(word + layout.flonum)))
    header = target.u64(wrap(word + layout.other_header))
    type_code = header & 0xFF
    if type_code == layout.type_vector:
        count = target.u64(wrap(word + layout.vector_length))
        elements = [show(target, layout,
                         target.u64(wrap(word + layout.vector_length + 8 * (i + 1))),
                         depth - 1)
                    for i in range(min(count, 10))]
        return "#(%s%s)" % (" ".join(elements), " ..." if count > 10 else "")
    if type_code == layout.type_bytevector:
        count = target.u64(wrap(word + layout.bytevector_length))
        data = target.read(wrap(word + layout.bytevector_length + 8), min(count, 16))
        return "#u8(%s%s)" % (" ".join(str(b) for b in data),
                              " ..." if count > 16 else "")
    return "#<%s>" % TYPE_NAMES.get(type_code, "object %d" % type_code)


def _string_text(target, layout, string):
    bytes_ = target.u64(wrap(string + layout.string_bytes))
    count = target.u64(wrap(bytes_ + layout.bytevector_length))
    return target.text(wrap(bytes_ + layout.bytevector_length + 8), count)


def _show_list(target, layout, word, depth):
    items = []
    tag_mask = (1 << layout.tag_bits) - 1
    while word & tag_mask == layout.tag_pair and len(items) < 10:
        items.append(show(target, layout, target.u64(wrap(word + layout.car)), depth - 1))
        word = target.u64(wrap(word + layout.cdr))
    if word & tag_mask == layout.tag_pair:
        items.append("...")
    elif not (word & tag_mask == layout.tag_immediate
              and layout.immediate_name(word) == "()"):
        items.extend([".", show(target, layout, word, depth - 1)])
    return "(%s)" % " ".join(items)


def _show_flonum(x):
    if x != x:
        return "+nan.0"
    if x in (float("inf"), float("-inf")):
        return "+inf.0" if x > 0 else "-inf.0"
    text = repr(x)
    return text if any(c in text for c in ".en") else text + ".0"


def backtrace(target, fp, pc):
    """`vali-bt`: one line per Scheme frame, innermost first."""
    found = frames(target, fp, pc)
    if not found:
        return "no Scheme frames"
    return "\n".join("#%d %s" % (i, f.describe()) for i, f in enumerate(found))


def frame_slots(target, fp, pc, n):
    """`vali-frame N`: frame N's procedure and slots, as values."""
    found = frames(target, fp, pc)
    if n >= len(found):
        return "no Scheme frame %d: there are %d" % (n, len(found))
    frame = found[n]
    layout = frame.function.layout
    lines = ["#%d %s" % (n, frame.describe())]
    base = frame.call_base
    for i in range(1, frame.function.slots):
        if base is not None and base <= i < base + 3:
            value = "(the header of the call it waits in)"
        else:
            value = show(target, layout, frame.slot(i))
        lines.append("  slot %d = %s" % (i, value))
    return "\n".join(lines)


def layout_near(target, fp, pc):
    """The layout of the innermost Scheme frame's strategy, to print values
    with, or None."""
    found = frames(target, fp, pc, limit=1)
    return found[0].function.layout if found else None
