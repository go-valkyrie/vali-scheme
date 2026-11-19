//! A directory of compiled libraries, reused between runs (ADR 0001 #10).
//!
//! Expanding a library from source costs several times what loading its
//! artifact does, so a host that compiles the same libraries run after run
//! can keep their distribution-format artifacts here and pass them back
//! next time. [`compile_program_cached`](crate::compile_program_cached) is
//! the way in. The cache is off unless a host makes one and asks: nothing
//! in this crate reads or writes a directory on its own.
//!
//! An entry is never trusted to be sound. Loading it makes every check an
//! artifact always gets (ADR 0001 #5: build stamp, source hash,
//! dependencies, links, exports), so a stale or damaged entry, or one that
//! fails validation, is refused, the library is compiled from source, and
//! the entry is replaced. That is not a defence against someone who can
//! write the directory: an entry they edit that stays valid, with its hash
//! updated, is loaded. Only
//! distribution-format artifacts are kept, never run-format bytecode: that
//! is made again at load, a function at a time (ADR 0017 #10).
//!
//! # Layout
//!
//! ```text
//! ROOT/<stamp>/<key>.vac
//! ```
//!
//! `<stamp>` is a hash of the build stamp, so artifacts of another build of
//! Vali sit in directories of their own and are evicted when they are the
//! oldest. `<key>` is a hash of the library's identity, the hash of its
//! source (its text, includes and what its `cond-expand`s found about
//! libraries), whether the optimiser ran, and the operating system,
//! architecture and byte order the cache runs on (ADR
//! 0021: until artifacts record the platform identifiers they tested, a
//! cache must not carry one between platforms). What the library imports
//! is not in the key, because it is not known before the imports are
//! loaded: the artifact records each import's hash, which the load checks,
//! so a changed dependency refuses the entry and replaces it, under the
//! same key. An entry is a short header, a hash of the artifact, and the
//! artifact, so damage is noticed before decoding.
//!
//! # Concurrent runs
//!
//! An entry is written to a file of its own in the same directory and
//! renamed over its name, which is atomic where the rename is: a reader
//! sees the old entry or the new one, never half of one, and two writers
//! of the same entry leave one whole entry (they compile the same
//! artifact, bytes and all). Nothing locks.
//!
//! # Eviction
//!
//! [`Cache::prune`] keeps the directory under [`Cache::max_bytes`] by
//! deleting the entries least recently used first. Reading an entry
//! touches it, at most once an hour, so recency is the modification time.
//! [`compile_program_cached`](crate::compile_program_cached) prunes after
//! it writes. [`Cache::clear`] deletes every entry.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use vali_dist::{Digest, Hasher};

use crate::Identity;

/// The start of an entry, so a file that is not one is not mistaken for
/// one.
const MAGIC: &[u8; 8] = b"valicach";

/// The extension of an entry.
const EXTENSION: &str = "vac";

/// The length of the name of a build's directory: hex digits of the hash
/// of its stamp.
const STAMP_LEN: usize = 16;

/// Whether `name` is what [`Cache::build_dir`] makes.
fn is_build_dir(name: &str) -> bool {
    name.len() == STAMP_LEN && is_hex(name)
}

/// Whether `name` is what [`Cache::entry`] makes: 64 hex digits and the
/// extension.
fn is_entry(name: &str) -> bool {
    name.strip_suffix(&format!(".{EXTENSION}"))
        .is_some_and(|key| key.len() == 64 && is_hex(key))
}

/// Whether `name` is what [`Cache::put`] writes before it renames:
/// the prefix, a process id and a count.
fn is_temp_name(name: &str) -> bool {
    name.strip_prefix(TEMP_PREFIX)
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(pid, n)| {
            !pid.is_empty()
                && !n.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit())
                && n.bytes().all(|b| b.is_ascii_digit())
        })
}

