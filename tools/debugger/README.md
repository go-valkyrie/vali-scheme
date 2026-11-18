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
feature. They read the interpreter's `fp` and `pc` locals from debug
information, so the build must keep it: debug builds do, and a release
build needs `debug = true` (or `CARGO_PROFILE_RELEASE_DEBUG=true`).

`vali_debug.py` holds what both share. It reads the C-layout records
described in `crates/vali-runtime/src/debug.rs`, and checks their version.
