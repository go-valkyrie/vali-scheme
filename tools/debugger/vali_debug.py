"""Vali's Scheme frames and values, read from a stopped process's memory.

Shared by the GDB extension (vali_gdb.py) and the LLDB one (vali_lldb.py),
which supply a `Target` that reads memory, and the interpreter's `fp` and
`pc` locals, read from DWARF (ADR 0004 #6).

What is read here is described by crates/vali-runtime/src/debug.rs, in C
layout: each function's `Code` starts with a `DebugFunction`, which points
to its memory strategy's `DebugLayout`. Both carry DEBUG_VERSION.
"""

import struct

DEBUG_VERSION = 1

# The frame header (docs/bytecode.md): the saved frame pointer, the return
# address, then the caller's position word.
ENTRY = (1 << 64) - 1
UNDERFLOW = 1 << 61

# A closure's pointer to its Code: closures always have a header, so this is
# the same under every strategy (vali_abi::Layout::closure_code). It is
# checked against the layout the function then points to.
CLOSURE_CODE = 8 - 0b010

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

IMMEDIATES = {2: "()", 3: "#<eof>", 4: "#<unspecified>", 5: "#<default>"}
MARKERS = {0: "#<unbound>", 1: "#<uninitialised>", 2: "#<tombstone>"}


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
    if closure & 0b111 != 0b010:
        raise ValueError("slot 0 does not hold a closure: %#x" % closure)
    function = Function(target, target.u64(wrap(closure + CLOSURE_CODE)))
    if function.layout.closure_code != CLOSURE_CODE:
        raise ValueError("unexpected closure layout")
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
        position = target.u64(fp + 16)
        if position == ENTRY:
            break
        # The caller waits at the call before where it resumes, and the
        # call's window starts at its base.
        pc = max(((position >> 3) & 0xFFFFFFFF) - 1, 0)
        call_base = (position >> 35) & 0xFFFF
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
    tag = word & 0b111
    if tag == 0b000:
        return str(_signed(word) >> 3)
    if tag == 0b111:
        subtag, payload = (word >> 3) & 0x1F, word >> 8
        if subtag == 0:
            name = CHARACTER_NAMES.get(payload)
            return "#\\" + (name if name else chr(payload))
        if subtag == 1:
            return "#t" if payload else "#f"
        if subtag == 6:
            return MARKERS.get(payload, "#<marker %d>" % payload)
        return IMMEDIATES.get(subtag, "#<immediate %#x>" % word)
    if depth == 0:
        return "..."
    if tag == 0b001:
        return _show_list(target, layout, word, depth)
    if tag == 0b010:
        return "#<procedure %s>" % function_of(target, word).name
    if tag == 0b011:
        name = target.u64(wrap(word + layout.symbol_name))
        return _string_text(target, layout, name)
    if tag == 0b100:
        text = _string_text(target, layout, word)
        return '"%s"' % text.replace("\\", "\\\\").replace('"', '\\"')
    if tag == 0b101:
        return _show_flonum(target.f64(wrap(word + layout.flonum)))
    header = target.u64(wrap(word + layout.other_header))
    type_code = header & 0xFF
    if type_code == 11:
        count = target.u64(wrap(word + layout.vector_length))
        elements = [show(target, layout,
                         target.u64(wrap(word + layout.vector_length + 8 * (i + 1))),
                         depth - 1)
                    for i in range(min(count, 10))]
        return "#(%s%s)" % (" ".join(elements), " ..." if count > 10 else "")
    if type_code == 13:
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
    while word & 0b111 == 0b001 and len(items) < 10:
        items.append(show(target, layout, target.u64(wrap(word + layout.car)), depth - 1))
        word = target.u64(wrap(word + layout.cdr))
    if word & 0b111 == 0b001:
        items.append("...")
    elif word != (2 << 3 | 0b111):
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
