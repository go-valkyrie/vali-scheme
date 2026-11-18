//! Lazy scope propagation in [`Syntax`], checked against the reference
//! type, which propagates every change to every node at once.

use vali_syntax::reference::{self, Shape};
use vali_syntax::{
    Exposed, Scope, ScopeContext, ScopeKind, ScopeSetId, SourceId, Span, Syntax, TextRange,
};

fn read(text: &str) -> vali_syntax::Datum {
    let mut read = vali_syntax::read(text, SourceId(0));
    assert!(read.errors.is_empty(), "{:?}", read.errors);
    read.datums.remove(0)
}

/// Every node, depth first, as its datum and its scopes. Lists and vectors
/// print as their kind, so the shapes are compared too.
fn flatten(context: &mut ScopeContext, syntax: &Syntax, out: &mut Vec<(String, Vec<Scope>)>) {
    let scopes = context.scopes(syntax.scopes()).to_vec();
    match syntax.expose(context).unwrap() {
        Exposed::Identifier(name) => out.push((name.to_string(), scopes)),
        Exposed::Atom(_) => out.push((syntax.to_datum().to_string(), scopes)),
        Exposed::List { items, tail } => {
            out.push((format!("list {}", tail.is_some()), scopes));
            for item in items.iter().chain(&tail) {
                flatten(context, item, out);
            }
        }
        Exposed::Vector(items) => {
            out.push(("vector".into(), scopes));
            for item in &items {
                flatten(context, item, out);
            }
        }
    }
}

fn flatten_reference(
    context: &ScopeContext,
    syntax: &reference::Syntax,
    out: &mut Vec<(String, Vec<Scope>)>,
) {
    let scopes = context.scopes(syntax.scopes).to_vec();
    match &syntax.shape {
        Shape::Identifier(name) => out.push((name.to_string(), scopes)),
        Shape::Atom(_) => out.push((syntax.to_datum().to_string(), scopes)),
        Shape::List { items, tail } => {
            out.push((format!("list {}", tail.is_some()), scopes));
            for item in items.iter().chain(tail.as_deref()) {
                flatten_reference(context, item, out);
            }
        }
        Shape::Vector(items) => {
            out.push(("vector".into(), scopes));
            for item in items {
                flatten_reference(context, item, out);
            }
        }
    }
}

/// A small deterministic generator, so failures reproduce.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % n as u64) as usize
    }
}

const SOURCE: &str = r#"(a (b c . d) #(e (f)) 1 "s" (g (h (i j))) () k)"#;

#[test]
fn lazy_propagation_matches_eager() {
    for seed in 0..200 {
        let mut context = ScopeContext::new();
        let scopes: Vec<Scope> = (0..5)
            .map(|_| context.new_scope(ScopeKind::Other).unwrap())
            .collect();
        let datum = read(SOURCE);
        let mut lazy = Syntax::from_datum(&datum, ScopeSetId::EMPTY);
        let mut eager = reference::Syntax::from_datum(&datum, ScopeSetId::EMPTY);
        let mut random = Lcg(seed);
        for step in 0..40 {
            let scope = scopes[random.below(scopes.len())];
            match random.below(6) {
                0 => {
                    lazy = lazy.add(&mut context, scope).unwrap();
                    eager.add(&mut context, scope).unwrap();
                }
                1 => {
                    lazy = lazy.remove(&mut context, scope).unwrap();
                    eager.remove(&mut context, scope).unwrap();
                }
                2 | 3 => {
                    lazy = lazy.flip(&mut context, scope).unwrap();
                    eager.flip(&mut context, scope).unwrap();
                }
                4 => {
                    // A macro step's output: the input's elements reversed,
                    // in a new list with a fresh node's scopes.
                    if let (Some(items), Some(eager_items)) =
                        (lazy.list_items(&mut context).unwrap(), eager.list())
                    {
                        let set = context.intern(&[scope]).unwrap();
                        let span = lazy.span();
                        lazy =
                            Syntax::list(items.into_iter().rev().collect(), None, set, span, None);
                        eager = reference::Syntax {
                            shape: Shape::List {
                                items: eager_items.iter().rev().cloned().collect(),
                                tail: None,
                            },
                            scopes: set,
                            span,
                            origin: None,
                        };
                    }
                }
                _ => {
                    // Descend into a list element that is itself a list.
                    if let (Some(items), Some(eager_items)) =
                        (lazy.list_items(&mut context).unwrap(), eager.list())
                    {
                        let i = random.below(items.len().max(1));
                        if items.get(i).is_some_and(|s| {
                            s.is_list()
                                && s.list_items(&mut context)
                                    .unwrap()
                                    .is_some_and(|l| !l.is_empty())
                        }) {
                            lazy = items[i].clone();
                            eager = eager_items[i].clone();
                        }
                    }
                }
            }
            let (mut a, mut b) = (Vec::new(), Vec::new());
            flatten(&mut context, &lazy, &mut a);
            flatten_reference(&context, &eager, &mut b);
            assert_eq!(a, b, "seed {seed}, step {step}");
        }
    }
}

