# Run-format bytecode

The instruction set the VM interprets and the JIT compiles from (ADR 0005). This is a specification, not an ADR: it changes with the format version, and there is no compatibility promise between versions. Any mismatch means rebuilding from source or from the distribution artifact. The `vali-bytecode` crate implements it.

**Format version:** 1.

## Modules and functions

A **module** is what one library's body compiles to:

- a **constant pool**: integers of any size, flonums, exact rationals (a ratio of two integer constants in lowest terms), complex numbers (a pair of real constants, both exact or both flonums), booleans, characters, the empty list, the unspecified value, SRFI 88 keywords, and immutable strings, symbols, pairs, vectors and bytevectors of constants. Heap constants are created in each isolate when the module loads there. A malformed numeric constant fails the load;
- a **variable table**: the module's variables. Code refers to them by index. Each entry has a name and a link, which says where its cell is once the module loads (ADR 0005 #4):
  - **defined**: the module defines it, and its instance has the cell;
  - **imported** from a library, by the library's identity and the name it exports: the cell is that library's instance's. An importer cannot assign it;
  - **global**: the isolate's global of that name, before or after the module loads: a REPL's variable, or one a host defines for bytecode it builds itself;
  - **primitive**: Vali's procedure implemented in Rust of that name, which `(vali primitive)` exports. An importer cannot assign it;
  - **native**, by a native library's identity: that library's function of the variable's name, as a procedure, linked as the module loads (ADR 0016 #8). An importer cannot assign it;
- an **export table**: the names other modules import variables by, each naming an entry of the variable table;
- a **function table**;
- a **source table**: the identities of the sources its functions' positions are in;
- the index of its **body**, a function of no arguments and no free variables.

A **function** has a name, an arity, a frame size in slots, a count of free variables, its code, and its positions. An **arity** is a number of required arguments, a number of optional ones, and whether the rest are collected into a list. A function's **positions** say where its instructions came from (ADR 0014 #9): each names an instruction, a source in the module's source table, a line from 1 and a column from 0, counting Unicode scalar values, and covers its instruction and those after it, up to the next. The compiler notes one at each call it compiles, inlined ones included, and the call's own again after its arguments, so a failing instruction, and each frame's waiting call, has the position of the call it belongs to. The validator checks that positions are in order, at instructions, and in the source table.

A function also has a table of **inlined copies** and a table of **scopes**, so that a backtrace still shows the frames of procedures inlined into it. Each copy names the procedure copied, where it was called (a source, line and column), and the copy that call was in: 0 for the function's own code, `i + 1` for copy `i`, which must come before it. Each scope names an instruction and a copy, 0 for the function's own code, and covers its instruction and those after it, up to the next, as positions do. A frame stopped in a copy shows as one frame per copy, innermost first, each at the position of the call the next one in replaced. The debugger extensions read the positions alone and do not show inlined frames yet. The validator checks that scopes are in order, at instructions, and name copies that exist, and that each copy's caller comes before it.

## Frames

Frames live on a task's stack segments, which grow downward like native stacks (ADR 0004). Each frame starts with a three-word header: a native frame record, meaning the saved frame pointer and the return address where the platform's ABI puts them, and above it the caller's bytecode position, which unwinders ignore. Below the header are the frame's slots, which are the VM's registers:

- slot 0 holds the running closure;
- the arguments follow, in slots 1 and up: required, then optional (the default object when omitted), then the rest list if there is one;
- the remaining slots are locals and temporaries.

**Calls** put the callee's frame inside the caller's. `call base, argc` uses slots `base` to `base + 2` for the callee's frame header, `base + 3` for the procedure, and the next `argc` slots for the arguments, which become the callee's slots 0 to `argc` without being copied. The result arrives in slot `base + 3`. The compiler must leave those slots free, and the validator checks that they lie inside the frame.

**Tail calls** move the procedure and arguments to slots 0 and up, and reuse the frame.