fn is_hex(text: &str) -> bool {
    text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// What a stale partial write is called, and how old it must be to be
/// deleted: a writer that died leaves one behind.
const TEMP_PREFIX: &str = ".tmp-";
const TEMP_TTL: Duration = Duration::from_secs(3600);

/// How long an entry goes unread before reading it touches it again.
const TOUCH_AFTER: Duration = Duration::from_secs(3600);

/// A cache of compiled libraries in one directory.
#[derive(Clone, Debug)]
pub struct Cache {
    root: PathBuf,
    max_bytes: u64,
}

/// What [`Cache::stats`] counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Entries, of any build of Vali.
    pub entries: u64,
    /// Their size in bytes.
    pub bytes: u64,
}

/// What a run did with the cache, per library.
#[derive(Clone, Debug, Default)]
pub struct CacheReport {
    /// Libraries loaded from an entry rather than compiled.
    pub hits: Vec<Identity>,
    /// Libraries whose entry was refused, with the reason; each was then
    /// compiled from source.
    pub refused: Vec<(Identity, String)>,
    /// Libraries compiled and then stored.
    pub stored: Vec<Identity>,
    /// Entries that could not be written, with the error. The run is
    /// unaffected: the cache is an optimisation.
    pub failed: Vec<(Identity, String)>,
}

impl Cache {
    /// The size the cache is kept under unless [`with_max_bytes`]
    /// says otherwise: 256 MiB, a few thousand libraries.
    ///
    /// [`with_max_bytes`]: Cache::with_max_bytes
    pub const DEFAULT_MAX_BYTES: u64 = 256 << 20;

    /// A cache in `root`, made when something is first stored in it.
    pub fn new(root: impl Into<PathBuf>) -> Cache {
        Cache {
            root: root.into(),
            max_bytes: Cache::DEFAULT_MAX_BYTES,
        }
    }

    /// This cache, kept under `max_bytes` instead.
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Cache {
        self.max_bytes = max_bytes;
        self
    }

    /// The directory the cache lives in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The size the cache is kept under.
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// The directory a user's cache is in unless they say otherwise:
    /// `$VALI_CACHE_DIR` if it is set and not empty; otherwise the
    /// platform's per-user cache directory under `vali`:
    /// `$XDG_CACHE_HOME/vali` or `~/.cache/vali` on Linux and other Unix,
    /// `~/Library/Caches/vali` on macOS, `%LOCALAPPDATA%\vali\cache` on
    /// Windows. `None` if the environment gives none of these, in which
    /// case a host should run without a cache.
    pub fn user_dir() -> Option<PathBuf> {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        if let Some(dir) = var("VALI_CACHE_DIR") {
            return Some(PathBuf::from(dir));
        }
        if cfg!(windows) {
            return var("LOCALAPPDATA").map(|d| PathBuf::from(d).join("vali").join("cache"));
        }
        if cfg!(target_os = "macos") {
            return var("HOME").map(|h| PathBuf::from(h).join("Library/Caches/vali"));
        }
        if let Some(dir) = var("XDG_CACHE_HOME").map(PathBuf::from)
            && dir.is_absolute()
        {
            return Some(dir.join("vali"));
        }
        var("HOME").map(|h| PathBuf::from(h).join(".cache/vali"))
    }

    /// The directory this build's entries are in.
    fn build_dir(&self) -> PathBuf {
        let stamp = Digest::of(crate::expand::build_stamp().as_bytes());
        self.root.join(&stamp.hex()[..STAMP_LEN])
    }

    /// The file for the library `identity` compiled from `source`.
    fn entry(&self, identity: &str, source: &Digest) -> PathBuf {
        let mut key = Hasher::new();
        key.field(identity.as_bytes());
        key.field(&source.0);
        key.field(std::env::consts::OS.as_bytes());
        key.field(std::env::consts::ARCH.as_bytes());
        key.field(if crate::expand::optimising() {
            b"optimised"
        } else {
            b"unoptimised"
        });
        key.field(if cfg!(target_endian = "little") {
            b"little"
        } else {
            b"big"
        });
        self.build_dir()
            .join(format!("{}.{EXTENSION}", key.finish().hex()))
    }

