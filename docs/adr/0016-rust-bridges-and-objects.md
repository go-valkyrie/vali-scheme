# 0016. Rust bridges and objects

- **Status:** Accepted
- **Date:** 2026-11-17
- **Amended:** 2026-11-17, the builder takes owned parameters and a borrowed receiver; borrowed parameters come with the macro (items 4, 5 and 7)
- **Amended:** 2026-11-17, how a native library is compiled: as a generated library form (item 8)
- **Related:** 0002 (the Port and Rust-object heap types), 0003 (handles, freeing), 0004 (Rust never in the middle of a Scheme stack), 0007 #5 (bridges declare Rust types), 0008 (isolates are `Send`), 0009 (loaders, expansion-safe bridges)

## Context

Vali is embedded in Rust programs, so how Rust code is exposed to Scheme is most of what an embedder sees. Several records already assume a bridge layer without designing it:

- ADR 0007 #5: bridges declare ordinary Rust parameter types, converted by exactness, with an error naming the argument.
- ADR 0004 #10: a bridge returns a value, asks for a tail call, or returns a future; Rust is never in the middle of a Scheme stack, except in a nested run behind a continuation barrier.
- ADR 0009 #15 to #18: a bridge's author marks it expansion-safe, with a version, where it is defined.
- ADR 0003: bridged Rust objects hold handles, not Scheme values the collector must trace.
- ADR 0002: the heap reserves types for ports and Rust objects.

R7RS ports are the first need: a port is Rust state (a buffer, later a file or a socket) that Scheme passes around. Vali's own ports should use the same mechanism a host would, so the mechanism is proven by its first user.

What makes an embedding API pleasant, judging by mlua, pyo3, rquickjs and wasmtime: authors write ordinary Rust structs and functions, with parameters such as `&str`, `i64`, `&T` and `&mut T`, and results such as `T` or `Result<T, E>`; no interpreter lifetimes leak into their signatures; errors name the procedure and the argument; and a macro removes the registration boilerplate without hiding what it generates.

## The choices

### Where a Rust object lives

- **Boxed, in a per-isolate table.** The heap object is a header and an index into the isolate's table of `Box<dyn RustObject>`. Any `'static + Send` type works, trait objects included; a moving collector can move the heap object while the struct stays put; freeing the heap object drops the box. One indirection per access. This is mlua's choice for `UserData`.
- **Inline in the heap object**, with a pointer to the type's vtable: one load per access, but every Rust object must be pinned in a non-moving space, and only sized types fit. This is pyo3's choice for `#[pyclass]`.

### How functions are exposed

- **A typed builder**, `lib.function("read-char", read_char)`, where `read_char` is an ordinary Rust function whose parameters convert through traits.
- **An attribute macro**, `#[vali::bridge]`, generating the builder calls and carrying ADR 0009's marks.
- Both, the macro expanding to the builder.

### How aliasing is ruled out