**Direct calls** call one of the module's own functions that has no free variables, known when the code was compiled. `call-direct base, argc, f` lays out the callee's frame as `call` does, but the VM puts the procedure in slot `base + 3` itself: a closure of `f` that the module's instance makes the first time a direct call needs it and keeps from then on. There is no procedure check and no arity check: the validator has checked that `f` has no free variables and takes `argc` arguments. `tail-call-direct proc, argc, f` takes the arguments from the slots after `proc`, and is to `tail-call` what `call-direct` is to `call`.

**Function entry** is not an instruction. The VM checks the callee's arity, fills in omitted optional arguments, builds the rest list, decrements fuel and counts the call, as ADR 0005 #6 describes.

## Encoding

Instructions are little-endian 32-bit words. The first word holds the opcode in its low 8 bits and up to 24 bits of operands in one of three layouts:

| Layout | Bits 8–15 | Bits 16–23 | Bits 24–31 |
|---|---|---|---|
| `a b c` | `a` | `b` | `c` |
| `a d` | `a` | `d`, 16 bits | |
| `e` | `e`, 24 bits | | |

- **Conditional branches** take a second word: a signed 32-bit offset.
- **`jump`** keeps a signed 24-bit offset in `e`.
- **Offsets** count words from the start of the next instruction.
- **`closure`** takes a second word, the function's index, then one word for every four free variables, each a slot number in one byte, low byte first.
- **`call-direct`** and **`tail-call-direct`** take a second word, the function's index.

- **`wide`** (opcode 45) is a prefix, not an instruction: its `a`, `b` and `c` fields are the high bytes of the next instruction's, which make slots, argument counts and free-variable indices 16 bits. A wide `closure` lists each slot in two bytes, two to a word. The prefix appears only when a field needs it, and never on an immediate, a `d` field or another prefix: every instruction has one encoding, and the decoder refuses any other. A branch's offset counts from the end of the instruction, prefix included; branches target the prefix.

Slots up to 255 fit an instruction's fields; with `wide`, frames have up to 65,535 slots and calls up to 65,535 arguments. Format version 3 added `wide`, and version 4 the direct calls.

## Instructions

Slots are written `rN`, constants `kN`, variables `vN` and functions `fN`. Every instruction checks the types and bounds of its operands at run time, and raises an error on a mismatch. Arithmetic and comparison instructions take a fast path for fixnums and fall back to generic arithmetic.

### Moving values

| Opcode | Mnemonic | Layout | Meaning |
|---|---|---|---|
| 1 | `mov dst, src` | `a b` | `dst ← src` |
| 2 | `load-constant dst, k` | `a d` | `dst ←` constant `k` |
| 3 | `load-int dst, n` | `a d` | `dst ←` the fixnum `n`, a signed 16-bit value |
| 4 | `get-variable dst, v` | `a d` | `dst ←` variable `v`; an error if it is unbound |
| 5 | `set-variable v, src` | `a d` | variable `v ← src` |

### Closures and boxes

| Opcode | Mnemonic | Layout | Meaning |
|---|---|---|---|
| 6 | `closure dst, f, r…` | `a b`, then words | `dst ←` a closure of function `f`, capturing the listed slots; `b` is how many |
| 7 | `free dst, i` | `a b` | `dst ←` free variable `i` of the running closure |
| 8 | `box dst, src` | `a b` | `dst ←` a new box holding `src` |
| 9 | `unbox dst, src` | `a b` | `dst ←` the contents of the box in `src` |
| 10 | `set-box cell, src` | `a b` | the box in `cell` now holds `src` |

