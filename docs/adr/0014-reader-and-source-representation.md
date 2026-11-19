# 0014. Reader and source representation

- **Status:** Accepted
- **Date:** 2026-11-16
- **Amended:** 2026-11-16, debug information is self-contained: usable without the source files
- **Amended:** 2026-11-16, lexer details: fold-case moves to the datum sink; strict atoms; Unicode identifiers by R6RS's categories with NFC; directives; shebang lines
- **Amended:** 2026-11-18, source locations as built: a table of sources per compilation, located compile errors, and `(vali syntax)` with Racket's names and counts (item 6); line tables in run-format bytecode, and backtraces with positions (item 9)
- **Amended:** 2026-11-18, reader hardening: the parser and the datum paths keep their own stacks, so nesting is limited by memory, not the native stack (item 2); `read` from a port reads one datum and no further, and keeps `#!fold-case` between reads; a label may not stand for nothing but itself (item 4)
- **Amended:** 2026-11-18, as built: backtraces show the frames of inlined procedures (item 9)
- **Amended:** 2026-11-18, as built: every pass over data a program can build keeps its own stack, and the passes over code either do or enforce a nesting limit that is a compile error (item 2)
- **Related:** 0001 (determinism, stable scope identities), 0005 (source spans per instruction), 0007 (syntax in scope), 0009 (source identities), 0015 (scopes and syntax objects)

## Context

The reader serves two very different users:

- **Running code** wants the fastest path from text to datums and syntax objects, because cold start is a goal.
- **Tools** (language servers, formatters, refactoring, debuggers) need a *lossless* tree that keeps every comment and space. They also need a tree even for broken code, and spans that survive macro expansion, so they can point at both the code as written and the macro that produced it.

rust-analyzer separates these concerns. Its parser contains no tree code: it emits events ("start node", "token", "finish node"), and a separate sink turns the events into a lossless Rowan tree. Logos generates fast lexers from token definitions. Both build on stable Rust and are dual-licensed MIT and Apache-2.0 (ADR 0011).

Scheme's syntax has a few features that a plain table of token patterns cannot handle:

- nested block comments (`#| … |#`);
- `#!fold-case` and `#!no-fold-case`, which change how identifiers read partway through a file;
- datum comments (`#;`), which comment out the next whole datum;
- datum labels (`#0=` and `#0#`), which build shared and circular structure.

## Decision

1. **One lexer, built with Logos.**
   - Tokens include trivia: whitespace and comments.
   - Nested block comments are lexed by a callback that counts nesting.
   - The lexer has no modes. `#!fold-case` and `#!no-fold-case` are `Directive` tokens; folding changes only what identifiers and character names mean, never where tokens start or end, so the datum sink applies it. The lossless tree is the same either way.
   - The recognised directives are `#!fold-case`, `#!no-fold-case` and `#!r6rs`, which is accepted and has no effect. Any other directive is an error.
   - A first line starting `#!/` or `#! ` is a shebang line, read as a comment, so scripts can be run directly.
   - An atom, a run of characters between delimiters, is a number if `vali-num` parses it, which owns R7RS's numeric grammar in full, including correctly rounded decimals. Otherwise it is an identifier if it follows R7RS's identifier grammar, and otherwise an error. Atoms such as `1+` or `12px` are errors, as R7RS requires, though many Schemes read them as symbols; `|1+|` writes that symbol. Accepting them later would break nothing, while rejecting them later would.
   - Identifiers may contain non-ASCII characters by R6RS's rule. They may start with characters in the Unicode general categories `Lu`, `Ll`, `Lt`, `Lm`, `Lo`, `Mn`, `Nl`, `No`, `Pd`, `Pc`, `Po`, `Sc`, `Sm`, `Sk`, `So` and `Co`, and continue with those plus `Nd`, `Mc` and `Me`. That keeps `λ`, `→` and `≤`, and excludes whitespace, control characters, invisible format characters (including the bidirectional controls used in "Trojan Source" attacks) and unassigned code points. Look-alike characters are left to tooling's warnings.
