//! Printing values: R7RS `write` and `display`.
//!
//! `write` prints strings, characters and symbols so that `read` gives them
//! back, and labels the pairs and vectors that close a cycle with datum
//! labels (`#0=` and `#0#`), so circular structure prints in finite space.
//! Shared structure that is not part of a cycle prints in full, as R7RS
//! requires of `write`. `display` prints strings and characters as their
//! text. Opaque objects print as `#<…>`, never with an address (ADR 0001).

use std::collections::HashMap;
use std::fmt::Write;

use vali_abi::{Tag, TypeCode};
use vali_num::{Number, Radix};

use crate::cx::Cx;
use crate::handle::{Error, Handle};
use crate::strategy::Strategy;
use crate::value::Value;

/// How strings and characters print.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Write,
    Display,
}

const CHARACTER_NAMES: &[(&str, char)] = &[
    ("alarm", '\u{7}'),
    ("backspace", '\u{8}'),
    ("delete", '\u{7f}'),
    ("escape", '\u{1b}'),
    ("newline", '\n'),
    ("null", '\0'),
    ("return", '\r'),
    ("space", ' '),
    ("tab", '\t'),
];

struct Printer<'a, S: Strategy> {
    cx: &'a Cx<S>,
    mode: Mode,
    /// Objects that close a cycle, and their label once printed.
    labels: HashMap<u64, Option<usize>>,
    next_label: usize,
    out: String,
}

