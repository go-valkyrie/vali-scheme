//! Ports (R7RS 6.13), Vali's first Rust objects (ADR 0016 #10).
//!
//! A [`Port`] is a Rust value Scheme holds: textual or binary, for input or
//! output, over a string or a bytevector. Its procedures are bridges in the
//! native library `(vali ports)`, which `(scheme base)` wraps to supply the
//! current ports. `write`, `display` and `read` need the printer and the
//! reader, so they are primitives here that take a port.
//!
//! Each isolate has console ports: what is written to its output and error
//! ports waits there until the host takes it, and its input port reads what
//! the host gives it. Files are not ambient authority (ADR 0009): file ports
//! come with capabilities.

use std::sync::{Arc, OnceLock};

use crate::RustObject;
use crate::bridge::{Bytes, Context, Eof, IntoScheme, NativeLibrary};
use crate::cx::Cx;
use crate::handle::{Error, Handle};
use crate::rust_objects::Borrow;
use crate::strategy::Strategy;
use crate::value::Value;

/// A port: textual or binary, for input or output.
#[derive(Debug)]
pub struct Port {
    input: bool,
    open: bool,
    data: Data,
}

#[derive(Debug)]
enum Data {
    /// Text, read from `at`, a byte offset, or written at the end.
    Text { text: String, at: usize },
    /// Bytes, read from `at` or written at the end.
    Binary { bytes: Vec<u8>, at: usize },
}

impl RustObject for Port {
    const NAME: &'static str = "port";

    fn write(&self, out: &mut String) {
        let direction = if self.input { "input" } else { "output" };
        let kind = match self.data {
            Data::Text { .. } => "textual",
            Data::Binary { .. } => "binary",
        };
        out.push_str(&format!("#<{kind} {direction} port>"));
    }
}

/// What a port operation could not do.
type Outcome<T> = Result<T, String>;

/// A value read, or the end of the input.
#[derive(Debug, PartialEq, Eq)]
pub enum Read<T> {
    /// A value.
    Value(T),
    /// The end.
    Eof,
}

// SAFETY: it returns an immediate, or a value `cx` makes in the call.
unsafe impl<T: IntoScheme> IntoScheme for Read<T> {
    fn into_scheme(self, cx: &mut dyn Context) -> Result<Value, Error> {
        match self {
            Read::Value(x) => x.into_scheme(cx),
            Read::Eof => Eof.into_scheme(cx),
        }
    }
}

impl Port {
    /// A textual input port reading `text`.
    pub fn input_string(text: impl Into<String>) -> Port {
        Port {
            input: true,
            open: true,
            data: Data::Text {
                text: text.into(),
                at: 0,
            },
        }
    }

    /// A textual output port gathering what is written.
    pub fn output_string() -> Port {
        Port {
            input: false,
            open: true,
            data: Data::Text {
                text: String::new(),
                at: 0,
            },
        }
    }

    /// A binary input port reading `bytes`.
    pub fn input_bytes(bytes: Vec<u8>) -> Port {
        Port {
            input: true,
            open: true,
            data: Data::Binary { bytes, at: 0 },
        }
    }

    /// A binary output port gathering what is written.
    pub fn output_bytes() -> Port {
        Port {
            input: false,
            open: true,
            data: Data::Binary {
                bytes: Vec::new(),
                at: 0,
            },
        }
    }

    fn is_input(&self) -> bool {
        self.input
    }

    fn is_output(&self) -> bool {
        !self.input
    }

    fn is_textual(&self) -> bool {
        matches!(self.data, Data::Text { .. })
    }

    fn is_binary(&self) -> bool {
        matches!(self.data, Data::Binary { .. })
    }

    fn is_input_open(&self) -> bool {
        self.input && self.open
    }

    fn is_output_open(&self) -> bool {
        !self.input && self.open
    }

    fn close(&mut self) {
        self.open = false;
    }

    fn close_input(&mut self) {
        if self.input {
            self.open = false;
        }
    }

    fn close_output(&mut self) {
        if !self.input {
            self.open = false;
        }
    }

    /// The unread text, and where it starts.
    fn text_in(&mut self) -> Outcome<(&str, &mut usize)> {
        match (&mut self.data, self.input && self.open) {
            (Data::Text { text, at }, true) => Ok((&text[*at..], at)),
            _ => Err("expected an open textual input port".into()),
        }
    }