2. **One parser, which emits events and builds no tree.** Datum comments and datum labels are handled here, as structure. Scheme's grammar never needs a finished node wrapped after the fact, so events stream straight into the sink, with no list in between. `[` and `]` work like parentheses but must match their own kind, as in R6RS: `(a ]` is an error.

   **As built:** the parser never recurses. It keeps the lists, prefixes and datum comments it is inside on a stack of its own, so a million nested parentheses parse in constant native stack; whether a stray closer belongs to an enclosing bracket is a count of open brackets of each kind, not a scan. Everything downstream of it that walks a datum does the same: the datum sink, dropping a `Datum`, and building heap values from one. The Rowan sink builds green nodes itself, sharing equal tokens, because Rowan's builder caches nodes by a recursive hash; Rowan still drops green nodes recursively, so a tool holding an extremely deep tree should expect that. The lexer is lazy, so a reader that wants one datum lexes no further than its end.

   **Past the reader.** A stack overflow aborts the host, so no input may cause one. Passes over data that a program can build at any size are iterative: cloning, comparing, hashing, printing and dropping `Datum`s, `Syntax` objects and constants (in the IR, the run format and the heap), converting between heap values, datums and syntax, lowering a quoted datum to a constant, building constants in the heap, `write`, and dropping the compiler's expression tree. The passes over code (expansion and lowering of an expression) recurse, and grow their stack in segments with `stacker`, so the depth of the code is limited by memory. Macro uses are limited to 10,000 steps deep, and what compiles patterns and templates recurses on a pattern's or template's own nesting, so those nest at most 1,000 levels and import sets at most 100, beyond which they are compile errors. A pass that recurses on the program's nesting and meets neither guard is a bug.
3. **Parsing never stops at the first error.** On malformed input, such as unbalanced parentheses or an invalid token, the parser emits an error event and recovers, so a tool always gets a complete tree. Each error becomes a diagnostic with a span.
4. **Two sinks consume the events:**
   - The **Rowan sink** builds a lossless syntax tree that keeps every byte of the source. Tools use it.
   - The **datum sink** builds datums through a builder trait. One builder makes a plain tree, for tests and tools that want datums. The runtime supplies another that builds heap values and syntax objects directly. Running code never builds Rowan nodes. The datum sink reports every error, and produces nothing for a top-level form that contains one. It applies `#!fold-case`, and normalises identifiers to Unicode NFC, so the same name typed with precomposed or combining characters is the same symbol. It expands abbreviations to their R7RS forms, so `'x` reads as `(quote x)` and `#'x` as `(syntax x)`. Datum labels reach the builder as definitions and references, and each builder decides how to build the shared or circular structure.

   **As built:** `read_first` reads a source's first datum and reports where it ends and whether `#!fold-case` is in effect after it; a port's `read` uses it, so reading a port datum by datum takes linear time, `#!fold-case` carries on to later reads from the same port as R7RS 2.1 says, and an error leaves the port where it was. A label whose datum is nothing but a reference to a label still being defined, as in `#0=#0#` or `#0=#1=#0#`, stands for no datum and is an error. Labels are kept in hash sets, so a datum with many labels reads in linear time.
5. **Keywords follow SRFI 88.** An identifier ending in a colon, such as `foo:`, reads as a keyword. A lone `:` is a symbol, and so is `|foo:|`, so a symbol ending in a colon can still be written. `#:foo` is not keyword syntax in Vali.
6. **A span is a source identity plus a byte range.** The source identity is the library identity from ADR 0009 #2. Line and column are worked out on demand from a line index kept per source, so datums don't each store them. Positions are converted to whatever a consumer expects: a language server may need UTF-16 code units, depending on what the client negotiates.

   **As built:** a span's `SourceId` indexes a table of the sources a compilation read, from 1: the program, each library, and each included file, under its path as written. `SourceId(0)` is no source, as for data `eval` builds or `read` reads at run time. The expander keeps the table, so `eval` adds to it, and so do the syntax procedures, at expansion time and at run time. Compile errors print `source:line:column: message`, the column from 1, as compilers do. `(vali syntax)` reads a syntax object's location with Racket's names and counts: `syntax-source` (the identity, a string), `syntax-line` (from 1), `syntax-column` (from 0), `syntax-position` (from 1) and `syntax-span` (in characters), each `#f` with no source. `include-ci` puts its `#!fold-case` on the first line, so lines keep their numbers.
