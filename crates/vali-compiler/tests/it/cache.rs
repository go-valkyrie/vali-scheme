//! The library cache: libraries compiled once and reused (ADR 0001 #10).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use vali_compiler::{
    Cache, Identity, LibraryName, LoadError, Loader, Program, Source, compile_program_cached,
};
use vali_dist::Digest;
use vali_runtime::{Cx, HeapConfig, NoCollection};

/// Serves libraries from a list of names and texts, which a test may edit.
struct Map(Mutex<Vec<(&'static str, String)>>);

impl Map {
    fn new(libraries: &[(&'static str, &str)]) -> Map {
        Map(Mutex::new(
            libraries.iter().map(|(n, t)| (*n, t.to_string())).collect(),
        ))
    }

    fn edit(&self, name: &str, text: &str) {
        let mut all = self.0.lock().unwrap();
        all.iter_mut().find(|(n, _)| *n == name).unwrap().1 = text.to_owned();
    }
}

impl Loader for Map {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        let wanted = name.to_string();
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|(n, _)| *n == wanted)
            .map(|(n, text)| Source::scheme(Identity::new(&format!("lib:{n}")), text)))
    }
}

const BASE: &str = "(define-library (test base)
  (import (scheme base))
  (export base-value)
  (begin (define (base-value) 10)))";

const MIDDLE: &str = "(define-library (test middle)
  (import (scheme base) (test base))
  (export middle-value twice)
  (begin
    (define-syntax twice (syntax-rules () ((_ e) (begin e e))))
    (define (middle-value) (+ (base-value) 1))))";

const TOP: &str = "(define-library (test top)
  (import (scheme base) (test middle))
  (export top-value)
  (begin (define (top-value) (* 2 (middle-value)))))";

const PROGRAM: &str = "(import (scheme base) (test top)) (top-value)";