    fn text_out(&mut self) -> Outcome<&mut String> {
        match (&mut self.data, !self.input && self.open) {
            (Data::Text { text, .. }, true) => Ok(text),
            _ => Err("expected an open textual output port".into()),
        }
    }

    fn bytes_in(&mut self) -> Outcome<(&[u8], &mut usize)> {
        match (&mut self.data, self.input && self.open) {
            (Data::Binary { bytes, at }, true) => Ok((&bytes[*at..], at)),
            _ => Err("expected an open binary input port".into()),
        }
    }

    fn bytes_out(&mut self) -> Outcome<&mut Vec<u8>> {
        match (&mut self.data, !self.input && self.open) {
            (Data::Binary { bytes, .. }, true) => Ok(bytes),
            _ => Err("expected an open binary output port".into()),
        }
    }

    fn read_char(&mut self) -> Outcome<Read<char>> {
        let (rest, at) = self.text_in()?;
        Ok(match rest.chars().next() {
            Some(c) => {
                *at += c.len_utf8();
                Read::Value(c)
            }
            None => Read::Eof,
        })
    }

    fn peek_char(&mut self) -> Outcome<Read<char>> {
        let (rest, _) = self.text_in()?;
        Ok(rest.chars().next().map_or(Read::Eof, Read::Value))
    }

    /// A line, without its end: a newline, a return, or both.
    fn read_line(&mut self) -> Outcome<Read<String>> {
        let (rest, at) = self.text_in()?;
        if rest.is_empty() {
            return Ok(Read::Eof);
        }
        let end = rest.find(['\n', '\r']).unwrap_or(rest.len());
        let line = rest[..end].to_owned();
        let skip = if rest[end..].starts_with("\r\n") {
            2
        } else if end < rest.len() {
            1
        } else {
            0
        };
        *at += end + skip;
        Ok(Read::Value(line))
    }

    fn read_string(&mut self, k: usize) -> Outcome<Read<String>> {
        let (rest, at) = self.text_in()?;
        if rest.is_empty() && k > 0 {
            return Ok(Read::Eof);
        }
        let end = rest.char_indices().nth(k).map_or(rest.len(), |(i, _)| i);
        let text = rest[..end].to_owned();
        *at += end;
        Ok(Read::Value(text))
    }

    fn char_ready(&mut self) -> Outcome<bool> {
        self.text_in().map(|_| true)
    }

    fn read_u8(&mut self) -> Outcome<Read<u8>> {
        let (rest, at) = self.bytes_in()?;
        Ok(match rest.first() {
            Some(&b) => {
                *at += 1;
                Read::Value(b)
            }
            None => Read::Eof,
        })
    }

    fn peek_u8(&mut self) -> Outcome<Read<u8>> {
        let (rest, _) = self.bytes_in()?;
        Ok(rest.first().map_or(Read::Eof, |&b| Read::Value(b)))
    }

    fn u8_ready(&mut self) -> Outcome<bool> {
        self.bytes_in().map(|_| true)
    }

    fn read_bytevector(&mut self, k: usize) -> Outcome<Read<Bytes>> {
        let (rest, at) = self.bytes_in()?;
        if rest.is_empty() && k > 0 {
            return Ok(Read::Eof);
        }
        let n = k.min(rest.len());
        let bytes = rest[..n].to_vec();
        *at += n;
        Ok(Read::Value(Bytes(bytes)))
    }

    fn write_char(&mut self, c: char) -> Outcome<()> {
        self.text_out()?.push(c);
        Ok(())
    }

    fn write_string(&mut self, s: String) -> Outcome<()> {
        self.text_out()?.push_str(&s);
        Ok(())
    }

    fn write_u8(&mut self, b: u8) -> Outcome<()> {
        self.bytes_out()?.push(b);
        Ok(())
    }

    fn write_bytevector(&mut self, b: Bytes) -> Outcome<()> {
        self.bytes_out()?.extend_from_slice(&b.0);
        Ok(())
    }

    fn flush(&mut self) -> Outcome<()> {
        if self.is_output_open() {
            Ok(())
        } else {
            Err("expected an open output port".into())
        }
    }

    /// What a string output port has gathered.
    fn output_text(&self) -> Outcome<String> {
        match (&self.data, self.input) {
            (Data::Text { text, .. }, false) => Ok(text.clone()),
            _ => Err("expected a string output port".into()),
        }
    }

