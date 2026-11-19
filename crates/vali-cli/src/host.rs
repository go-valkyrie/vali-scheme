//! The command's host: what a program run by `vali` may reach.
//!
//! Vali has no ambient authority (ADR 0009 #5), so this is where the
//! command decides. It serves libraries from the library search path,
//! and it grants the file, environment and clock capabilities the user
//! asked for, by serving `(scheme file)`, `(scheme process-context)` and
//! `(scheme time)` over native libraries of its own (ADR 0009 #6). A
//! capability not granted leaves Vali's own version of its library, whose
//! operations raise a condition naming what is missing.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use vali::bridge::NativeLibrary;
use vali::load::{Identity, LibraryName, LoadError, Loader, NamePart, Source};
use vali::ports::Port;

/// Which paths a file capability reaches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Access {
    /// None: the capability is not granted.
    #[default]
    None,
    /// Any path.
    Any,
    /// Paths inside these directories.
    Within(Vec<PathBuf>),
}

impl Access {
    /// Grants access to `dir`, or to any path if there is none.
    pub fn grant(&mut self, dir: Option<PathBuf>) {
        match (&mut *self, dir) {
            (Access::Any, _) => {}
            (_, None) => *self = Access::Any,
            (Access::Within(dirs), Some(dir)) => dirs.push(dir),
            (Access::None, Some(dir)) => *self = Access::Within(vec![dir]),
        }
    }

    fn granted(&self) -> bool {
        *self != Access::None
    }
}

/// Which environment variables the environment capability reaches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum EnvAccess {
    /// None: the capability is not granted.
    #[default]
    None,
    /// Every variable.
    Any,
    /// These variables.
    Names(Vec<String>),
}

impl EnvAccess {
    /// Grants access to `names`, or to every variable if there are none.
    pub fn grant(&mut self, names: Option<Vec<String>>) {
        match (&mut *self, names) {
            (EnvAccess::Any, _) => {}
            (_, None) => *self = EnvAccess::Any,
            (EnvAccess::Names(have), Some(names)) => have.extend(names),
            (EnvAccess::None, Some(names)) => *self = EnvAccess::Names(names),
        }
    }

    fn allows(&self, name: &str) -> bool {
        match self {
            EnvAccess::None => false,
            EnvAccess::Any => true,
            EnvAccess::Names(names) => names.iter().any(|n| n == name),
        }
    }
}

/// The capabilities the user granted. None by default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    /// Reading files, and asking whether they exist.
    pub read: Access,
    /// Writing and deleting files.
    pub write: Access,
    /// Reading environment variables.
    pub env: EnvAccess,
    /// Reading the clock.
    pub clock: bool,
}

impl Grants {
    /// Everything.
    pub fn all() -> Grants {
        Grants {
            read: Access::Any,
            write: Access::Any,
            env: EnvAccess::Any,
            clock: true,
        }
    }
}

/// `path`, absolute and with links resolved, as far as it exists: the part
/// that does not exist yet is appended as written, so a file about to be
/// created resolves too. A `..` in the part that does not exist is refused,
/// since it could climb out of where the rest points.
fn resolve(path: &Path) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
    let mut existing = absolute.as_path();
    let mut tail = Vec::new();
    while std::fs::symlink_metadata(existing).is_err() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent))
                if !matches!(
                    Path::new(name).components().next(),
                    Some(Component::ParentDir)
                ) =>
            {
                tail.push(name.to_owned());
                existing = parent;
            }
            _ => return Err("the path climbs out of a directory that does not exist".into()),
        }
    }
    let mut resolved = existing.canonicalize().map_err(|e| e.to_string())?;
    resolved.extend(tail.iter().rev());
    Ok(resolved)
}

impl Access {
    /// The resolved `path` if this access reaches it; otherwise the
    /// message of the error to raise. `what` is `read` or `write`.
    fn permit(&self, op: &str, what: &str, path: &str) -> Result<PathBuf, String> {
        match self {
            Access::None => Err(format!(
                "{op}: needs the file capability ({what} access), which this program was not \
                 given (--allow-{what})"
            )),
            Access::Any => resolve(Path::new(path)).map_err(|e| format!("{op}: {path}: {e}")),
            Access::Within(dirs) => {
                let resolved =
                    resolve(Path::new(path)).map_err(|e| format!("{op}: {path}: {e}"))?;
                let inside = dirs
                    .iter()
                    .any(|dir| resolve(dir).is_ok_and(|dir| resolved.starts_with(dir)));
                if inside {
                    Ok(resolved)
                } else {
                    Err(format!(
                        "{op}: {path}: outside the directories granted with --allow-{what}"
                    ))
                }
            }
        }
    }
}

