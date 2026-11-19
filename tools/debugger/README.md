# Debugger extensions

GDB and LLDB extensions that show Vali's Scheme frames and values in a
stopped process (ADR 0004 #6).

```
gdb:   source tools/debugger/vali_gdb.py
lldb:  command script import tools/debugger/vali_lldb.py
```

| Command | Shows |
|---|---|
| `vali-bt` | The Scheme frames, innermost first, with source positions |
| `vali-frame N` | Scheme frame N's procedure and its slots, as values |
| `vali-print EXPR` | A Vali value, or a raw word, as Scheme writes it |

Values of type `vali_runtime::value::Value` are also shown as Scheme writes
them, through a GDB pretty-printer and an LLDB summary.

They work whether or not `vali-runtime` was built with the `native-frames`
feature.

## Finding the innermost frame

Each isolate keeps a small record (`DebugState` in
`crates/vali-runtime/src/debug.rs`) holding the interpreter's `fp` and `pc`
and the id of its thread. The VM stores them where the interpreter calls
out of its loop for a Rust procedure, a raised error or a new stack
segment, not at every instruction, so a stop in a Rust procedure called
from Scheme code sees exactly where Scheme code was. The records form a list that the exported symbol
`VALI_DEBUG_STATES` heads, and the extensions pick the record of the
selected thread.

Where the build has full debug information for `vali-runtime`, the
extensions read the interpreter's `fp` and `pc` locals from DWARF instead,
which are exact even when stopped inside the interpreter's loop, and fall
back to the records when those are optimised out. Set
`VALI_DEBUGGER_NO_DWARF=1` to use the records only.

## What a build needs

| Host built with | `vali-bt`, `vali-frame` | `vali-print` and `Value` printing |
|---|---|---|
| Full debug information (`debug = true`) | Yes, from DWARF locals | Yes, `vali-print` takes any expression and `Value`s print as Scheme |
| Line tables only (`debug = "line-tables-only"`) | Yes, from the records | `vali-print` of raw words (`vali-print 0x17`) only; no types, so no `Value` printing and no expressions over locals |
| Symbols stripped, `VALI_DEBUG_STATES` kept | Yes, from the records | As for line tables only |
| Fully stripped | No: nothing locates the records | No |

A host that strips its symbols must keep `VALI_DEBUG_STATES`:
`strip --keep-symbol=VALI_DEBUG_STATES`, or the `-C link-arg` that exports
it from a shared library (`--export-dynamic-symbol=VALI_DEBUG_STATES`),
where a dynamic symbol survives `strip`. Everything else the extensions
read is in the process's memory: frames, function descriptors, line tables
and object layouts.

While stopped inside the interpreter's loop itself, or in native code that
is not a Rust procedure, an error or a stack segment (an allocation, say),
the records show where the interpreter last called out, which may be many
instructions or calls back, and so an older frame. With several isolates running on several
threads, the extensions show the one on the selected thread, or none.

`vali_debug.py` holds what both share. It reads the C-layout records
described in `crates/vali-runtime/src/debug.rs`, and checks their version.
