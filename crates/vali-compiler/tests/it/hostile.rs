//! Hostile artifacts: bytes that decode but name what is not there, count
//! what they do not hold, or nest without end must be refused with an error
//! (ADR 0017 #13, ADR 0014). Nothing here may panic, overflow the stack or
//! reserve memory in proportion to anything but the artifact's size.
//!
//! The artifacts are made by compiling a small library and patching one
//! field of its sections, found by a walker that mirrors the decoders. The
//! cases run on a test thread's stack, which is smaller than a main
//! thread's.

#![allow(dead_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use vali_compiler::{
    Artifacts, Identity, LibraryName, LoadError, Loader, Program, Source, compile_program_with,
};
use vali_dist::{Artifact, DecodeError, Reader, Writer};
use vali_runtime::{Cx, HeapConfig, NoCollection};

const SECTIONS: &[&str] = &["build", "source", "library", "syntax", "invoke", "depends"];

/// Serves libraries from a list of names and texts.
struct Map(Vec<(&'static str, String)>);

impl Loader for Map {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        let wanted = name.to_string();
        Ok(self
            .0
            .iter()
            .find(|(n, _)| *n == wanted)
            .map(|(n, text)| Source::scheme(Identity::new(&format!("lib:{n}")), text)))
    }
}

/// `(test greet)`, with a procedure, a `syntax-rules` macro, and a
/// `syntax-case` macro; `(test evil)`, whose export the tests replace.
fn libraries() -> Map {
    Map(vec![
        (
            "(test greet)",
            "(define-library (test greet)
               (import (scheme base))
               (export greet twice count-args)
               (begin
                 (define (helper x) (string-append \"hi, \" x))
                 (define (greet x) (helper x))
                 (define-syntax twice
                   (syntax-rules () ((_ e) (begin e e))))
                 (define-syntax count-args
                   (lambda (stx)
                     (syntax-case stx ()
                       ((_ a ...) (datum->syntax stx (length (syntax->datum #'(a ...))))))))))"
                .to_owned(),
        ),
        (
            "(test evil)",
            "(define-library (test evil) (import (scheme base))
               (export dummy) (begin (define dummy 1)))"
                .to_owned(),
        ),
    ])
}

const PROGRAM: &str = "
    (import (scheme base) (test greet))
    (define n 0)
    (twice (set! n (+ n 1)))
    (list (greet \"you\") n (count-args 1 2 3) (let ((helper 5)) helper))";

const GREET: &str = "lib:(test greet)";
const EVIL: &str = "lib:(test evil)";

type Set = Vec<(String, Vec<u8>)>;

fn compile(program: &str, artifacts: &Artifacts) -> Result<Program, String> {
    compile_program_with(program, &Identity::new("main"), &libraries(), artifacts)
        .map_err(|e| e.message)
}

/// The artifacts of every library `program` uses.
fn written(program: &str) -> Set {
    let artifacts = Artifacts {
        write: true,
        ..Artifacts::default()
    };
    compile(program, &artifacts)
        .unwrap()
        .artifacts
        .into_iter()
        .map(|(i, b)| (i.as_str().to_owned(), b))
        .collect()
}

fn reading(set: &Set) -> Artifacts {
    Artifacts {
        read: set.iter().cloned().collect(),
        write: false,
    }
}

fn find<'a>(set: &'a Set, identity: &str) -> &'a [u8] {
    &set.iter().find(|(i, _)| i == identity).unwrap().1
}

fn with(set: &Set, identity: &str, bytes: Vec<u8>) -> Set {
    let mut set: Set = set.iter().filter(|(i, _)| i != identity).cloned().collect();
    set.push((identity.to_owned(), bytes));
    set
}

fn section(bytes: &[u8], name: &str) -> Vec<u8> {
    Artifact::decode(bytes, SECTIONS)
        .unwrap()
        .section(name)
        .unwrap()
        .to_vec()
}

fn replace_section(bytes: &[u8], name: &str, new: Vec<u8>) -> Vec<u8> {
    let mut a = Artifact::decode(bytes, SECTIONS).unwrap();
    let at = a.sections.iter().position(|s| &*s.name == name).unwrap();
    a.sections[at].bytes = new;
    a.encode()
}

fn uint(n: u64) -> Vec<u8> {
    let mut w = Writer::new();
    w.uint(n);
    w.finish()
}

fn splice(bytes: &[u8], at: usize, len: usize, new: &[u8]) -> Vec<u8> {
    let mut out = bytes[..at].to_vec();
    out.extend_from_slice(new);
    out.extend_from_slice(&bytes[at + len..]);
    out
}

/// What compiling and running `program` against `set` came to, as text.
/// It must not panic: a panic fails the test, naming `case`.
#[track_caller]
fn attempt(case: &str, program: &str, set: &Set) -> String {
    let start = Instant::now();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let program = match compile(program, &reading(set)) {
            Ok(program) => program,
            Err(e) => return format!("refused: {e}"),
        };
        let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        let id = match vali_compiler::load(&mut cx, program) {
            Ok(id) => id,
            Err(e) => return format!("refused: {e}"),
        };
        match cx.run(id) {
            Ok(v) => format!("ran: {}", cx.write_string(&v).unwrap()),
            Err(e) => format!("failed: {e}"),
        }
    }));
    let took = start.elapsed();
    let outcome = outcome.unwrap_or_else(|_| panic!("{case}: panicked"));
    eprintln!("{case}: {took:?}: {}", outcome.lines().next().unwrap_or(""));
    assert!(took.as_secs() < 10, "{case}: took {took:?}");
    outcome
}

/// greet's artifact with its library record's interface replaced by
/// `count` entries, `entries` in all.
fn greet_interface(set: &Set, count: u64, entries: &[u8]) -> Set {
    let bytes = find(set, GREET);
    let library = section(bytes, "library");
    let fields = walk_library(&library);
    let at = fields
        .iter()
        .find(|f| f.what == "interface.count")
        .unwrap()
        .at;
    let mut new = library[..at].to_vec();
    new.extend(uint(count));
    new.extend_from_slice(entries);
    with(set, GREET, replace_section(bytes, "library", new))
}

/// greet's artifact with the syntax-section field `pick` finds replaced.
fn greet_syntax_patch(set: &Set, pick: impl Fn(&Field) -> bool, new: &[u8]) -> Set {
    let bytes = find(set, GREET);
    let syntax = section(bytes, "syntax");
    let fields = walk_syntax(&syntax);
    let f = fields.iter().find(|f| pick(f)).expect("the field is there");
    let patched = splice(&syntax, f.at, f.len, new);
    with(set, GREET, replace_section(bytes, "syntax", patched))
}

/// The syntax export of `(test evil)`, written by hand: one library scope
/// and one set of it, then whatever `body` writes for the macros, compiled
/// syntax and bindings.
fn evil_export(body: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut w = Writer::new();
    w.uint(1); // scopes
    w.byte(3); // a library scope
    w.uint(1); // sets
    w.uint(1); // of one scope
    w.uint(0); // scope 0
    w.uint(0); // the core forms' scope
    w.uint(0); // and set
    w.uint(0); // the next macro
    w.uint(0); // imported units
    body(&mut w);
    w.finish()
}

fn no_span(w: &mut Writer) {
    w.uint(0);
    w.uint(0);
    w.uint(0);
}

/// A string atom with no scopes, span or origin.
fn string_syntax(w: &mut Writer, s: &str) {
    w.byte(3);
    w.byte(3);
    w.str(s);
    w.uint(0);
    w.uint(0);
    no_span(w);
    w.uint(0);
    w.uint(0);
}

/// `(test evil)`'s artifacts with its syntax export replaced.
fn evil_set(export: Vec<u8>) -> Set {
    let set = written("(import (scheme base) (test evil)) dummy");
    let evil = replace_section(find(&set, EVIL), "syntax", export);
    with(&set, EVIL, evil)
}

const USES_EVIL: &str = "(import (scheme base) (test evil)) 1";

#[test]
fn an_interface_naming_a_macro_that_is_not_there_is_refused() {
    let set = written(PROGRAM);
    let mut w = Writer::new();
    w.str("twice");
    w.byte(3); // a macro
    w.uint(0); // of the unit named next
    w.str(GREET);
    w.uint(99); // number 99
    let set = greet_interface(&set, 1, &w.finish());
    let r = attempt("missing macro", "(import (test greet)) (twice 1)", &set);
    assert!(r.starts_with("refused"), "{r}");
}

#[test]
fn a_binding_naming_a_macro_that_is_not_there_is_refused() {
    // The same through the syntax export's binding table: `twice` bound to
    // macro 99 of (test greet).
    let set = written(PROGRAM);
    let bytes = find(&set, GREET);
    let syntax = section(bytes, "syntax");
    let fields = walk_syntax(&syntax);
    let at = fields
        .iter()
        .position(|f| f.what == "binding.tag" && f.value == 3)
        .expect("a macro binding");
    let index = fields[at..]
        .iter()
        .find(|f| f.what == "macro.index")
        .expect("its number");
    let patched = splice(&syntax, index.at, index.len, &uint(99));
    let set = with(&set, GREET, replace_section(bytes, "syntax", patched));
    let r = attempt("binding to a missing macro", PROGRAM, &set);
    assert!(r.starts_with("refused"), "{r}");
}

#[test]
fn a_repeat_whose_pattern_does_not_bind_its_variable_is_refused() {
    let set = written(PROGRAM);
    let bytes = find(&set, GREET);
    let syntax = section(bytes, "syntax");
    let fields = walk_syntax(&syntax);
    // count-args' compiled pattern `(_ a ...)`: the repeated item `a`
    // becomes `_`, which binds nothing, though the repeat names variable 0.
    let tag = fields
        .iter()
        .position(|f| {
            f.what == "pattern.tag"
                && f.ctx.contains("cpattern")
                && f.ctx.ends_with("each/pattern")
                && f.value == 1
        })
        .expect("count-args' (a ...)");
    let (t, v) = (&fields[tag], &fields[tag + 1]);
    let patched = splice(&syntax, t.at, v.at + v.len - t.at, &[0]);
    let compiled = with(&set, GREET, replace_section(bytes, "syntax", patched));
    let r = attempt("compiled repeat binds nothing", PROGRAM, &compiled);
    assert!(r.starts_with("refused"), "{r}");

    // And in a `syntax-rules` clause `(_ e ...)` where `e` is `_`.
    let mut w = Writer::new();
    w.byte(4); // a list
    w.uint(1); // before the repeat: one pattern
    w.byte(0); // `_`
    w.byte(1); // a repeat
    w.byte(0); // of `_`
    w.uint(1); // naming one variable
    w.uint(0); // variable 0
    w.uint(0); // nothing after
    w.byte(0); // no tail
    let clause = fields
        .iter()
        .find(|f| f.what == "pattern(whole)" && f.ctx == "macro/clause")
        .unwrap();
    let patched = splice(&syntax, clause.at, clause.len, &w.finish());
    // twice's depths are [0]: make them [1] so the clause still decodes.
    let fields = walk_syntax(&patched);
    let d = fields
        .iter()
        .find(|f| f.what == "depths(whole)" && f.ctx == "macro/clause")
        .unwrap();
    let patched = splice(&patched, d.at, d.len, &[1, 1]);
    let rules = with(&set, GREET, replace_section(bytes, "syntax", patched));
    let r = attempt(
        "syntax-rules repeat binds nothing",
        "(import (scheme base) (test greet)) (twice 1 2)",
        &rules,
    );
    assert!(r.starts_with("refused"), "{r}");
}

#[test]
fn a_compiled_pattern_that_counts_more_variables_than_it_binds_is_refused() {
    let set = written(PROGRAM);
    // 2 is one too many; 2^34 would reserve 962 GB.
    for vars in [2u64, 1 << 34, 1 << 61] {
        let hostile = greet_syntax_patch(&set, |f| f.what == "cpattern.vars", &uint(vars));
        let r = attempt(&format!("cpattern.vars = {vars}"), PROGRAM, &hostile);
        assert!(r.starts_with("refused"), "{r}");
    }
}

#[test]
fn a_template_with_hostile_counts_is_refused() {
    let set = written(PROGRAM);
    // count-args' template (a ...): the item's ellipsis count, and a depth.
    for n in [2u64, 1 << 40, u64::MAX] {
        let hostile = greet_syntax_patch(
            &set,
            |f| f.what == "item.ellipses" && f.ctx.contains("ctemplate"),
            &uint(n),
        );
        let r = attempt(&format!("item.ellipses = {n}"), PROGRAM, &hostile);
        assert!(r.starts_with("refused"), "{r}");
        if n > 2 {
            assert!(r.contains("an ellipsis count of"), "{r}");
        }
    }
    let hostile = greet_syntax_patch(
        &set,
        |f| f.what == "depth" && f.ctx.contains("ctemplate"),
        &uint(u64::MAX),
    );
    let r = attempt("template depth = MAX", PROGRAM, &hostile);
    // Refused as a depth, not for what it would have done: a depth drives
    // recursion when a match is turned back into syntax, and 2^64 deep
    // aborted the process.
    assert!(r.contains("a depth of"), "{r}");
    // The same in a `syntax-rules` clause's depths.
    let hostile = greet_syntax_patch(
        &set,
        |f| f.what == "depth" && f.ctx == "macro/clause",
        &uint(1 << 40),
    );
    let r = attempt("clause depth", PROGRAM, &hostile);
    assert!(r.contains("a depth of"), "{r}");
}

#[test]
fn a_procedure_transformers_phase_is_bounded() {
    let set = written(PROGRAM);
    // One isolate is made for each phase level, so phase 2^40 ran 15 s to
    // exhaust memory.
    for phase in [64u64, 512, 1 << 40] {
        let hostile = greet_syntax_patch(&set, |f| f.what == "macro.phase", &uint(phase));
        let r = attempt(&format!("macro.phase = {phase}"), PROGRAM, &hostile);
        assert!(r.starts_with("refused"), "{r}");
    }
    // A phase the language has still works.
    let r = attempt("macro.phase = 0", PROGRAM, &set);
    assert!(r.starts_with("ran"), "{r}");
}

#[test]
fn many_sources_in_an_export_load_in_linear_time() {
    // Each source took a scan of those before it: 16,000 took 2.2 s.
    let n = 40_000u64;
    let export = evil_export(|w| {
        w.uint(0); // macros
        w.uint(1); // compiled syntax
        w.str("k");
        w.uint(n); // quoted syntax objects
        for i in 0..n {
            w.byte(3); // an atom
            w.byte(1); // a boolean
            w.byte(0);
            w.uint(0); // no scope set
            w.uint(0); // no pending changes
            w.uint(1); // a source, in full
            w.str(&format!("s{i}"));
            w.str("");
            w.uint(0);
            w.uint(0);
            w.uint(0); // no origin
            w.uint(0);
        }
        w.uint(0); // no patterns
        w.uint(0); // no templates
        w.uint(0); // bindings
    });
    attempt("many sources", USES_EVIL, &evil_set(export));
}

/// A pattern nested `depth` lists deep, as the bytes of the clause pattern,
/// with variable 0 at the bottom.
fn deep_pattern(depth: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(depth * 4 + 2);
    for _ in 0..depth {
        v.extend([4, 1]); // a list of one pattern before its end
    }
    v.extend([1, 0]); // variable 0
    for _ in 0..depth {
        v.extend([0, 0]); // no repeat, no tail
    }
    v
}

/// A quoted syntax object nested `depth` lists deep.
fn deep_syntax(depth: usize) -> Vec<u8> {
    let mut w = Writer::new();
    for _ in 0..depth {
        w.byte(1); // a list
        w.uint(1); // of one item
    }
    string_syntax(&mut w, "x");
    for _ in 0..depth {
        w.byte(0); // no tail
        w.uint(0); // no scope set
        w.uint(0); // no pending changes
        no_span(&mut w);
        w.uint(0);
        w.uint(0);
    }
    w.finish()
}

/// How deep a hostile pattern or syntax object nests: 200,000 deep is
/// about 1 MB of input, and a test thread's stack is 2 MB.
const DEEP: usize = 200_000;

#[test]
fn a_deeply_nested_pattern_is_not_a_stack_overflow() {
    let set = written(PROGRAM);
    let bytes = find(&set, GREET);
    let syntax = section(bytes, "syntax");
    let fields = walk_syntax(&syntax);
    let p = fields
        .iter()
        .find(|f| f.what == "pattern(whole)" && f.ctx == "macro/clause")
        .unwrap();
    // The clause's template and depths no longer fit it, so it may be
    // refused: but not by crashing, and not on dropping what was decoded.
    let mut patched = splice(&syntax, p.at, p.len, &deep_pattern(DEEP));
    let hostile = with(
        &set,
        GREET,
        replace_section(bytes, "syntax", patched.clone()),
    );
    attempt("a deep pattern", USES_GREET, &hostile);
    // Decoding fails after it.
    patched.push(0xFF);
    let hostile = with(&set, GREET, replace_section(bytes, "syntax", patched));
    attempt("a deep pattern, then an error", USES_GREET, &hostile);
}

const USES_GREET: &str = "(import (scheme base) (test greet)) 1";

/// `(test greet)` with `twice`'s clause pattern nested `depth` lists deep.
fn greet_with_nested_pattern(depth: usize) -> Set {
    let set = written(PROGRAM);
    let bytes = find(&set, GREET);
    let syntax = section(bytes, "syntax");
    let fields = walk_syntax(&syntax);
    let p = fields
        .iter()
        .find(|f| f.what == "pattern(whole)" && f.ctx == "macro/clause")
        .unwrap();
    let patched = splice(&syntax, p.at, p.len, &deep_pattern(depth));
    with(&set, GREET, replace_section(bytes, "syntax", patched))
}

#[test]
fn a_pattern_at_the_nesting_limit_loads_on_a_small_stack() {
    // 1,999 lists and the variable under them are 2,000 levels, the most
    // allowed. Importing remaps the pattern into the importer's context,
    // recursing a level at a time: it aborted a 2 MB thread at this depth.
    let r = attempt(
        "a pattern at the limit",
        USES_GREET,
        &greet_with_nested_pattern(1999),
    );
    assert!(!r.contains("nested more"), "{r}");
    // One more is refused.
    let r = attempt(
        "a pattern past the limit",
        USES_GREET,
        &greet_with_nested_pattern(2000),
    );
    assert!(r.contains("nested more than 2000"), "{r}");
}

#[test]
fn a_library_cannot_define_another_librarys_macros() {
    // (test evil)'s export defines macro 0 of (test greet), `twice`, as
    // `"hijacked"`. Importers copy what an export holds and keep the first
    // copy of a macro, so this replaced `twice` in `(test user)`.
    let program = "(import (scheme base) (test greet) (test evil)) dummy";
    let set = written(program);
    let export = evil_export(|w| {
        w.uint(1); // macros
        w.uint(0); // of the unit named next
        w.str(GREET);
        w.uint(0); // number 0
        w.str("twice");
        w.byte(0); // syntax-rules
        w.uint(1); // one clause
        w.byte(0); // pattern: _
        w.byte(1); // template: a constant
        string_syntax(w, "hijacked");
        w.uint(0); // no depths
        w.uint(0); // no compiled syntax
        w.uint(0); // no bindings
    });
    let evil = replace_section(find(&set, EVIL), "syntax", export);
    let set = with(&set, EVIL, evil);
    let r = attempt(
        "a macro of another library",
        "(import (scheme base) (test greet) (test evil)) (twice 1)",
        &set,
    );
    assert!(r.contains("of another library"), "{r}");
}

#[test]
fn a_library_cannot_supply_another_librarys_compiled_syntax() {
    let program = "(import (scheme base) (test greet) (test evil)) dummy";
    let set = written(program);
    let export = evil_export(|w| {
        w.uint(0); // macros
        w.uint(1); // compiled syntax, under (test greet)'s key
        w.str(GREET);
        w.uint(0); // no quoted syntax
        w.uint(0); // no patterns
        w.uint(0); // no templates
        w.uint(0); // no bindings
    });
    let evil = replace_section(find(&set, EVIL), "syntax", export);
    let set = with(&set, EVIL, evil);
    let r = attempt(
        "compiled syntax of another library",
        "(import (scheme base) (test greet) (test evil)) (count-args 1 2 3)",
        &set,
    );
    assert!(r.contains("not its own key"), "{r}");
}

#[test]
fn a_transformer_may_not_link_an_internal_primitive() {
    let set = written(PROGRAM);
    let bytes = find(&set, GREET);
    let syntax = section(bytes, "syntax");
    let fields = walk_syntax(&syntax);
    let f = fields.iter().find(|f| f.what == "macro.ir").unwrap();
    let ir_bytes = Reader::new(&syntax[f.at..]).bytes().unwrap().to_vec();
    let mut ir = vali_dist::decode(&ir_bytes).unwrap();
    // The transformer links `%syntax-match`, which syntax-case may; make
    // it `%exit`, which a library of its own may not.
    for g in &mut ir.globals {
        if &*g.name == "%syntax-match" {
            g.name = "%exit".into();
        }
    }
    let mut w = Writer::new();
    w.bytes(&vali_dist::encode(&ir).unwrap());
    let patched = splice(&syntax, f.at, f.len, &w.finish());
    let hostile = with(&set, GREET, replace_section(bytes, "syntax", patched));
    let r = attempt("a transformer linking %exit", PROGRAM, &hostile);
    assert!(r.contains("links %exit"), "{r}");
    // As it is, it links what syntax-case does, and loads.
    assert!(attempt("the transformer as it is", PROGRAM, &set).starts_with("ran"));
}

#[test]
fn a_deeply_nested_syntax_object_is_not_a_stack_overflow() {
    for (case, tail) in [("a deep syntax object", 0), ("then an error", 7)] {
        let export = evil_export(|w| {
            w.uint(0); // macros
            w.uint(1); // compiled syntax
            w.str("k");
            w.uint(1); // one quoted syntax object
            w.raw(&deep_syntax(DEEP));
            w.uint(0); // no patterns
            w.uint(0); // no templates
            w.uint(tail); // bindings: none, or a count the input lacks
        });
        attempt(case, USES_EVIL, &evil_set(export));
    }
}

#[test]
fn a_very_long_constant_list_is_not_a_stack_overflow() {
    // A list of a million elements is 3 MB of constant: cloning, comparing
    // and dropping it must not recurse.
    let set = written(PROGRAM);
    let bytes = find(&set, GREET);
    let mut ir = vali_dist::decode(&section(bytes, "invoke")).unwrap();
    let mut c = vali_dist::Constant::EmptyList;
    for _ in 0..1_000_000 {
        c = vali_dist::Constant::Pair(Box::new(vali_dist::Constant::Integer(1)), Box::new(c));
    }
    ir.constants.push(c);
    let invoke = vali_dist::encode(&ir).unwrap();
    let hostile = with(&set, GREET, replace_section(bytes, "invoke", invoke));
    attempt("a long constant", PROGRAM, &hostile);
}

/// Every number in `(test greet)`'s library and syntax sections, one at a
/// time, replaced by values that stress counts and indices: nothing may
/// panic or take long, whatever it makes the artifact say. Structure-aware,
/// as byte flips mostly stop at the first check.
#[test]
fn replacing_any_number_in_an_artifact_never_panics() {
    let set = written(PROGRAM);
    let bytes = find(&set, GREET);
    let text = [
        "identity",
        "text",
        "name",
        "symbol",
        "key",
        "datum",
        "bytes",
        "ir",
        "atom.text",
    ];
    let values = [0u64, 99, 1 << 34, u64::MAX];
    let start = Instant::now();
    let mut tried = 0;
    let mut refused = 0;
    for (name, walk) in [
        ("library", walk_library as fn(&[u8]) -> Vec<Field>),
        ("syntax", walk_syntax),
    ] {
        let original = section(bytes, name);
        for field in walk(&original) {
            let skip = field.what.ends_with("(whole)")
                || text.iter().any(|t| field.what.ends_with(t))
                || field.len > 10;
            if skip {
                continue;
            }
            for value in values {
                if value == field.value {
                    continue;
                }
                let patched = splice(&original, field.at, field.len, &uint(value));
                let hostile = with(&set, GREET, replace_section(bytes, name, patched));
                let case = format!(
                    "{name} {} {} at {} = {value}",
                    field.ctx, field.what, field.at
                );
                tried += 1;
                if attempt_quietly(&case, PROGRAM, &hostile).starts_with("refused") {
                    refused += 1;
                }
            }
        }
    }
    eprintln!(
        "{tried} mutated artifacts, {refused} refused, in {:?}",
        start.elapsed()
    );
    assert!(refused > tried / 4, "most mutations should be noticed");
}

/// [`attempt`] without reporting each case.
#[track_caller]
fn attempt_quietly(case: &str, program: &str, set: &Set) -> String {
    let start = Instant::now();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let Ok(program) = compile(program, &reading(set)) else {
            return "refused".to_owned();
        };
        let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
        cx.install_primitives().unwrap();
        match vali_compiler::load(&mut cx, program) {
            Ok(id) => match cx.run(id) {
                Ok(_) => "ran".to_owned(),
                Err(_) => "failed".to_owned(),
            },
            Err(_) => "refused".to_owned(),
        }
    }));
    let outcome = outcome.unwrap_or_else(|_| panic!("{case}: panicked"));
    assert!(
        start.elapsed().as_secs() < 10,
        "{case}: took {:?}",
        start.elapsed()
    );
    outcome
}

// ---------------------------------------------------------------------
// Finding the fields of an artifact's sections.

/// A field the walker met.
#[derive(Clone, Debug)]
pub struct Field {
    pub at: usize,
    pub len: usize,
    pub what: &'static str,
    pub ctx: String,
    pub value: u64,
}

pub struct Walk<'a> {
    r: Reader<'a>,
    pub fields: Vec<Field>,
    ctx: Vec<&'static str>,
}

type W<T> = Result<T, DecodeError>;

impl<'a> Walk<'a> {
    pub fn new(bytes: &'a [u8]) -> Walk<'a> {
        Walk {
            r: Reader::new(bytes),
            fields: Vec::new(),
            ctx: Vec::new(),
        }
    }

    fn note(&mut self, at: usize, what: &'static str, value: u64) {
        self.fields.push(Field {
            at,
            len: self.r.offset() - at,
            what,
            ctx: self.ctx.join("/"),
            value,
        });
    }

    fn uint(&mut self, what: &'static str) -> W<u64> {
        let at = self.r.offset();
        let n = self.r.uint()?;
        self.note(at, what, n);
        Ok(n)
    }

    fn byte(&mut self, what: &'static str) -> W<u8> {
        let at = self.r.offset();
        let b = self.r.byte()?;
        self.note(at, what, u64::from(b));
        Ok(b)
    }

    fn str(&mut self, what: &'static str) -> W<String> {
        let at = self.r.offset();
        let s = self.r.str()?.to_owned();
        self.note(at, what, s.len() as u64);
        Ok(s)
    }

    fn bytes(&mut self, what: &'static str) -> W<()> {
        let at = self.r.offset();
        let b = self.r.bytes()?;
        self.note(at, what, b.len() as u64);
        Ok(())
    }

    fn enter(&mut self, c: &'static str) {
        self.ctx.push(c);
    }

    fn leave(&mut self) {
        self.ctx.pop();
    }

    /// A whole structure, from `at` to here.
    fn whole(&mut self, at: usize, what: &'static str) {
        self.note(at, what, 0);
    }

    pub fn state(&mut self) -> W<()> {
        let n = self.uint("scopes.count")?;
        for _ in 0..n {
            self.byte("scope.kind")?;
        }
        let n = self.uint("sets.count")?;
        for _ in 0..n {
            let len = self.uint("set.len")?;
            for _ in 0..len {
                self.uint("set.scope")?;
            }
        }
        self.uint("core_scope")?;
        self.uint("core")?;
        self.uint("next_macro")?;
        let n = self.uint("imported.count")?;
        for _ in 0..n {
            self.unit()?;
        }
        let n = self.uint("macros.count")?;
        for _ in 0..n {
            let at = self.r.offset();
            self.enter("macro");
            self.macro_id()?;
            self.r#macro()?;
            self.leave();
            self.whole(at, "macro(whole)");
        }
        let n = self.uint("compiled.count")?;
        for _ in 0..n {
            self.enter("compiled");
            self.str("compiled.key")?;
            self.compiled()?;
            self.leave();
        }
        let n = self.uint("bindings.count")?;
        for _ in 0..n {
            self.enter("binding");
            self.str("binding.symbol")?;
            self.uint("binding.set")?;
            if self.byte("phases.tag")? == 1 {
                let n = self.uint("phases.count")?;
                for _ in 0..n {
                    self.uint("phase")?;
                }
            }
            self.binding()?;
            self.leave();
        }
        self.r.end()
    }

    pub fn library(&mut self) -> W<()> {
        self.str("library.identity")?;
        self.str("library.key")?;
        let n = self.uint("imports.count")?;
        for _ in 0..n {
            self.unit()?;
        }
        let n = self.uint("interface.count")?;
        for _ in 0..n {
            self.str("interface.name")?;
            self.binding()?;
        }
        self.r.end()
    }

    fn unit(&mut self) -> W<()> {
        if self.uint("unit.ref")? == 0 {
            self.str("unit.identity")?;
        }
        Ok(())
    }

    fn macro_id(&mut self) -> W<()> {
        self.unit()?;
        self.uint("macro.index")?;
        Ok(())
    }

    fn r#macro(&mut self) -> W<()> {
        self.str("macro.name")?;
        match self.byte("transformer.tag")? {
            0 => {
                let n = self.uint("clauses.count")?;
                for _ in 0..n {
                    self.enter("clause");
                    self.pattern()?;
                    self.template()?;
                    self.depths()?;
                    self.leave();
                }
            }
            _ => {
                self.uint("macro.phase")?;
                self.bytes("macro.ir")?;
            }
        }
        Ok(())
    }

    fn depths(&mut self) -> W<()> {
        let at = self.r.offset();
        let n = self.uint("depths.count")?;
        for _ in 0..n {
            self.uint("depth")?;
        }
        self.whole(at, "depths(whole)");
        Ok(())
    }

    fn compiled(&mut self) -> W<()> {
        let n = self.uint("quoted.count")?;
        for _ in 0..n {
            self.syntax()?;
        }
        let n = self.uint("cpatterns.count")?;
        for _ in 0..n {
            self.enter("cpattern");
            self.pattern()?;
            self.uint("cpattern.vars")?;
            self.leave();
        }
        let n = self.uint("ctemplates.count")?;
        for _ in 0..n {
            self.enter("ctemplate");
            self.template()?;
            self.depths()?;
            self.leave();
        }
        Ok(())
    }

    fn binding(&mut self) -> W<()> {
        match self.byte("binding.tag")? {
            0 | 2 | 4 => {
                self.str("binding.name")?;
            }
            1 => {
                self.unit()?;
                self.str("binding.name")?;
            }
            _ => self.macro_id()?,
        }
        Ok(())
    }

    fn span(&mut self) -> W<()> {
        if self.uint("span.source")? == 1 {
            self.str("source.identity")?;
            self.str("source.text")?;
        }
        self.uint("span.start")?;
        self.uint("span.end")?;
        Ok(())
    }

    fn origin(&mut self) -> W<()> {
        let n = self.uint("origin.new")?;
        self.uint("origin.base")?;
        for _ in 0..n {
            self.str("origin.name")?;
            self.span()?;
        }
        Ok(())
    }

    fn syntax(&mut self) -> W<()> {
        let at = self.r.offset();
        self.enter("syntax");
        match self.byte("syntax.tag")? {
            0 => {
                self.str("syntax.identifier")?;
            }
            1 => {
                let n = self.uint("syntax.items")?;
                for _ in 0..n {
                    self.syntax()?;
                }
                if self.byte("syntax.tail")? == 1 {
                    self.syntax()?;
                }
            }
            2 => {
                let n = self.uint("syntax.items")?;
                for _ in 0..n {
                    self.syntax()?;
                }
            }
            _ => match self.byte("atom.tag")? {
                1 => {
                    self.byte("atom.bool")?;
                }
                2 => {
                    self.uint("atom.char")?;
                }
                6 => self.bytes("atom.bytes")?,
                _ => {
                    self.str("atom.text")?;
                }
            },
        }
        self.uint("syntax.set")?;
        let n = self.uint("pending.count")?;
        for _ in 0..n {
            self.uint("pending.scope")?;
            self.byte("pending.effect")?;
        }
        self.span()?;
        self.origin()?;
        self.leave();
        self.whole(at, "syntax(whole)");
        Ok(())
    }

    fn patterns(&mut self) -> W<()> {
        let n = self.uint("patterns.count")?;
        for _ in 0..n {
            self.pattern()?;
        }
        Ok(())
    }

    fn pattern(&mut self) -> W<()> {
        let at = self.r.offset();
        self.enter("pattern");
        match self.byte("pattern.tag")? {
            0 => {}
            1 => {
                self.uint("pattern.var")?;
            }
            2 => self.syntax()?,
            3 => {
                self.str("pattern.datum")?;
            }
            4 => {
                self.patterns()?;
                self.repeat()?;
                if self.byte("pattern.has_tail")? == 1 {
                    self.pattern()?;
                }
            }
            _ => {
                self.patterns()?;
                self.repeat()?;
            }
        }
        self.leave();
        self.whole(at, "pattern(whole)");
        Ok(())
    }

    fn repeat(&mut self) -> W<()> {
        if self.byte("repeat.flag")? == 1 {
            self.enter("each");
            self.pattern()?;
            self.leave();
            let at = self.r.offset();
            let n = self.uint("repeat.vars.count")?;
            for _ in 0..n {
                self.uint("repeat.var")?;
            }
            self.whole(at, "repeat.vars(whole)");
            self.patterns()?;
        }
        Ok(())
    }

    fn template(&mut self) -> W<()> {
        let at = self.r.offset();
        self.enter("template");
        match self.byte("template.tag")? {
            0 => {
                self.uint("template.var")?;
            }
            1 => self.syntax()?,
            2 => {
                self.items()?;
                if self.byte("template.has_tail")? == 1 {
                    self.template()?;
                }
                self.syntax()?;
            }
            _ => {
                self.items()?;
                self.syntax()?;
            }
        }
        self.leave();
        self.whole(at, "template(whole)");
        Ok(())
    }

    fn items(&mut self) -> W<()> {
        let n = self.uint("items.count")?;
        for _ in 0..n {
            self.template()?;
            self.uint("item.ellipses")?;
        }
        Ok(())
    }
}

pub fn walk_syntax(bytes: &[u8]) -> Vec<Field> {
    let mut w = Walk::new(bytes);
    w.state().expect("the walker mirrors the decoder");
    w.fields
}

pub fn walk_library(bytes: &[u8]) -> Vec<Field> {
    let mut w = Walk::new(bytes);
    w.library().expect("the walker mirrors the decoder");
    w.fields
}