Boxes hold assigned variables that closures capture (ADR 0005 #4).

### Control

Conditional branches continue to the next instruction when their condition holds, and jump otherwise.

| Opcode | Mnemonic | Layout | Meaning |
|---|---|---|---|
| 11 | `jump offset` | `e` | Jump |
| 12 | `if r, offset` | `a`, offset | Continue if `r` is not `#f` |
| 13 | `if-eq a, b, offset` | `a b`, offset | Continue if `(eq? a b)` |
| 14 | `if-lt a, b, offset` | `a b`, offset | Continue if `(< a b)` |
| 15 | `if-le a, b, offset` | `a b`, offset | Continue if `(<= a b)` |
| 16 | `if-num-eq a, b, offset` | `a b`, offset | Continue if `(= a b)` |
| 17 | `if-lt-imm a, n, offset` | `a b`, offset | Continue if `(< a n)`, `n` a signed 8-bit fixnum |
| 18 | `if-null r, offset` | `a`, offset | Continue if `r` is the empty list |
| 19 | `if-pair r, offset` | `a`, offset | Continue if `r` is a pair |
| 20 | `call base, argc` | `a b` | Call, as described under Frames |
| 21 | `tail-call proc, argc` | `a b` | Tail call the procedure in `proc` with the `argc` arguments after it |
| 22 | `return r` | `a` | Return `r` |
| 46 | `call-direct base, argc, f` | `a b`, then the function | Call function `f` directly, as described under Frames |
| 47 | `tail-call-direct proc, argc, f` | `a b`, then the function | Tail call function `f` directly with the `argc` arguments after `proc` |

### Arithmetic and comparison

| Opcode | Mnemonic | Layout | Meaning |
|---|---|---|---|
| 23 | `add dst, a, b` | `a b c` | `dst ← (+ a b)` |
| 24 | `sub dst, a, b` | `a b c` | `dst ← (- a b)` |
| 25 | `mul dst, a, b` | `a b c` | `dst ← (* a b)` |
| 26 | `add-imm dst, a, n` | `a b c` | `dst ← (+ a n)`, `n` a signed 8-bit fixnum |
| 27 | `sub-imm dst, a, n` | `a b c` | `dst ← (- a n)` |
| 28 | `lt dst, a, b` | `a b c` | `dst ← (< a b)` |
| 29 | `le dst, a, b` | `a b c` | `dst ← (<= a b)` |
| 30 | `num-eq dst, a, b` | `a b c` | `dst ← (= a b)` |
| 31 | `eq dst, a, b` | `a b c` | `dst ← (eq? a b)` |
| 32 | `not dst, src` | `a b` | `dst ← (not src)` |

### Pairs and vectors

| Opcode | Mnemonic | Layout | Meaning |
|---|---|---|---|
| 33 | `cons dst, a, b` | `a b c` | `dst ← (cons a b)` |
| 34 | `car dst, pair` | `a b` | `dst ← (car pair)` |
| 35 | `cdr dst, pair` | `a b` | `dst ← (cdr pair)` |
| 36 | `set-car pair, value` | `a b` | `(set-car! pair value)` |
| 37 | `set-cdr pair, value` | `a b` | `(set-cdr! pair value)` |
| 38 | `null? dst, src` | `a b` | `dst ← (null? src)` |
| 39 | `pair? dst, src` | `a b` | `dst ← (pair? src)` |
| 40 | `make-vector dst, len, fill` | `a b c` | `dst ← (make-vector len fill)` |
| 41 | `vector-ref dst, v, i` | `a b c` | `dst ← (vector-ref v i)` |
| 42 | `vector-set v, i, x` | `a b c` | `(vector-set! v i x)` |
| 43 | `vector-length dst, v` | `a b` | `dst ← (vector-length v)` |

### Closures, continued

| Opcode | Mnemonic | Layout | Meaning |
|---|---|---|---|
| 44 | `set-free closure, i, src` | `a b c` | Free variable `i` of the closure in `closure` now holds `src`. `letrec` uses it to close the references between closures that refer to each other. The VM checks `i` against the closure's free-variable count |

Everything else is a call to a procedure: primitives are procedures implemented in Rust, bound to variables like any other.

## Validation

The loader rejects a module unless:

- its body exists, and takes no arguments and no free variables;
- every function has at most 256 slots, and enough for slot 0 and its parameters;
- every function's code decodes, and its last instruction is `jump`, `tail-call`, `tail-call-direct` or `return`, so execution cannot run off the end;
- every slot operand lies inside the frame, including the header, procedure and arguments of every `call` and `call-direct`;
- every constant, variable, function and free-variable index exists, and every `closure` captures as many slots as its function has free variables;
- every direct call's function has no free variables and takes the call's argument count;
- no `set-variable` assigns an imported variable, and no name is exported twice;
- every branch lands on the start of an instruction.

A module that passes can be run without further checks in the dispatch loop (ADR 0005 #7).