#[test]
fn a_flip_and_its_undo_leave_nothing_pending() {
    let mut context = ScopeContext::new();
    let scope = context.new_scope(ScopeKind::Macro).unwrap();
    let syntax = Syntax::from_datum(&read("(a (b c))"), ScopeSetId::EMPTY);
    let twice = syntax
        .flip(&mut context, scope)
        .unwrap()
        .flip(&mut context, scope)
        .unwrap();
    // Taking it apart creates no new scope sets: nothing was pending.
    let sets = context.set_count();
    let mut out = Vec::new();
    flatten(&mut context, &twice, &mut out);
    assert_eq!(context.set_count(), sets);
    assert!(out.iter().all(|(_, scopes)| scopes.is_empty()));
}

#[test]
fn datum_conversions() {
    let mut context = ScopeContext::new();
    let scope = context.new_scope(ScopeKind::Other).unwrap();
    let syntax = Syntax::from_datum(&read("(a #(b 1) . c)"), ScopeSetId::EMPTY)
        .add(&mut context, scope)
        .unwrap();
    assert_eq!(syntax.to_datum().to_string(), "(a #(b 1) . c)");

    // datum->syntax takes the context's scopes and span for every node.
    let span = Span {
        source: SourceId(7),
        range: TextRange::new(3.into(), 4.into()),
    };
    let context_id = Syntax::identifier("k", context.intern(&[scope]).unwrap(), span, None);
    let made = Syntax::datum_to_syntax(&context_id, &read("(x y)"));
    let items = made.list_items(&mut context).unwrap().unwrap();
    assert_eq!(items[1].span(), span);
    assert_eq!(context.scopes(items[1].scopes()), &[scope]);
}

/// Remapping syntax into another context keeps its structure and, scope
/// for scope, its sets, pending changes included: each scope maps to one
/// fresh scope of the same kind, and an identified scope to the one given.
#[test]
fn remapping_keeps_structure_and_scopes() {
    use vali_syntax::{Exposed, Remap, ScopeContext, ScopeKind, SourceId, Syntax};
    let mut from = ScopeContext::new();
    let core = from.new_scope(ScopeKind::Library).unwrap();
    let library = from.new_scope(ScopeKind::Library).unwrap();
    let step = from.new_scope(ScopeKind::Macro).unwrap();
    let top = from.intern(&[core, library]).unwrap();
    let datum = &vali_syntax::read("(x (y))", SourceId(0)).datums[0];
    // The flip is pending below the list's own node.
    let stx = Syntax::from_datum(datum, top)
        .flip(&mut from, step)
        .unwrap();

    let mut to = ScopeContext::new();
    let to_core = to.new_scope(ScopeKind::Library).unwrap();
    let _ = to.new_scope(ScopeKind::Other).unwrap();
    let mut remap = Remap::new();
    remap.identify(core, to_core);
    let copy = stx.remap(&mut remap, &from, &mut to).unwrap();

    let Exposed::List { items, .. } = copy.expose(&mut to).unwrap() else {
        panic!("a list");
    };
    let x = &items[0];
    assert_eq!(x.as_identifier(), Some("x"));
    let scopes = to.scopes(x.scopes()).to_vec();
    assert_eq!(scopes.len(), 3);
    assert!(scopes.contains(&to_core));
    let kinds: Vec<ScopeKind> = scopes.iter().map(|&s| to.kind(s)).collect();
    assert!(kinds.contains(&ScopeKind::Macro), "{kinds:?}");
    assert!(remap.has_met_all(&from, top));
    // The same scope maps to the same scope wherever it is met.
    let again = Syntax::from_datum(datum, top)
        .remap(&mut remap, &from, &mut to)
        .unwrap();
    assert!(to.is_subset(again.scopes(), x.scopes()));
}