    /// The artifact stored for `identity` compiled from `source`, if there
    /// is an intact entry. Whether it fits is for the load to say.
    pub(crate) fn get(&self, identity: &str, source: &Digest) -> Option<Vec<u8>> {
        let path = self.entry(identity, source);
        let bytes = fs::read(&path).ok()?;
        let body = bytes.strip_prefix(MAGIC)?.get(32..)?;
        let recorded = &bytes[MAGIC.len()..MAGIC.len() + 32];
        if Digest::of(body).0 != *recorded {
            return None;
        }
        // Recency for eviction; whether it works does not matter.
        if let Ok(modified) = fs::metadata(&path).and_then(|m| m.modified())
            && modified.elapsed().is_ok_and(|age| age > TOUCH_AFTER)
            && let Ok(file) = fs::OpenOptions::new().write(true).open(&path)
        {
            let _ = file.set_modified(SystemTime::now());
        }
        Some(body.to_vec())
    }

    /// Stores `artifact` as the entry for `identity` compiled from
    /// `source`, replacing any. It is written whole or not at all.
    pub(crate) fn put(&self, identity: &str, source: &Digest, artifact: &[u8]) -> io::Result<()> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = self.entry(identity, source);
        let dir = path.parent().expect("an entry is in a directory");
        create_private_dir(dir)?;
        let temp = dir.join(format!(
            "{TEMP_PREFIX}{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut bytes = Vec::with_capacity(MAGIC.len() + 32 + artifact.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&Digest::of(artifact).0);
        bytes.extend_from_slice(artifact);
        let written = fs::write(&temp, &bytes).and_then(|()| fs::rename(&temp, &path));
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        written
    }

    /// Every file of the cache: its directory, name, size and time of
    /// last use. Only what the cache itself makes: entries and partial
    /// writes inside the directories of builds.
    fn files(&self) -> io::Result<Vec<(PathBuf, u64, SystemTime)>> {
        let mut files = Vec::new();
        let roots = match fs::read_dir(&self.root) {
            Ok(roots) => roots,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(files),
            Err(e) => return Err(e),
        };
        for dir in roots.flatten() {
            let named = dir.file_name();
            if !dir.file_type().is_ok_and(|t| t.is_dir()) || !is_build_dir(&named.to_string_lossy())
            {
                continue;
            }
            for file in fs::read_dir(dir.path())?.flatten() {
                let name = file.file_name();
                let name = name.to_string_lossy();
                let ours = is_temp_name(&name) || is_entry(&name);
                let Ok(meta) = file.metadata() else { continue };
                if ours && meta.is_file() {
                    let used = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    files.push((file.path(), meta.len(), used));
                }
            }
        }
        Ok(files)
    }

    /// How many entries there are and how big they are.
    pub fn stats(&self) -> io::Result<CacheStats> {
        let mut stats = CacheStats::default();
        for (path, bytes, _) in self.files()? {
            if !is_temp(&path) {
                stats.entries += 1;
                stats.bytes += bytes;
            }
        }
        Ok(stats)
    }

    /// Deletes the entries used least recently until what is left fits
    /// [`max_bytes`](Cache::max_bytes), and any partial write that was
    /// left behind. Returns how many entries it deleted.
    pub fn prune(&self) -> io::Result<u64> {
        let mut deleted = 0;
        let mut entries = Vec::new();
        let mut total = 0;
        for (path, bytes, used) in self.files()? {
            if is_temp(&path) {
                if used.elapsed().is_ok_and(|age| age > TEMP_TTL) {
                    let _ = fs::remove_file(&path);
                }
            } else {
                total += bytes;
                entries.push((used, path, bytes));
            }
        }
        if total > self.max_bytes {
            entries.sort();
            for (_, path, bytes) in entries {
                if total <= self.max_bytes {
                    break;
                }
                if fs::remove_file(&path).is_ok() {
                    total -= bytes;
                    deleted += 1;
                }
            }
            self.remove_empty_dirs();
        }
        Ok(deleted)
    }

    /// Deletes every entry. What else is in the directory is left alone.
    /// Returns how many it deleted.
    pub fn clear(&self) -> io::Result<u64> {
        let mut deleted = 0;
        for (path, _, _) in self.files()? {
            if fs::remove_file(&path).is_ok() && !is_temp(&path) {
                deleted += 1;
            }
        }
        self.remove_empty_dirs();
        Ok(deleted)
    }

    /// Removes the directories of builds that have nothing in them.
    fn remove_empty_dirs(&self) {
        if let Ok(dirs) = fs::read_dir(&self.root) {
            // `remove_dir` refuses one that has something in it.
            for dir in dirs.flatten() {
                if is_build_dir(&dir.file_name().to_string_lossy()) {
                    let _ = fs::remove_dir(dir.path());
                }
            }
        }
    }
}

fn is_temp(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| is_temp_name(&n.to_string_lossy()))
}