impl<S: Strategy> Printer<'_, S> {
    fn is_vector(&self, v: Value) -> bool {
        // SAFETY: values reachable from a live value are valid.
        v.tag() == Tag::Other
            && unsafe { self.cx.header(v, TypeCode::Vector) }.and_then(vali_abi::Header::type_code)
                == Some(TypeCode::Vector)
    }

    /// The pairs and vectors reachable from `v`, each followed once.
    fn children(&self, v: Value) -> Vec<Value> {
        // SAFETY: `v` is a live pair or vector.
        unsafe {
            if v.is_pair() {
                vec![self.cx.car(v), self.cx.cdr(v)]
            } else if self.is_vector(v) {
                (0..self.cx.vector_length(v))
                    .map(|i| self.cx.vector_ref(v, i))
                    .collect()
            } else {
                Vec::new()
            }
        }
    }

    /// Finds the objects that close a cycle: those reached again while
    /// still being visited. Iterative, so long lists don't overflow the
    /// Rust stack.
    fn find_cycles(&mut self, root: Value) {
        #[derive(PartialEq)]
        enum State {
            Visiting,
            Done,
        }
        let mut state: HashMap<u64, State> = HashMap::new();
        // Each entry: an object, and whether its children were pushed.
        let mut stack = vec![(root, false)];
        while let Some((v, expanded)) = stack.pop() {
            if !(v.is_pair() || self.is_vector(v)) {
                continue;
            }
            if expanded {
                state.insert(v.raw(), State::Done);
                continue;
            }
            match state.get(&v.raw()) {
                Some(State::Visiting) => {
                    self.labels.insert(v.raw(), None);
                    continue;
                }
                Some(State::Done) => continue,
                None => {}
            }
            state.insert(v.raw(), State::Visiting);
            stack.push((v, true));
            for child in self.children(v).into_iter().rev() {
                stack.push((child, false));
            }
        }
    }

    /// Prints a label definition or reference for `v`, if it has one.
    /// Returns whether `v` was already printed.
    fn label(&mut self, v: Value) -> bool {
        match self.labels.get(&v.raw()) {
            None => false,
            Some(Some(n)) => {
                let _ = write!(self.out, "#{n}#");
                true
            }
            Some(None) => {
                let n = self.next_label;
                self.next_label += 1;
                self.labels.insert(v.raw(), Some(n));
                let _ = write!(self.out, "#{n}=");
                false
            }
        }
    }

    fn print(&mut self, v: Value) {
        if v.is_pair() {
            if self.label(v) {
                return;
            }
            self.out.push('(');
            let mut v = v;
            loop {
                // SAFETY: `v` is a live pair.
                let (car, cdr) = unsafe { (self.cx.car(v), self.cx.cdr(v)) };
                self.print(car);
                if cdr == Value::EMPTY_LIST {
                    break;
                }
                // A labelled pair in the tail must print as a dotted pair, so
                // its label has somewhere to go.
                if cdr.is_pair() && !self.labels.contains_key(&cdr.raw()) {
                    self.out.push(' ');
                    v = cdr;
                    continue;
                }
                self.out.push_str(" . ");
                self.print(cdr);
                break;
            }
            self.out.push(')');
            return;
        }
        if self.is_vector(v) {
            if self.label(v) {
                return;
            }
            self.out.push_str("#(");
            for (i, item) in self.children(v).into_iter().enumerate() {
                if i > 0 {
                    self.out.push(' ');
                }
                self.print(item);
            }
            self.out.push(')');
            return;
        }
        self.atom(v);
    }

    fn atom(&mut self, v: Value) {
        if let Some(n) = v.as_fixnum() {
            let _ = write!(self.out, "{n}");
            return;
        }
        if let Some(b) = v.as_boolean() {
            self.out.push_str(if b { "#t" } else { "#f" });
            return;
        }
        if let Some(c) = v.as_character() {
            self.character(c);
            return;
        }
        match v {
            Value::EMPTY_LIST => return self.out.push_str("()"),
            Value::EOF => return self.out.push_str("#<eof>"),
            Value::UNSPECIFIED => return self.out.push_str("#<unspecified>"),
            Value::DEFAULT => return self.out.push_str("#<default>"),
            _ => {}
        }
        // SAFETY: a valid value with a pointer tag points to a live object
        // of that tag's type.
        unsafe {
            match v.tag() {
                Tag::Flonum => {
                    let _ = write!(self.out, "{}", Number::Real(self.cx.flonum_value(v)));
                }
                Tag::String => {
                    let text = self.cx.string_str(v).to_owned();
                    self.string(&text);
                }
                Tag::Symbol => {
                    let name = self.cx.symbol_name(v).to_owned();
                    self.symbol(&name);
                }
                Tag::Closure => match self.cx.procedure_name(v) {
                    Some(name) => {
                        let _ = write!(self.out, "#<procedure {name}>");
                    }
                    None => self.out.push_str("#<procedure>"),
                },
                Tag::Other => {
                    let header = self.cx.header(v, TypeCode::Record);
                    let type_code = header.and_then(vali_abi::Header::type_code);
                    match type_code {
                        Some(TypeCode::Bignum) => {
                            let n = self.cx.bignum_value(v);
                            self.out.push_str(&n.to_string_radix(Radix::Decimal));
                        }
                        Some(TypeCode::Box) => self.out.push_str("#<box>"),
                        Some(TypeCode::Bytevector) => {
                            self.out.push_str("#u8(");
                            for (i, b) in self.cx.bytevector_bytes(v).iter().enumerate() {
                                if i > 0 {
                                    self.out.push(' ');
                                }
                                let _ = write!(self.out, "{b}");
                            }
                            self.out.push(')');
                        }
                        Some(t) => {
                            let _ = write!(self.out, "#<{t:?}>");
                        }
                        None => self.out.push_str("#<object>"),
                    }
                }
                _ => {
                    let _ = write!(self.out, "{v:?}");
                }
            }
        }
    }

    fn character(&mut self, c: char) {
        if self.mode == Mode::Display {
            self.out.push(c);
            return;
        }
        self.out.push_str("#\\");
        match CHARACTER_NAMES.iter().find(|(_, named)| *named == c) {
            Some((name, _)) => self.out.push_str(name),
            None if c.is_control() => {
                let _ = write!(self.out, "x{:x}", c as u32);
            }
            None => self.out.push(c),
        }
    }

    fn string(&mut self, text: &str) {
        if self.mode == Mode::Display {
            self.out.push_str(text);
            return;
        }
        self.out.push('"');
        for c in text.chars() {
            match c {
                '"' => self.out.push_str("\\\""),
                '\\' => self.out.push_str("\\\\"),
                '\n' => self.out.push_str("\\n"),
                '\t' => self.out.push_str("\\t"),
                '\r' => self.out.push_str("\\r"),
                c if c.is_control() => {
                    let _ = write!(self.out, "\\x{:x};", c as u32);
                }
                c => self.out.push(c),
            }
        }
        self.out.push('"');
    }

    fn symbol(&mut self, name: &str) {
        if self.mode == Mode::Display || vali_syntax::is_identifier(name) {
            self.out.push_str(name);
            return;
        }
        self.out.push('|');
        for c in name.chars() {
            match c {
                '|' => self.out.push_str("\\|"),
                '\\' => self.out.push_str("\\\\"),
                c if c.is_control() => {
                    let _ = write!(self.out, "\\x{:x};", c as u32);
                }
                c => self.out.push(c),
            }
        }
        self.out.push('|');
    }
}

impl<S: Strategy> Cx<S> {
    fn print(&self, v: Value, mode: Mode) -> String {
        let mut printer = Printer {
            cx: self,
            mode,
            labels: HashMap::new(),
            next_label: 0,
            out: String::new(),
        };
        printer.find_cycles(v);
        printer.print(v);
        printer.out
    }

    /// `v` as R7RS `write` prints it.
    pub fn write_string(&self, v: &Handle) -> Result<String, Error> {
        Ok(self.print(self.get(v)?, Mode::Write))
    }

    /// `v` as R7RS `display` prints it.
    pub fn display_string(&self, v: &Handle) -> Result<String, Error> {
        Ok(self.print(self.get(v)?, Mode::Display))
    }
}
