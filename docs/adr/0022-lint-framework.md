# 0022. Lint framework

- **Status:** Accepted, not built
- **Date:** 2026-11-19
- **Related:** 0001 (expansion output replayed from artifacts), 0009 (the `vali` command, grants), 0014 (lossless tree, spans), 0015 (the expander, syntax objects), 0021 (the first lint)

## Context

The compiler has no way to say anything but "this failed". `CompileError` holds a message, a span and a location, and compiling returns either a module or one error. The reader reports `ParseError`s with spans, but those are errors too. Nothing in the compiler, the loader or the CLI carries a warning.

ADR 0021 needs a warning: a library tested the platform while it was expanded. More will follow, such as unused imports, or a macro that shadows a standard name. Each should not invent its own naming, its own way to silence it and its own place to appear. The planned language server (`vali-ide`) will show the same findings in an editor.

ADR 0001 says output written during expansion is stored in the artifact and replayed when the artifact is reused, so that a macro's warnings do not vanish on a cache hit. That is not built, and it needs the same channel.

## Decision

### The framework

1. **A lint has a stable name,** a lower-case identifier such as `platform-features`. Names are part of the interface: they appear in output, in declarations and in configuration, and are not renamed.
2. **Each lint has a level: `allow`, `warn` or `deny`.** `allow` suppresses the diagnostic. `warn` reports it and compiling succeeds. `deny` reports it as an error, so the compile fails. Each lint has a default level, and the first lints default to `warn`.
3. **A library suppresses a lint with a declaration in `define-library`,** such as `(allow platform-features)`. The same form takes `warn` and `deny` to change the level for that library. A form-level wrapper for one site can be added later if a library needs finer control.
4. **Lints are diagnostics, not errors.** Compiling returns its diagnostics beside its result: each has the lint name, its level, a message, the spans it points at (one or several, so that a warning can name a macro's definition and its use) and an optional fix. Hosts receive them through the compiler's API and decide what to do. The CLI prints them to standard error in the form it prints errors in.
5. **Lints come out of every compile,** not only a separate run. A warning from an artifact that is reused is replayed from the artifact with the rest of its expansion output (ADR 0001 #4), so a cache hit reports what a cold build did. A `deny` lint fails the compile on a cache hit too.
6. **`vali lint` is a subcommand that compiles and reports lints without running anything.** It takes the same library options and grants as `vali run` (ADR 0009), since expansion can read through capabilities. It exits with 0 when there is no diagnostic at `warn` or above, 1 when there is a `deny` or a compile error, and 2 on a usage error. A flag sets a lint's level for one run.
7. **Configuration files and `vali-ide` integration come later.** The level of a lint in a project, and a language server's use of the fixes, build on items 1 to 6 and need no change to them.

### Rules

8. **The framework does not depend on how a rule is written.** A rule produces diagnostics: a lint name, spans, a message and an optional fix. Everything above treats them alike, wherever they came from.
9. **Built-in rules are written in Rust first.** A rule that is about what happens during expansion lives in the expander, because neither the syntax tree nor the expanded code shows it: the platform rule (ADR 0021) watches `cond-expand` resolution and `(features)` calls from phase-1 code, which only the expander sees.
10. **Rules over the lossless syntax tree** use Rowan (ADR 0014 #3). They see every byte, work on code that does not parse or expand, and give an editor the spans for quick fixes. They are added when the first syntactic or style lint, or `vali-ide`, needs one.
11. **Scheme-defined lints** (`define-lint`) come later, and the framework is designed to allow them:
    - a stable inspection API over syntax objects (ADR 0015) and expansion events, so a rule sees what a macro saw;
    - fuel and no capabilities: a lint cannot reach outside, and cannot loop (ADR 0009's limits apply);
    - shippable with a library, so a library's authors can lint its users' code against its own conventions.

    Their design waits for a few built-in rules, which show which events a rule needs.

## Consequences

- The compiler's API grows: compiling returns diagnostics. Every caller has to be updated, and hosts that ignore them lose nothing but the warnings.
- The expander gets an event channel. Built-in rules use it first; Scheme-defined ones would use it through a stable API, so what is exposed needs care.
- A lint's name is a promise. Choosing names takes some care before the first one ships.
- An artifact holds its diagnostics, so a cache hit can replay them.
- `vali lint` shares the compiler, so it cannot drift from what compiling reports.
- Rowan rules and expander rules see different things. A lint that needs both has to be split, or placed in the expander, which can also see source spans.

## Alternatives considered

- **Warnings as plain text on standard error:** hosts could not collect them, silence them or show them in an editor.
- **Rowan only:** rules over the lossless tree cannot see expansion, which is where the platform lint lives. Cond-expand resolution and phase-1 `(features)` calls leave no trace in the pre-expansion tree.
- **Scheme only** (`define-lint` from the start): the right interface for user-defined lints, but it cannot be designed well before built-in rules show which expansion events and syntax queries are needed, and the first rules are simpler and faster in Rust.
- **Suppression only in configuration files:** a library that is platform-specific on purpose would be unmarked in its own source, and a published artifact does not carry the project's configuration.
- **Per-form suppression first:** more precise, but it needs a wrapper form that the expander must recognise. A library-level declaration covers the first lint, and a wrapper can follow.
- **A separate `vali lint` only,** with compiling silent: a build would then pass without anyone seeing the warning, and the artifact's record would go unexplained.

## Left open

- The diagnostics type: its fields, how fixes are expressed, and whether errors move into it.
- The syntax of lint declarations beyond `(allow name)`: whether `warn` and `deny` take several names, and whether a declaration can name a group.
- Where a project's configuration lives and how it combines with declarations and flags.
- Whether `deny` in a library applies to the libraries it imports.
- How the rule API for Scheme-defined lints looks, and which expansion events it exposes.
- A form-level suppression wrapper.
- How `vali-ide` receives diagnostics and applies fixes.

## References

- Clippy and rustc's lint levels (`allow`, `warn`, `deny`), the model for names and levels.