    /// What a bytevector output port has gathered.
    fn output_bytes_so_far(&self) -> Outcome<Bytes> {
        match (&self.data, self.input) {
            (Data::Binary { bytes, .. }, false) => Ok(Bytes(bytes.clone())),
            _ => Err("expected a bytevector output port".into()),
        }
    }
}

/// The identity `(vali ports)` is registered under.
pub(crate) const PORTS_IDENTITY: &str = "vali:vali/ports";

/// `(vali ports)`: the port procedures, which `(scheme base)` wraps.
pub fn library() -> Arc<NativeLibrary> {
    static LIBRARY: OnceLock<Arc<NativeLibrary>> = OnceLock::new();
    Arc::clone(LIBRARY.get_or_init(|| {
        Arc::new(
            NativeLibrary::new(&["vali", "ports"])
                .predicate::<Port>("port?")
                .method("input-port?", Port::is_input)
                .method("output-port?", Port::is_output)
                .method("textual-port?", Port::is_textual)
                .method("binary-port?", Port::is_binary)
                .method("input-port-open?", Port::is_input_open)
                .method("output-port-open?", Port::is_output_open)
                .method("close-port", Port::close)
                .method("close-input-port", Port::close_input)
                .method("close-output-port", Port::close_output)
                .function("open-input-string", |s: String| Port::input_string(s))
                .function("open-output-string", Port::output_string)
                .method("get-output-string", Port::output_text)
                .function("open-input-bytevector", |b: Bytes| Port::input_bytes(b.0))
                .function("open-output-bytevector", Port::output_bytes)
                .method("get-output-bytevector", Port::output_bytes_so_far)
                .method("read-char", Port::read_char)
                .method("peek-char", Port::peek_char)
                .method("read-line", Port::read_line)
                .method("read-string", Port::read_string)
                .method("char-ready?", Port::char_ready)
                .method("read-u8", Port::read_u8)
                .method("peek-u8", Port::peek_u8)
                .method("u8-ready?", Port::u8_ready)
                .method("read-bytevector", Port::read_bytevector)
                .method("write-char", Port::write_char)
                .method("write-string", Port::write_string)
                .method("write-u8", Port::write_u8)
                .method("write-bytevector", Port::write_bytevector)
                .method("flush-output-port", Port::flush)
                // String ports do nothing outside the program.
                .all_expansion_safe(1),
        )
    }))
}

/// Which console port.
#[derive(Clone, Copy)]
enum Console {
    Input,
    Output,
    Error,
}

impl<S: Strategy> Cx<S> {
    /// The console port `which`, made when first needed.
    fn console(&mut self, which: Console) -> Result<Handle, Error> {
        let slot = match which {
            Console::Input => &self.console_input,
            Console::Output => &self.console_output,
            Console::Error => &self.console_error,
        };
        if let Some(h) = slot {
            return Ok(h.clone());
        }
        let port = match which {
            Console::Input => Port::input_string(""),
            Console::Output | Console::Error => Port::output_string(),
        };
        let h = self.object(port)?;
        match which {
            Console::Input => self.console_input = Some(h.clone()),
            Console::Output => self.console_output = Some(h.clone()),
            Console::Error => self.console_error = Some(h.clone()),
        }
        Ok(h)
    }

    /// Takes what has been written to a console output port.
    fn take_console(&mut self, which: Console) -> String {
        match self.console(which) {
            Ok(h) => self
                .with_object(&h, |p: &mut Port| match &mut p.data {
                    Data::Text { text, .. } => std::mem::take(text),
                    Data::Binary { .. } => String::new(),
                })
                .unwrap_or_default(),
            Err(_) => String::new(),
        }
    }

    /// What has been written to the console output port, the default
    /// current output port, since the host last took it.
    pub fn take_output(&mut self) -> String {
        self.take_console(Console::Output)
    }

    /// What has been written to the console error port since the host last
    /// took it.
    pub fn take_error_output(&mut self) -> String {
        self.take_console(Console::Error)
    }