/// `(vali-cli file)`: the file procedures, checked against the grants.
fn file_library(grants: &Grants) -> NativeLibrary {
    let (read, write) = (grants.read.clone(), grants.write.clone());
    let text_in = {
        let read = read.clone();
        move |name: String| -> Result<Port, String> {
            let path = read.permit("open-input-file", "read", &name)?;
            let text = std::fs::read(&path).map_err(|e| format!("open-input-file: {name}: {e}"))?;
            String::from_utf8(text)
                .map(Port::input_string)
                .map_err(|_| format!("open-input-file: {name}: not valid UTF-8 text"))
        }
    };
    let bytes_in = {
        let read = read.clone();
        move |name: String| -> Result<Port, String> {
            let path = read.permit("open-binary-input-file", "read", &name)?;
            std::fs::read(&path)
                .map(Port::input_bytes)
                .map_err(|e| format!("open-binary-input-file: {name}: {e}"))
        }
    };
    let open_out = |write: Access, op: &'static str, binary: bool| {
        move |name: String| -> Result<Port, String> {
            let path = write.permit(op, "write", &name)?;
            std::fs::File::create(&path)
                .map(|file| Port::output_file(file, binary))
                .map_err(|e| format!("{op}: {name}: {e}"))
        }
    };
    let exists = {
        let read = read.clone();
        move |name: String| -> Result<bool, String> {
            let path = read.permit("file-exists?", "read", &name)?;
            Ok(path.exists())
        }
    };
    let delete = {
        let write = write.clone();
        move |name: String| -> Result<(), String> {
            let path = write.permit("delete-file", "write", &name)?;
            std::fs::remove_file(&path).map_err(|e| format!("delete-file: {name}: {e}"))
        }
    };
    NativeLibrary::new(&["vali-cli", "file"])
        .function("open-input-file", text_in)
        .function("open-binary-input-file", bytes_in)
        .function(
            "open-output-file",
            open_out(write.clone(), "open-output-file", false),
        )
        .function(
            "open-binary-output-file",
            open_out(write, "open-binary-output-file", true),
        )
        .function("file-exists?", exists)
        .function("delete-file", delete)
}

/// `(vali-cli process)`: the command line and the environment.
fn process_library(grants: &Grants, command_line: Vec<String>) -> NativeLibrary {
    let denied = |op: &str| {
        format!(
            "{op}: needs the environment capability, which this program was not given \
             (--allow-env)"
        )
    };
    let env = grants.env.clone();
    let one = {
        let env = env.clone();
        move |name: String| -> Result<Option<String>, String> {
            if !env.allows(&name) {
                return Err(denied("get-environment-variable"));
            }
            Ok(std::env::var(&name).ok())
        }
    };
    let all = move || -> Result<Vec<Vec<String>>, String> {
        if env == EnvAccess::None {
            return Err(denied("get-environment-variables"));
        }
        Ok(std::env::vars()
            .filter(|(name, _)| env.allows(name))
            .map(|(name, value)| vec![name, value])
            .collect())
    };
    NativeLibrary::new(&["vali-cli", "process"])
        .function("command-line", move || command_line.clone())
        .function("get-environment-variable", one)
        .function("get-environment-variables", all)
}

/// `(vali-cli clock)`.
fn clock_library() -> NativeLibrary {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now);
    NativeLibrary::new(&["vali-cli", "clock"])
        .function("current-second", || -> f64 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64())
        })
        .function("current-jiffy", || -> i64 {
            START
                .get_or_init(Instant::now)
                .elapsed()
                .as_micros()
                .try_into()
                .unwrap_or(i64::MAX)
        })
}

/// The host's (scheme file), over `(vali-cli file)`.
const SCHEME_FILE: &str = "
(define-library (scheme file)
  (import (scheme base) (vali-cli file))
  (export call-with-input-file call-with-output-file delete-file file-exists?
          open-binary-input-file open-binary-output-file open-input-file
          open-output-file with-input-from-file with-output-to-file)
  (begin
    (define (call-with-port-closing port proc)
      (call-with-values (lambda () (proc port))
        (lambda results (close-port port) (apply values results))))
    (define (call-with-input-file name proc)
      (call-with-port-closing (open-input-file name) proc))
    (define (call-with-output-file name proc)
      (call-with-port-closing (open-output-file name) proc))
    (define (with-input-from-file name thunk)
      (call-with-port-closing (open-input-file name)
        (lambda (port) (parameterize ((current-input-port port)) (thunk)))))
    (define (with-output-to-file name thunk)
      (call-with-port-closing (open-output-file name)
        (lambda (port) (parameterize ((current-output-port port)) (thunk)))))))";

/// The host's (scheme process-context), over `(vali-cli process)`.
const SCHEME_PROCESS_CONTEXT: &str = "
(define-library (scheme process-context)
  (import (scheme base) (vali exit) (prefix (vali-cli process) %))
  (export command-line emergency-exit exit get-environment-variable
          get-environment-variables)
  (begin
    (define (command-line) (%command-line))
    (define (get-environment-variable name) (%get-environment-variable name))
    (define (get-environment-variables)
      (map (lambda (p) (cons (car p) (cadr p))) (%get-environment-variables)))))";

/// The host's (scheme time), over `(vali-cli clock)`.
const SCHEME_TIME: &str = "
(define-library (scheme time)
  (import (scheme base) (vali-cli clock))
  (export current-second current-jiffy jiffies-per-second)
  (begin
    (define (jiffies-per-second) 1000000)))";