/// Makes `dir` and what is above it, readable by the user alone where the
/// platform has such a thing: what is cached is code that runs.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vali-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn what_is_put_is_got() {
        let dir = scratch("roundtrip");
        let cache = Cache::new(&dir);
        let source = Digest::of(b"source");
        assert_eq!(cache.get("a", &source), None);
        cache.put("a", &source, b"artifact").unwrap();
        assert_eq!(cache.get("a", &source).as_deref(), Some(&b"artifact"[..]));
        // Another library, or another source, is another entry.
        assert_eq!(cache.get("b", &source), None);
        assert_eq!(cache.get("a", &Digest::of(b"other")), None);
        cache.put("a", &source, b"replaced").unwrap();
        assert_eq!(cache.get("a", &source).as_deref(), Some(&b"replaced"[..]));
        assert_eq!(cache.stats().unwrap().entries, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_what_the_cache_made_is_deleted() {
        let dir = scratch("strangers");
        let cache = Cache::new(&dir).with_max_bytes(0);
        cache.put("a", &Digest::of(b"a"), b"artifact").unwrap();
        let stamp = cache.build_dir();
        let strangers = [
            dir.join("notes/design.vac"),
            dir.join("notes/readme.txt"),
            dir.join("build/.tmp-session"),
            stamp.join("other.vac"),
            stamp.join(format!("{}.txt", "a".repeat(64))),
            stamp.join(".tmp-x-y"),
        ];
        for path in &strangers {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"mine").unwrap();
        }
        fs::create_dir_all(dir.join("emptydir")).unwrap();
        // Everything is over the cap, but only the entry is the cache's.
        assert_eq!(cache.prune().unwrap(), 1);
        assert_eq!(cache.clear().unwrap(), 0);
        for path in &strangers {
            assert!(path.exists(), "{path:?}");
        }
        assert!(dir.join("emptydir").is_dir());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn damage_is_noticed() {
        let dir = scratch("damage");
        let cache = Cache::new(&dir);
        let source = Digest::of(b"source");
        cache.put("a", &source, b"artifact").unwrap();
        let path = cache.entry("a", &source);
        let mut bytes = fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert_eq!(cache.get("a", &source), None);
        fs::write(&path, b"short").unwrap();
        assert_eq!(cache.get("a", &source), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_oldest_go_first() {
        let dir = scratch("prune");
        let cache = Cache::new(&dir).with_max_bytes(150);
        for (name, age) in [("old", 300), ("mid", 200), ("new", 100)] {
            let source = Digest::of(name.as_bytes());
            cache.put(name, &source, &[0; 40]).unwrap();
            let file = fs::OpenOptions::new()
                .write(true)
                .open(cache.entry(name, &source))
                .unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(age))
                .unwrap();
        }
        // Three entries of 80 bytes: 240 over 150, so two go.
        assert_eq!(cache.prune().unwrap(), 2);
        assert!(cache.get("new", &Digest::of(b"new")).is_some());
        assert!(cache.get("old", &Digest::of(b"old")).is_none());
        assert_eq!(cache.clear().unwrap(), 1);
        assert_eq!(cache.stats().unwrap(), CacheStats::default());
        let _ = fs::remove_dir_all(&dir);
    }
}