    /// Gives the console input port, the default current input port, `text`
    /// to read, after what it has not read yet.
    pub fn give_input(&mut self, text: &str) -> Result<(), Error> {
        let h = self.console(Console::Input)?;
        self.with_object(&h, |p: &mut Port| {
            if let Data::Text { text: buffer, at } = &mut p.data {
                buffer.drain(..*at);
                *at = 0;
                buffer.push_str(text);
            }
        })
    }

    /// The port `v`, borrowed exclusively for `f`.
    fn with_port<R>(
        &mut self,
        name: &str,
        v: Value,
        f: impl FnOnce(&mut Port) -> R,
    ) -> Result<R, Error> {
        let entry = Self::rust_object_index(v).and_then(|i| self.objects.entry_mut(i));
        match entry {
            Some(entry) if entry.borrow != Borrow::Free => {
                Err(Error::scheme(format!("{name}: port is already borrowed")))
            }
            Some(entry) => match entry.value.as_any_mut().downcast_mut::<Port>() {
                Some(port) => Ok(f(port)),
                None => Err(crate::vm::wrong_type(self, name, "a port", v)),
            },
            None => Err(crate::vm::wrong_type(self, name, "a port", v)),
        }
    }
}

/// `(%console-input)`, `(%console-output)` and `(%console-error)`: the
/// isolate's console ports, the current ports' defaults.
pub(crate) fn console_input<S: Strategy>(cx: &mut Cx<S>, _args: &[Value]) -> Result<Value, Error> {
    let h = cx.console(Console::Input)?;
    cx.get(&h)
}

pub(crate) fn console_output<S: Strategy>(cx: &mut Cx<S>, _args: &[Value]) -> Result<Value, Error> {
    let h = cx.console(Console::Output)?;
    cx.get(&h)
}

pub(crate) fn console_error<S: Strategy>(cx: &mut Cx<S>, _args: &[Value]) -> Result<Value, Error> {
    let h = cx.console(Console::Error)?;
    cx.get(&h)
}

/// How `%write-to` prints: R7RS's four printing procedures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    Display,
    Write,
    WriteShared,
    WriteSimple,
}

/// `(%write-to obj port style)`: `display` (0), `write` (1),
/// `write-shared` (2) or `write-simple` (3) to a textual output port.
pub(crate) fn write_to<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let style = match args[2].as_fixnum() {
        Some(0) => Style::Display,
        Some(1) => Style::Write,
        Some(2) => Style::WriteShared,
        _ => Style::WriteSimple,
    };
    let name = match style {
        Style::Display => "display",
        Style::Write => "write",
        Style::WriteShared => "write-shared",
        Style::WriteSimple => "write-simple",
    };
    let text = cx.print_styled(args[0], style);
    cx.with_port(name, args[1], |p| {
        p.text_out().map(|out| out.push_str(&text))
    })?
    .map_err(|e| Error::scheme(format!("{name}: {e}")))?;
    Ok(Value::UNSPECIFIED)
}

/// `(%read port)`: the next datum from a textual input port, or the
/// end-of-file object.
pub(crate) fn read<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let rest = cx
        .with_port("read", args[0], |p| {
            p.text_in().map(|(rest, _)| rest.to_owned())
        })?
        .map_err(|e| Error::scheme(format!("read: {e}")))?;
    let parsed = vali_syntax::read(&rest, vali_syntax::SourceId(0));
    let Some(datum) = parsed.datums.first() else {
        if let Some(e) = parsed.errors.first() {
            return Err(Error::scheme(format!("read: {}", e.kind)));
        }
        cx.with_port("read", args[0], |p| {
            if let Ok((rest, at)) = p.text_in() {
                *at += rest.len();
            }
        })?;
        return Ok(Value::EOF);
    };
    let end = usize::from(datum.span.range.end());
    if let Some(e) = parsed
        .errors
        .iter()
        .find(|e| usize::from(e.range.start()) < end)
    {
        return Err(Error::scheme(format!("read: {}", e.kind)));
    }
    let datum = datum.clone();
    cx.with_port("read", args[0], |p| {
        if let Ok((_, at)) = p.text_in() {
            *at += end;
        }
    })?;
    cx.datum_raw(&datum)
}

/// `(eof-object)`.
pub(crate) fn eof_object<S: Strategy>(_cx: &mut Cx<S>, _args: &[Value]) -> Result<Value, Error> {
    Ok(Value::EOF)
}

/// `(eof-object? obj)`.
pub(crate) fn is_eof_object<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0] == Value::EOF))
}