/// What `vali` serves a program: libraries from a search path, and the
/// capabilities granted.
pub struct Host {
    path: Vec<PathBuf>,
    file: Arc<NativeLibrary>,
    process: Arc<NativeLibrary>,
    clock: Arc<NativeLibrary>,
    grants: Grants,
    /// For each library or program served from a file: the file and the
    /// directory its includes may read from.
    served: Mutex<HashMap<Identity, (PathBuf, PathBuf)>>,
}

impl Host {
    /// A host searching `path` for libraries, granting `grants`, whose
    /// programs see `command_line` as `(command-line)`.
    pub fn new(path: Vec<PathBuf>, grants: Grants, command_line: Vec<String>) -> Host {
        Host {
            path,
            file: Arc::new(file_library(&grants)),
            process: Arc::new(process_library(&grants, command_line)),
            clock: Arc::new(clock_library()),
            grants,
            served: Mutex::new(HashMap::new()),
        }
    }

    /// Records that the program `identity` is the file `file`, so what it
    /// includes is read from beside it.
    pub fn serve_program(&self, identity: &Identity, file: &Path) {
        let dir = file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if let (Ok(file), Ok(dir)) = (file.canonicalize(), dir.canonicalize()) {
            self.served
                .lock()
                .expect("not poisoned")
                .insert(identity.clone(), (file, dir));
        }
    }

    /// The file `name` is, relative to a root of the search path, if
    /// there is one: its path under the root, and the root.
    fn find(&self, name: &LibraryName) -> Option<(String, PathBuf, PathBuf)> {
        let mut relative = String::new();
        for part in &name.0 {
            let part = match part {
                NamePart::Symbol(s) => s.clone(),
                NamePart::Number(n) => n.to_string(),
            };
            // A name is not a way out of the root.
            if part.is_empty() || part == "." || part == ".." || part.contains(['/', '\\', '\0']) {
                return None;
            }
            if !relative.is_empty() {
                relative.push('/');
            }
            relative.push_str(&part);
        }
        relative.push_str(".sld");
        self.path.iter().find_map(|root| {
            let file = root.join(&relative);
            file.is_file()
                .then(|| (relative.clone(), file, root.clone()))
        })
    }
}

impl Loader for Host {
    fn load(
        &self,
        name: &LibraryName,
        _importer: Option<&Identity>,
    ) -> Result<Option<Source>, LoadError> {
        let is = |parts: &[&str]| *name == LibraryName::new(parts.iter().copied());
        let g = &self.grants;
        let served = if is(&["scheme", "file"]) && (g.read.granted() || g.write.granted()) {
            Some(Source::scheme(
                Identity::new("vali-cli:scheme/file"),
                SCHEME_FILE,
            ))
        } else if is(&["vali-cli", "file"]) && (g.read.granted() || g.write.granted()) {
            Some(Source::native(
                Identity::new("vali-cli:file"),
                Arc::clone(&self.file),
            ))
        } else if is(&["scheme", "process-context"]) {
            Some(Source::scheme(
                Identity::new("vali-cli:scheme/process-context"),
                SCHEME_PROCESS_CONTEXT,
            ))
        } else if is(&["vali-cli", "process"]) {
            Some(Source::native(
                Identity::new("vali-cli:process"),
                Arc::clone(&self.process),
            ))
        } else if is(&["scheme", "time"]) && g.clock {
            Some(Source::scheme(
                Identity::new("vali-cli:scheme/time"),
                SCHEME_TIME,
            ))
        } else if is(&["vali-cli", "clock"]) && g.clock {
            Some(Source::native(
                Identity::new("vali-cli:clock"),
                Arc::clone(&self.clock),
            ))
        } else {
            None
        };
        if served.is_some() {
            return Ok(served);
        }
        let Some((relative, file, root)) = self.find(name) else {
            return Ok(None);
        };
        let text = std::fs::read_to_string(&file).map_err(|e| LoadError {
            message: format!("{}: {e}", file.display()),
        })?;
        // By the path under its root, never the machine's path (ADR 0009
        // #2 and #11).
        let identity = Identity::new(&relative);
        if let (Ok(file), Ok(root)) = (file.canonicalize(), root.canonicalize()) {
            self.served
                .lock()
                .expect("not poisoned")
                .insert(identity.clone(), (file, root));
        }
        Ok(Some(Source::scheme(identity, text)))
    }

    /// Files beside the library or program that includes them, or below
    /// its search path root: nothing else, `..` and links included, since
    /// paths are compared once resolved (ADR 0009 #8).
    fn include(&self, path: &str, from: &Identity) -> Result<Option<String>, LoadError> {
        let served = self.served.lock().expect("not poisoned");
        let Some((file, root)) = served.get(from) else {
            return Ok(None);
        };
        let Some(dir) = file.parent() else {
            return Ok(None);
        };
        let Ok(resolved) = dir.join(path).canonicalize() else {
            return Ok(None);
        };
        if !resolved.starts_with(root) {
            return Ok(None);
        }
        std::fs::read_to_string(&resolved)
            .map(Some)
            .map_err(|e| LoadError {
                message: format!("{}: {e}", resolved.display()),
            })
    }
}