fn libraries() -> Map {
    Map::new(&[
        ("(test base)", BASE),
        ("(test middle)", MIDDLE),
        ("(test top)", TOP),
    ])
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vali-it-cache-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn compile(loader: &Map, cache: &Cache) -> Program {
    compile_program_cached(PROGRAM, &Identity::new("main"), loader, cache)
        .unwrap_or_else(|e| panic!("{e}"))
}

fn run(program: Program) -> String {
    let mut cx = Cx::<NoCollection>::new(&HeapConfig::default());
    cx.install_primitives().unwrap();
    let id = vali_compiler::load(&mut cx, program).unwrap();
    let result = cx.run(id).unwrap();
    cx.write_string(&result).unwrap()
}

fn names(identities: &[Identity]) -> Vec<&str> {
    let mut names: Vec<&str> = identities.iter().map(|i| i.as_str()).collect();
    names.sort();
    names
}

const ALL: [&str; 3] = ["lib:(test base)", "lib:(test middle)", "lib:(test top)"];

/// Every entry file under `root`.
fn entries(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in std::fs::read_dir(root).into_iter().flatten().flatten() {
        for file in std::fs::read_dir(dir.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            found.push(file.path());
        }
    }
    found.sort();
    found
}

#[test]
fn a_warm_run_loads_from_the_cache() {
    let dir = scratch("warm");
    let cache = Cache::new(&dir);
    let loader = libraries();
    let cold = compile(&loader, &cache);
    // Cold: all three compiled and stored.
    assert!(cold.cache.hits.is_empty());
    assert_eq!(names(&cold.cache.stored), ALL);
    assert_eq!(run(cold), "22");
    assert_eq!(entries(&dir).len(), 3);
    // Warm: none compiled, none stored; Vali's own libraries are not
    // kept, since the standard library has its own artifacts.
    let warm = compile(&loader, &cache);
    assert_eq!(names(&warm.cache.hits), ALL);
    assert!(warm.cache.stored.is_empty() && warm.cache.refused.is_empty());
    assert_eq!(run(warm), "22");
    assert_eq!(entries(&dir).len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_source_edit_invalidates() {
    let dir = scratch("edit");
    let cache = Cache::new(&dir);
    let loader = libraries();
    compile(&loader, &cache);
    // Only what is edited is compiled again: base's value is not part of
    // what middle observes of it, so middle and top still load.
    loader.edit("(test base)", &BASE.replace("10", "20"));
    let edited = compile(&loader, &cache);
    assert_eq!(
        names(&edited.cache.hits),
        ["lib:(test middle)", "lib:(test top)"]
    );
    assert_eq!(names(&edited.cache.stored), ["lib:(test base)"]);
    assert_eq!(run(edited), "42");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_changed_dependency_invalidates_its_importers() {
    let dir = scratch("dependency");
    let cache = Cache::new(&dir);
    let loader = libraries();
    compile(&loader, &cache);
    // Middle's export changes, so what top was compiled against is gone,
    // though top's own source is not.
    loader.edit(
        "(test middle)",
        &MIDDLE
            .replace(
                "(export middle-value twice)",
                "(export middle-value twice extra)",
            )
            .replace(
                "(define (middle-value)",
                "(define (extra) 0) (define (middle-value)",
            ),
    );
    let changed = compile(&loader, &cache);
    assert_eq!(names(&changed.cache.hits), ["lib:(test base)"]);
    assert_eq!(
        names(&changed.cache.stored),
        ["lib:(test middle)", "lib:(test top)"]
    );
    // Top's entry was found and refused, then replaced.
    let refused = names_of_refused(&changed);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].starts_with("lib:(test top)"), "{refused:?}");
    assert!(refused[0].contains("has changed"), "{refused:?}");
    assert_eq!(run(changed), "22");
    let again = compile(&loader, &cache);
    assert_eq!(names(&again.cache.hits), ALL);
    let _ = std::fs::remove_dir_all(&dir);
}

fn names_of_refused(program: &Program) -> Vec<String> {
    program
        .cache
        .refused
        .iter()
        .map(|(i, why)| format!("{}: {why}", i.as_str()))
        .collect()
}

/// Rewraps `artifact` as a well-formed entry, as the cache would write it.
fn wrap(artifact: &[u8]) -> Vec<u8> {
    let mut bytes = b"valicach".to_vec();
    bytes.extend_from_slice(&Digest::of(artifact).0);
    bytes.extend_from_slice(artifact);
    bytes
}

#[test]
fn a_tampered_entry_is_refused_and_replaced() {
    let dir = scratch("tamper");
    let cache = Cache::new(&dir);
    let loader = libraries();
    compile(&loader, &cache);
    let files = entries(&dir);
    assert_eq!(files.len(), 3);
    let artifacts: Vec<Vec<u8>> = files
        .iter()
        .map(|f| std::fs::read(f).unwrap()[40..].to_vec())
        .collect();
    let mangled: [(&str, Vec<u8>); 4] = [
        // Whole as an entry, but not an artifact.
        ("not an artifact", wrap(b"hello")),
        // An artifact cut short, its entry rewrapped so the frame is whole.
        ("truncated", wrap(&artifacts[0][..artifacts[0].len() / 2])),
        // Another library's artifact under this one's name.
        ("swapped", wrap(&artifacts[1])),
        // Damage the frame's own check catches.
        ("flipped", {
            let mut bytes = std::fs::read(&files[0]).unwrap();
            let middle = bytes.len() / 2;
            bytes[middle] ^= 0x40;
            bytes
        }),
    ];
    for (what, bytes) in mangled {
        std::fs::write(&files[0], &bytes).unwrap();
        let program = compile(&loader, &cache);
        // Whichever library's entry was files[0], it was not trusted: it
        // was compiled and stored again, and the program is right.
        assert_eq!(program.cache.stored.len(), 1, "{what}");
        assert_eq!(program.cache.hits.len(), 2, "{what}");
        if what != "flipped" {
            assert_eq!(program.cache.refused.len(), 1, "{what}");
        }
        assert_eq!(run(program), "22", "{what}");
        // And the entry is whole again.
        let after = compile(&loader, &cache);
        assert_eq!(after.cache.hits.len(), 3, "{what}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_runs_do_not_corrupt_the_cache() {
    let dir = scratch("concurrent");
    let loader = libraries();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let cache = Cache::new(&dir);
                for _ in 0..3 {
                    let program = compile(&loader, &cache);
                    assert_eq!(run(program), "22");
                }
            });
        }
    });
    // Whole entries, no partial writes left, and all of them good.
    let files = entries(&dir);
    assert_eq!(files.len(), 3, "{files:?}");
    let program = compile(&loader, &Cache::new(&dir));
    assert_eq!(names(&program.cache.hits), ALL);
    assert!(program.cache.refused.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cache_that_cannot_be_written_does_not_fail_the_run() {
    let dir = scratch("unwritable");
    // The root is a file, so no directory can be made in it.
    std::fs::write(&dir, b"").unwrap();
    let program = compile(&libraries(), &Cache::new(&dir));
    assert_eq!(program.cache.failed.len(), 3);
    assert_eq!(run(program), "22");
    let _ = std::fs::remove_file(&dir);
}