7. **Spans survive expansion.** Every syntax object carries its span. A syntax object a macro creates also records the macro use that produced it. Following that chain, errors, stack traces, debuggers and language servers can show both where code was written and which macro produced it. The chain is serialised in artifacts along with scope identities (ADR 0001).
8. **The expander can record binding facts for tools.** On request, it records:
   - for every identifier occurrence, the binding it refers to, with that binding's span and phase;
   - for every macro use, what it expanded into.

   This is what a language server needs for go-to-definition, find-references, renaming that respects hygiene, and stepping through a macro's expansion. DrRacket's Check Syntax works from the same kind of information. Running code never records it.
9. **Compiled code carries self-contained debug information.** Byte-range spans are used only while the source is at hand. Compiling into anything that can outlive its source (run-format bytecode, distribution artifacts, standalone executables) resolves spans into debug information that needs no source file:
   - source identity, line and column (counted in Unicode scalar values), plus the byte offset for tools that do have the source;
   - function names, and a line table mapping instructions to positions, compressed as DWARF line programs and the JVM's `LineNumberTable` are;
   - local variable names for each frame slot, with the instructions where each is live (ADR 0005);
   - macro-origin chains, resolved the same way, so a stack trace can name the macro that produced the code and where it was used.

   **As built in tier 0:** each run-format function has a table of positions, sorted by instruction, each a source in the module's table of identities, a line and a column; a position covers instructions up to the next. Codegen notes one at each call, inlined ones included, and the enclosing call's again once a nested call is done. A backtrace frame is the procedure's name and the position of its failing instruction or waiting call: `f (source:3:5)`. The table is plain for now; compressing it, local variable names and macro-origin chains come later.

   **As built, with inlining:** a procedure inlined into another leaves its frame in backtraces. The IR's positions name the inlined copy they are in, and each function lists its copies, each with the procedure's name and the position of the call it replaced, which may be in an earlier copy. Run format keeps these as a table of copies and a table of scopes beside the positions, so the debuggers' layout of positions is unchanged. A frame stopped in a copy shows as one frame per copy, innermost first, then the function's own: `f (source:2:3)`, `g (source:4:8)`, `main (source:5:7)`, as if nothing had been inlined. Loops made by contification have no frame of their own, as before.

   Error messages, stack traces and debuggers read only this information, never the source. Debug information can be stripped, or shipped as a separate file matched to its artifact by hash, as .NET's PDB files and Apple's dSYM bundles are. It can optionally embed the source text itself, so a debugger can show code that no one has a copy of.
10. **Top-level forms reparse independently.** A tool can reparse only the top-level form containing an edit and splice the result into the Rowan tree.

## Consequences

- There is one grammar and one set of parser bugs. A language server and the runtime cannot disagree about what a file says.
- On the path to running code, reading costs a lexer, a parser and building datums, with no tree in between.
- Tools always get a tree, including for code that doesn't parse yet, with diagnostics attached.
- An identifier ending in a colon is a keyword everywhere. Code from other Schemes that uses such symbols must write them as `|foo:|`.
- Spans are byte ranges while parsing, so converting to lines and columns needs the source text or its line index. Compiled code never needs it: its positions are resolved when it is compiled (item 9).
- Debug information makes artifacts larger. That is why it can be stripped or shipped separately.
- Recording binding facts and macro origins costs time and memory; only tools ask for binding facts. Macro origins are always kept, because error messages need them too.

## Alternatives considered

- **Building a Rowan tree on every read, and deriving datums from it:** one path, but every cold load allocates a full lossless tree only to throw it away.
- **Two separate readers, one fast and one lossless:** what the design overview first described. Two grammars drift apart, and tools then disagree with the runtime about edge cases.
- **Racket's `#:foo` keyword syntax:** unambiguous, and `foo:` stays a symbol. SRFI 88's syntax was chosen instead, for portability with Gambit and CHICKEN, which use it.

## Left open

- **The debugger:** its protocol, probably the Debug Adapter Protocol, built on the debug information in item 9.
- The language server's crate design and the analysis it runs on top of binding facts.
- The formatter's style.

## References

- The rust-analyzer architecture documentation: the parser, events and syntax trees.
- The Logos and Rowan crates.
- R7RS-small, sections 2.1 to 2.4 (lexical conventions) and 7.1.1 (lexical structure).
- SRFI 88, *Keyword objects*.
- The Racket documentation: Check Syntax, and the `origin` syntax property.
- The Language Server Protocol specification: position encodings.