A bridge taking `&mut T` must not see the same object through another reference. Without threads (ADR 0008), two cases remain: one call passing the same object for a `&mut` parameter and another parameter, as `(copy-port! p p)` could; and a nested run (ADR 0004 #10), where Rust waiting on a Scheme callback holds `&mut T` while the callback reaches the same object. Async bridges cannot conflict, since their futures are `'static` and hold no borrow across an `await`.

- **A borrow flag per table entry**, set as arguments convert and cleared when the call ends, by a guard, on return, error and panic alike. A conflict raises a Scheme condition. A load and a store per object argument. This is what pyo3, mlua and wasmtime do.
- **A per-call alias check**, comparing a call's object arguments, plus a rule that a nested run cannot start while a bridge holds an object borrow. No per-object state, but the rule is coarse: holding a `&mut` across a nested run always fails, even when the callback never reaches the object, so authors must re-borrow through `Obj<T>` after it.

### How bindings reach the expander

The expander needs a library's export names at compile time; the isolate needs the functions at run time.

- **Through the loader:** a loader serves a native library as it serves source, and the compiled program carries what was served to the isolate, so the two cannot drift.
- **A separate registry** handed to both the compiler and each isolate, which must be kept in step.

## Decision

1. **A Rust object is a boxed Rust value in its isolate's table.** Its type implements `RustObject` (`'static + Send`), which gives its Scheme name, and optionally how `write` and `display` show it; by default it prints as `#<name>`, never with an address (ADR 0001). The heap object holds the table index; its type is in the table entry, so `eq?` is identity and a type predicate is one comparison.
2. **Freeing the heap object drops the Rust value:** at an epoch under the counting strategies, when a cycle collection frees it, or when the isolate drops. `Drop` runs with no access to the isolate, so it cannot call Scheme. Resources with a lifetime that matters, such as files, are closed explicitly (`close-port`), and `Drop` is the backstop.
3. **A Rust object holds handles, not raw values (ADR 0003).** A handle is a root, so a cycle that passes through a Rust object is never collected; a type that must hold Scheme values in cycles uses weak handles, left open below.
4. **Conversions are two traits.** `FromScheme` turns an argument into an owned Rust type: the numeric types by ADR 0007 #5's table, `bool`, `char`, `String`, `Bytes` for a bytevector, `Obj<T>` (a typed handle to a Rust object, to keep it beyond the call), `Handle` (any value), `Vec<T>` from a proper list and `Vector<T>` from a vector; a parameter can also be `Option<T>`, an optional trailing argument, or `Rest<T>`, the remaining ones. A method's receiver, `&T` or `&mut T` for a Rust object type, is borrowed from the table for the call. A builder cannot accept other borrowed parameters, such as `&str`: inferring a function's parameter types does not work for a function generic over their lifetimes, and a guard type that could outlive the call would let safe code keep a borrow past its isolate. The macro, which writes each conversion out, accepts `&str`, `&[u8]`, `&T` and `&mut T` anywhere, borrowed for the call only. `IntoScheme` turns a result into a value: the same types the other way (`Vec<T>` as a list), `()` as the unspecified value, `Option<T>` with `None` as `#f`, any `RustObject` moved into the table, and `Result<T, E>` raising `E` as a condition. A failed conversion names the procedure, the argument's position and what it got, as `write` prints it.
5. **The builder is the API; the macro writes it.** `NativeLibrary::new(&["vali", "ports"]).method("read-char", Port::read_char).predicate::<Port>("port?")` builds a library; a function's arity follows from its signature. `#[vali::object]` on a struct and `#[vali::bridge]` on functions and `impl` blocks, in a `vali-macros` crate, generate those calls, map `read_char` to `read-char` unless a `name` is given, and carry ADR 0009's `expansion_safe` and `version` marks. Everything the macro does, the builder can do by hand.
6. **Bridges see the isolate through an object-safe context, not `Cx<S>`.** A native library must not depend on the memory strategy, since the compiler sees it through the loader without knowing which strategy will run it. Conversions reach the isolate through a `dyn` context trait that `Cx<S>` implements: one dynamic call at the boundary. Vali's core primitives stay monomorphic `NativeFn<S>`, as now, where calls are hot.
7. **Borrows are checked per object.** Each table entry has a borrow flag, set as a receiver or another `&T` or `&mut T` parameter converts and cleared when the call ends, however it ends; host code borrowing through `Cx::with` checks it too. A conflicting borrow raises "*procedure*: *name* is already borrowed", never undefined behaviour.
8. **Native libraries come through the loader.** A loader serves a library either as source or as a native library, `Arc<NativeLibrary>`. The compiler turns a native library into a generated library form that defines each function as `(%native-procedure identity name)` and exports it, so expansion, imports and linking treat it as any other library. The compiled program keeps the `Arc`, and loading it registers the library with the isolate, where `%native-procedure` finds exactly those functions, so the export list and the bindings cannot disagree. In the expander's isolate no native library is registered, so each function is a stub that fails when called (ADR 0009 #17), until expansion-safe marks exist.
9. **A panic in a bridge becomes a Scheme condition** naming the procedure (ADR 0007), and leaves the isolate usable; the borrow guards and the table stay consistent because both are released by `Drop`.
10. **Ports are Vali's first Rust objects.** One `Port` type, with an input or output direction, textual or binary, over a backend trait that string ports, bytevector ports and the host's output buffer implement now, and files and sockets later; a host can add backends. `port?` is a type check on `Port`.

11. **Async bridges** (`async fn`, ADR 0008 #8) come with the async integration, converting through the same traits.

## Consequences

- An embedder's API is ordinary Rust: plain structs, plain functions, and either a macro or a builder, with no Vali lifetimes in signatures.
- Every object access through a bridge costs a table lookup and a borrow-flag check: a few loads, small beside a call through the bridge boundary, but measurable for tiny methods. Hot procedures can still be written as monomorphic primitives inside Vali.
- The strategies gain a notice when they free an object that owns Rust state, which the cycle collector and the epoch both use.
- Hosts serve native libraries through the same loader trait as source, so import policies (ADR 0009 #20) cover them too.
- Rust objects that hold handles to each other in a cycle leak until their isolate drops.

## Alternatives considered

The per-call alias check was rejected for the borrow flag: the flag costs about the same, is local to the conversion code, and does not fail code that keeps a borrow across a nested run that never reaches the object. A separate registry for native libraries was rejected for the loader, so the export list and the bindings cannot disagree. Inline objects were rejected for boxed ones, which work for any type and need no pinned space.

## Left open

- The async bridge API, with the async integration.
- Weak handles, for Rust objects that must refer to Scheme values in cycles.

## References

- mlua, `UserData` and `UserDataRef`/`UserDataRefMut`: boxed values with per-object borrow checking.
- PyO3, `#[pyclass]` and `PyRef`/`PyRefMut`: inline objects with a borrow flag.
- rquickjs, `#[rquickjs::class]` and `#[rquickjs::methods]`: attribute macros over a class builder.
- wasmtime, `Linker::func_wrap`: typed host functions whose arity comes from their Rust signature.
