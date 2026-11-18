//! Strings, characters, symbols' names and bytevectors (R7RS 6.6, 6.7,
//! 6.9; ADR 0002 #11).
//!
//! A string's text is UTF-8 in a bytevector its header points to, so a
//! change that alters how many bytes a character takes replaces the
//! bytevector; a change that does not is made in place. Literals and the
//! names `symbol->string` returns are immutable, and so are literal
//! bytevectors and vectors.

use vali_abi::{Header, Tag, TypeCode, data};
use vali_bytecode::Arity;

use crate::code::NativeFn;
use crate::cx::{Cx, get, put};
use crate::handle::Error;
use crate::page::OutOfMemory;
use crate::primitives::elements;
use crate::strategy::Strategy;
use crate::value::Value;
use crate::vm::wrong_type;

impl<S: Strategy> Cx<S> {
    /// The runtime data in `v`'s header, `v` being of type `t`.
    ///
    /// # Safety
    ///
    /// `v` must be a live object of type `t`, which has a header.
    pub(crate) unsafe fn header_flags(&self, v: Value, t: TypeCode) -> u32 {
        let offset = Self::LAYOUT.header(t).expect("a headered type");
        // SAFETY: passed on to the caller.
        Header(unsafe { get(v, offset) }).data()
    }

    /// Sets or clears `flag` in `v`'s header, leaving the strategy's half
    /// alone.
    ///
    /// # Safety
    ///
    /// As `header_flags`.
    unsafe fn set_header_flag(&mut self, v: Value, t: TypeCode, flag: u32, on: bool) {
        let offset = Self::LAYOUT.header(t).expect("a headered type");
        // SAFETY: passed on to the caller.
        unsafe {
            let header = Header(get(v, offset));
            let data = if on {
                header.data() | flag
            } else {
                header.data() & !flag
            };
            put(v, offset, header.with_data(data).expect("flags fit").0);
        }
    }

    /// An error saying `name` cannot change the immutable vector `v`.
    pub(crate) fn immutable_vector(&self, name: &str, v: Value) -> Error {
        Error::scheme(format!(
            "{name}: the vector {} is immutable",
            self.written(v)
        ))
    }

    /// A bytevector's bytes, to change.
    ///
    /// # Safety
    ///
    /// `v` must be a live, mutable bytevector, and nothing else may read or
    /// write its bytes while the slice is held.
    #[allow(clippy::mut_from_ref)]
    unsafe fn bytevector_bytes_mut(&self, v: Value) -> &mut [u8] {
        let length = Self::LAYOUT.length(TypeCode::Bytevector);
        // SAFETY: passed on to the caller; the bytes follow the length word.
        unsafe {
            let len = get(v, length) as usize;
            let start = v.raw().wrapping_add_signed(i64::from(length) + 8) as *mut u8;
            std::slice::from_raw_parts_mut(start, len)
        }
    }

    /// Replaces the text of `s`, a live mutable string: new bytes, and the
    /// counts and the ASCII flag that describe them.
    fn set_string_text(&mut self, s: Value, text: &str) -> Result<(), OutOfMemory> {
        let bytes = self.bytevector_raw(text.as_bytes())?;
        // SAFETY: `s` is a live string, and these are its fields.
        unsafe {
            self.write_field(s, Self::LAYOUT.string_bytes(), bytes);
            put(s, Self::LAYOUT.string_byte_length(), text.len() as u64);
            put(
                s,
                Self::LAYOUT.string_char_count(),
                text.chars().count() as u64,
            );
            self.write_field(s, Self::LAYOUT.string_hints(), Value::FALSE);
            self.set_header_flag(s, TypeCode::String, data::ASCII, text.is_ascii());
        }
        Ok(())
    }
}

fn string_arg<'a, S: Strategy>(cx: &'a Cx<S>, name: &str, v: Value) -> Result<&'a str, Error> {
    if v.tag() == Tag::String {
        // SAFETY: a live string, read before anything can change it.
        Ok(unsafe { cx.string_str(v) })
    } else {
        Err(wrong_type(cx, name, "a string", v))
    }
}

/// A string `name` changes: an error if it is immutable.
fn mutable_string<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<Value, Error> {
    string_arg(cx, name, v)?;
    // SAFETY: a live string.
    if unsafe { cx.header_flags(v, TypeCode::String) } & data::IMMUTABLE != 0 {
        return Err(Error::scheme(format!(
            "{name}: the string {} is immutable",
            cx.written(v)
        )));
    }
    Ok(v)
}

/// A vector `name` changes: an error if it is not a vector, or is immutable.
pub(crate) fn mutable_vector<S: Strategy>(
    cx: &Cx<S>,
    name: &str,
    v: Value,
) -> Result<Value, Error> {
    // SAFETY: a valid value.
    if unsafe { cx.header_type(v) } != Some(TypeCode::Vector) {
        return Err(wrong_type(cx, name, "a vector", v));
    }
    // SAFETY: a live vector.
    if unsafe { cx.header_flags(v, TypeCode::Vector) } & data::IMMUTABLE != 0 {
        return Err(cx.immutable_vector(name, v));
    }
    Ok(v)
}

fn char_arg<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<char, Error> {
    v.as_character()
        .ok_or_else(|| wrong_type(cx, name, "a character", v))
}

/// An index from 0 to `len`, inclusive.
fn index_arg<S: Strategy>(cx: &Cx<S>, name: &str, v: Value, len: usize) -> Result<usize, Error> {
    match v.as_fixnum() {
        Some(i) if (0..=len as i64).contains(&i) => Ok(i as usize),
        _ => Err(Error::scheme(format!(
            "{name}: index {} is out of range for length {len}",
            cx.written(v)
        ))),
    }
}

/// The optional start and end of a range, from `args[i]` on, into
/// something of length `len`.
fn range_args<S: Strategy>(
    cx: &Cx<S>,
    name: &str,
    args: &[Value],
    i: usize,
    len: usize,
) -> Result<(usize, usize), Error> {
    let given = |j: usize| args.get(j).copied().filter(|&v| v != Value::DEFAULT);
    let start = match given(i) {
        Some(v) => index_arg(cx, name, v, len)?,
        None => 0,
    };
    let end = match given(i + 1) {
        Some(v) => index_arg(cx, name, v, len)?,
        None => len,
    };
    if start > end {
        return Err(Error::scheme(format!(
            "{name}: start {start} is after end {end}"
        )));
    }
    Ok((start, end))
}

/// The byte offset of character `k` of `text`, which may be one past the
/// end.
fn byte_offset(text: &str, k: usize) -> usize {
    if text.is_ascii() {
        return k;
    }
    text.char_indices().nth(k).map_or(text.len(), |(i, _)| i)
}

/// Characters `start` to `end` of `text`.
fn chars_between(text: &str, start: usize, end: usize) -> &str {
    &text[byte_offset(text, start)..byte_offset(text, end)]
}

fn fixnum(n: usize) -> Value {
    Value::fixnum(n as i64).expect("lengths fit")
}

// Strings.

fn is_string<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0].tag() == Tag::String))
}

fn make_string<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let k = match args[0].as_fixnum() {
        Some(k) if k >= 0 => k as usize,
        _ => return Err(wrong_type(cx, "make-string", "a length", args[0])),
    };
    let fill = match args.get(1).copied().filter(|&v| v != Value::DEFAULT) {
        None => ' ',
        Some(v) => char_arg(cx, "make-string", v)?,
    };
    let text: String = std::iter::repeat_n(fill, k).collect();
    Ok(cx.string_raw(&text, false)?)
}

fn string<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let text = args
        .iter()
        .map(|&v| char_arg(cx, "string", v))
        .collect::<Result<String, _>>()?;
    Ok(cx.string_raw(&text, false)?)
}

fn string_length<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    string_arg(cx, "string-length", args[0])?;
    // SAFETY: a live string.
    Ok(fixnum(unsafe { cx.string_length(args[0]) }))
}

/// `string-ref`: a direct index into all-ASCII strings, a scan otherwise,
/// until strings carry index hints (ADR 0002 #11).
fn string_ref<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let text = string_arg(cx, "string-ref", args[0])?;
    // SAFETY: a live string.
    let len = unsafe { cx.string_length(args[0]) };
    let i = index_arg(cx, "string-ref", args[1], len)?;
    let c = if text.is_ascii() {
        text.as_bytes().get(i).map(|&b| char::from(b))
    } else {
        text.chars().nth(i)
    };
    c.map(Value::character).ok_or_else(|| {
        Error::scheme(format!(
            "string-ref: index {i} is out of range for length {len}"
        ))
    })
}

/// `string-set!`: in place when the new character takes as many bytes as
/// the old one, as it always does in an ASCII string; new bytes otherwise.
fn string_set<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let s = mutable_string(cx, "string-set!", args[0])?;
    let c = char_arg(cx, "string-set!", args[2])?;
    // SAFETY: a live string.
    let len = unsafe { cx.string_length(s) };
    let k = index_arg(cx, "string-set!", args[1], len)?;
    if k == len {
        return Err(Error::scheme(format!(
            "string-set!: index {k} is out of range for length {len}"
        )));
    }
    let text = string_arg(cx, "string-set!", s)?;
    let at = byte_offset(text, k);
    let old = text[at..]
        .chars()
        .next()
        .expect("an index inside the string");
    if old.len_utf8() == c.len_utf8() {
        // SAFETY: a live, mutable string owns its bytes, and `at` starts a
        // character as long as `c`'s encoding.
        unsafe {
            let bytes = Value::from_raw(get(s, Cx::<S>::LAYOUT.string_bytes()));
            c.encode_utf8(&mut cx.bytevector_bytes_mut(bytes)[at..at + c.len_utf8()]);
            if !c.is_ascii() {
                cx.set_header_flag(s, TypeCode::String, data::ASCII, false);
            }
        }
        return Ok(Value::UNSPECIFIED);
    }
    let mut changed = String::with_capacity(text.len() + 4);
    changed.push_str(&text[..at]);
    changed.push(c);
    changed.push_str(&text[at + old.len_utf8()..]);
    cx.set_string_text(s, &changed)?;
    Ok(Value::UNSPECIFIED)
}

/// `(string-fill! string char [start [end]])`.
fn string_fill<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let s = mutable_string(cx, "string-fill!", args[0])?;
    let c = char_arg(cx, "string-fill!", args[1])?;
    // SAFETY: a live string.
    let len = unsafe { cx.string_length(s) };
    let (start, end) = range_args(cx, "string-fill!", args, 2, len)?;
    let text = string_arg(cx, "string-fill!", s)?;
    let (a, b) = (byte_offset(text, start), byte_offset(text, end));
    let mut changed = String::with_capacity(text.len());
    changed.push_str(&text[..a]);
    changed.extend(std::iter::repeat_n(c, end - start));
    changed.push_str(&text[b..]);
    cx.set_string_text(s, &changed)?;
    Ok(Value::UNSPECIFIED)
}

/// `(string-copy! to at from [start [end]])`, safe when `to` and `from` are
/// the same string.
fn string_copy_into<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let to = mutable_string(cx, "string-copy!", args[0])?;
    let from = string_arg(cx, "string-copy!", args[2])?;
    // SAFETY: live strings.
    let (to_len, from_len) = unsafe { (cx.string_length(to), cx.string_length(args[2])) };
    let at = index_arg(cx, "string-copy!", args[1], to_len)?;
    let (start, end) = range_args(cx, "string-copy!", args, 3, from_len)?;
    if at + (end - start) > to_len {
        return Err(Error::scheme(format!(
            "string-copy!: {} characters do not fit at {at} in a string of length {to_len}",
            end - start
        )));
    }
    let piece = chars_between(from, start, end).to_owned();
    let text = string_arg(cx, "string-copy!", to)?;
    let (a, b) = (byte_offset(text, at), byte_offset(text, at + (end - start)));
    let changed = [&text[..a], &piece, &text[b..]].concat();
    cx.set_string_text(to, &changed)?;
    Ok(Value::UNSPECIFIED)
}

/// A chain of comparisons of `f`'s view of each string: `(string<? a b c)`
/// is `a < b` and `b < c`.
fn compare_strings<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    args: &[Value],
    view: fn(&str) -> String,
    ok: fn(std::cmp::Ordering) -> bool,
) -> Result<Value, Error> {
    let texts = args
        .iter()
        .map(|&v| string_arg(cx, name, v).map(view))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::boolean(
        texts.windows(2).all(|pair| ok(pair[0].cmp(&pair[1]))),
    ))
}

macro_rules! string_comparisons {
    ($($f:ident $name:literal $view:expr, $ok:expr;)*) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            compare_strings(cx, $name, args, $view, $ok)
        }
    )*};
}

string_comparisons! {
    string_eq "string=?" str::to_owned, |o| o.is_eq();
    string_lt "string<?" str::to_owned, |o| o.is_lt();
    string_ci_eq "string-ci=?" fold_string, |o| o.is_eq();
    string_ci_lt "string-ci<?" fold_string, |o| o.is_lt();
    string_ci_gt "string-ci>?" fold_string, |o| o.is_gt();
    string_ci_le "string-ci<=?" fold_string, |o| o.is_le();
    string_ci_ge "string-ci>=?" fold_string, |o| o.is_ge();
}

fn string_append<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut text = String::new();
    for &s in args {
        text.push_str(string_arg(cx, "string-append", s)?);
    }
    Ok(cx.string_raw(&text, false)?)
}

fn substring<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let text = string_arg(cx, "substring", args[0])?;
    // SAFETY: a live string.
    let len = unsafe { cx.string_length(args[0]) };
    let (start, end) = range_args(cx, "substring", args, 1, len)?;
    let part = chars_between(text, start, end).to_owned();
    Ok(cx.string_raw(&part, false)?)
}

fn string_to_list<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let chars: Vec<char> = string_arg(cx, "string->list", args[0])?.chars().collect();
    let mut list = Value::EMPTY_LIST;
    for c in chars.into_iter().rev() {
        // SAFETY: valid values.
        list = unsafe { cx.cons_raw(Value::character(c), list)? };
    }
    Ok(list)
}

fn list_to_string<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let text = elements(cx, "list->string", args[0])?
        .into_iter()
        .map(|v| char_arg(cx, "list->string", v))
        .collect::<Result<String, _>>()?;
    Ok(cx.string_raw(&text, false)?)
}

/// A string mapped through `f`.
fn map_string<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    args: &[Value],
    f: fn(&str) -> String,
) -> Result<Value, Error> {
    let text = f(string_arg(cx, name, args[0])?);
    Ok(cx.string_raw(&text, false)?)
}

fn string_upcase<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    map_string(cx, "string-upcase", args, str::to_uppercase)
}
fn string_downcase<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    map_string(cx, "string-downcase", args, str::to_lowercase)
}
fn string_foldcase<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    map_string(cx, "string-foldcase", args, fold_string)
}

/// `(string->utf8 string [start [end]])`.
fn string_to_utf8<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let text = string_arg(cx, "string->utf8", args[0])?;
    // SAFETY: a live string.
    let len = unsafe { cx.string_length(args[0]) };
    let (start, end) = range_args(cx, "string->utf8", args, 1, len)?;
    let bytes = chars_between(text, start, end).as_bytes().to_vec();
    Ok(cx.bytevector_raw(&bytes)?)
}

/// `(utf8->string bytevector [start [end]])`: an error for bytes that are
/// not UTF-8.
fn utf8_to_string<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let bytes = bytevector_arg(cx, "utf8->string", args[0])?;
    let (start, end) = range_args(cx, "utf8->string", args, 1, bytes.len())?;
    let text = std::str::from_utf8(&bytes[start..end])
        .map_err(|e| Error::scheme(format!("utf8->string: invalid UTF-8: {e}")))?
        .to_owned();
    Ok(cx.string_raw(&text, false)?)
}

// Symbols' names.

fn is_symbol<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0].tag() == Tag::Symbol))
}

fn symbol_to_string<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if args[0].tag() != Tag::Symbol {
        return Err(wrong_type(cx, "symbol->string", "a symbol", args[0]));
    }
    // SAFETY: a live symbol; its name is copied before allocating.
    let name = unsafe { cx.symbol_name(args[0]) }.to_owned();
    // The result must not be modified, so it shares nothing and is
    // immutable, as R7RS allows.
    Ok(cx.string_raw(&name, true)?)
}

fn string_to_symbol<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = string_arg(cx, "string->symbol", args[0])?.to_owned();
    Ok(cx.intern(&name)?)
}

// SRFI 88 keywords: interned like symbols, each a name without its colon.

impl<S: Strategy> Cx<S> {
    /// The keyword named `name`, its colon left out.
    pub(crate) fn keyword_raw(&mut self, name: &str) -> Result<Value, OutOfMemory> {
        if let Some(&v) = self.keywords.get(name) {
            return Ok(v);
        }
        let string = self.string_raw(name, true)?;
        let v = self.new_object(TypeCode::Keyword, 1, 0)?;
        // SAFETY: just allocated with its one field.
        unsafe { self.init_field(v, Self::LAYOUT.field(TypeCode::Keyword, 0), string) };
        self.hold(v);
        self.keywords.insert(name.into(), v);
        Ok(v)
    }

    /// A keyword's name, without its colon, if `v` is a keyword.
    pub(crate) fn keyword_name(&self, v: Value) -> Option<&str> {
        if !self.is_other(v, TypeCode::Keyword) {
            return None;
        }
        // SAFETY: a live keyword, whose one field is its name, a string.
        Some(unsafe {
            self.string_str(Value::from_raw(get(
                v,
                Self::LAYOUT.field(TypeCode::Keyword, 0),
            )))
        })
    }
}

fn is_keyword<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(cx.keyword_name(args[0]).is_some()))
}

fn keyword_to_string<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let Some(name) = cx.keyword_name(args[0]).map(str::to_owned) else {
        return Err(wrong_type(cx, "keyword->string", "a keyword", args[0]));
    };
    Ok(cx.string_raw(&name, true)?)
}

fn string_to_keyword<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let name = string_arg(cx, "string->keyword", args[0])?.to_owned();
    Ok(cx.keyword_raw(&name)?)
}

// Characters.

fn is_char<S: Strategy>(_cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(Value::boolean(args[0].as_character().is_some()))
}

fn char_to_integer<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let c = char_arg(cx, "char->integer", args[0])?;
    Ok(fixnum(u32::from(c) as usize))
}

fn integer_to_char<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    args[0]
        .as_fixnum()
        .and_then(|n| u32::try_from(n).ok())
        .and_then(char::from_u32)
        .map(Value::character)
        .ok_or_else(|| wrong_type(cx, "integer->char", "a Unicode scalar value", args[0]))
}

/// A chain of comparisons of `f`'s view of each character.
fn compare_chars<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    args: &[Value],
    view: fn(char) -> char,
    ok: fn(std::cmp::Ordering) -> bool,
) -> Result<Value, Error> {
    let chars = args
        .iter()
        .map(|&v| char_arg(cx, name, v).map(view))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::boolean(
        chars.windows(2).all(|pair| ok(pair[0].cmp(&pair[1]))),
    ))
}

fn same(c: char) -> char {
    c
}

macro_rules! char_comparisons {
    ($($f:ident $name:literal $view:expr, $ok:expr;)*) => {$(
        fn $f<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
            compare_chars(cx, $name, args, $view, $ok)
        }
    )*};
}

char_comparisons! {
    char_eq "char=?" same, |o| o.is_eq();
    char_lt "char<?" same, |o| o.is_lt();
    char_ci_eq "char-ci=?" fold_char, |o| o.is_eq();
    char_ci_lt "char-ci<?" fold_char, |o| o.is_lt();
    char_ci_gt "char-ci>?" fold_char, |o| o.is_gt();
    char_ci_le "char-ci<=?" fold_char, |o| o.is_le();
    char_ci_ge "char-ci>=?" fold_char, |o| o.is_ge();
}

/// A one-character case mapping, keeping the character when Unicode maps it
/// to several.
fn single(mut mapped: impl Iterator<Item = char>, c: char) -> char {
    match (mapped.next(), mapped.next()) {
        (Some(m), None) => m,
        _ => c,
    }
}

/// Simple case folding, as `char-foldcase` does it: the lowercase letter,
/// with final sigma folding to sigma. Close to Unicode's simple folding,
/// which differs only for a few scripts' characters.
fn fold_char(c: char) -> char {
    match c {
        'ς' => 'σ',
        _ => single(c.to_lowercase(), c),
    }
}

/// Full case folding, as `string-foldcase` does it: `ß` folds to `ss`.
fn fold_string(text: &str) -> String {
    text.chars()
        .flat_map(|c| match c {
            'ß' | 'ẞ' => "ss".chars().collect::<Vec<_>>(),
            'ς' => vec!['σ'],
            _ => c.to_lowercase().collect(),
        })
        .collect()
}

/// A one-character procedure from a character to a character.
fn char_map<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    args: &[Value],
    f: fn(char) -> char,
) -> Result<Value, Error> {
    Ok(Value::character(f(char_arg(cx, name, args[0])?)))
}

fn char_upcase<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    char_map(cx, "char-upcase", args, |c| single(c.to_uppercase(), c))
}
fn char_downcase<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    char_map(cx, "char-downcase", args, |c| single(c.to_lowercase(), c))
}
fn char_foldcase<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    char_map(cx, "char-foldcase", args, fold_char)
}

/// A predicate on characters.
fn char_test<S: Strategy>(
    cx: &mut Cx<S>,
    name: &str,
    args: &[Value],
    f: fn(char) -> bool,
) -> Result<Value, Error> {
    Ok(Value::boolean(f(char_arg(cx, name, args[0])?)))
}

fn char_alphabetic<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    char_test(cx, "char-alphabetic?", args, char::is_alphabetic)
}
fn char_numeric<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    char_test(cx, "char-numeric?", args, |c| digit_value(c).is_some())
}
fn char_whitespace<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    char_test(cx, "char-whitespace?", args, char::is_whitespace)
}
fn char_upper_case<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    char_test(cx, "char-upper-case?", args, char::is_uppercase)
}
fn char_lower_case<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    char_test(cx, "char-lower-case?", args, char::is_lowercase)
}

/// Where each run of ten Unicode decimal digits (general category Nd), 0
/// to 9, starts, from Unicode 15.1. Every Nd character is in one of these
/// runs.
const DIGIT_ZEROS: [u32; 68] = [
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0, 0x11650,
    0x116c0, 0x11730, 0x118e0, 0x11950, 0x11c50, 0x11d50, 0x11da0, 0x11f50, 0x16a60, 0x16ac0,
    0x16b50, 0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e950,
    0x1fbf0,
];

/// A decimal digit's value: R7RS's `digit-value`, and what makes a
/// character `char-numeric?`.
fn digit_value(c: char) -> Option<u32> {
    let code = u32::from(c);
    let i = DIGIT_ZEROS.partition_point(|&zero| zero <= code);
    let zero = *DIGIT_ZEROS.get(i.checked_sub(1)?)?;
    (code - zero < 10).then_some(code - zero)
}

fn digit_value_proc<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let c = char_arg(cx, "digit-value", args[0])?;
    Ok(digit_value(c).map_or(Value::FALSE, |d| fixnum(d as usize)))
}

// Bytevectors.

fn bytevector_arg<'a, S: Strategy>(cx: &'a Cx<S>, name: &str, v: Value) -> Result<&'a [u8], Error> {
    // SAFETY: a valid value.
    if unsafe { cx.header_type(v) } == Some(TypeCode::Bytevector) {
        // SAFETY: a live bytevector, read before anything can change it.
        Ok(unsafe { cx.bytevector_bytes(v) })
    } else {
        Err(wrong_type(cx, name, "a bytevector", v))
    }
}

/// A bytevector `name` changes: an error if it is immutable.
fn mutable_bytevector<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<Value, Error> {
    bytevector_arg(cx, name, v)?;
    // SAFETY: a live bytevector.
    if unsafe { cx.header_flags(v, TypeCode::Bytevector) } & data::IMMUTABLE != 0 {
        return Err(Error::scheme(format!(
            "{name}: the bytevector {} is immutable",
            cx.written(v)
        )));
    }
    Ok(v)
}

fn byte_arg<S: Strategy>(cx: &Cx<S>, name: &str, v: Value) -> Result<u8, Error> {
    v.as_fixnum()
        .and_then(|n| u8::try_from(n).ok())
        .ok_or_else(|| wrong_type(cx, name, "a byte", v))
}

fn is_bytevector<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: a valid value.
    Ok(Value::boolean(
        unsafe { cx.header_type(args[0]) } == Some(TypeCode::Bytevector),
    ))
}

fn make_bytevector<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let k = match args[0].as_fixnum() {
        Some(k) if k >= 0 => k as usize,
        _ => return Err(wrong_type(cx, "make-bytevector", "a length", args[0])),
    };
    let fill = match args.get(1).copied().filter(|&v| v != Value::DEFAULT) {
        None => 0,
        Some(v) => byte_arg(cx, "make-bytevector", v)?,
    };
    Ok(cx.bytevector_raw(&vec![fill; k])?)
}

fn bytevector<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let bytes = args
        .iter()
        .map(|&v| byte_arg(cx, "bytevector", v))
        .collect::<Result<Vec<u8>, _>>()?;
    Ok(cx.bytevector_raw(&bytes)?)
}

fn bytevector_length<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    Ok(fixnum(
        bytevector_arg(cx, "bytevector-length", args[0])?.len(),
    ))
}

fn bytevector_u8_ref<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let bytes = bytevector_arg(cx, "bytevector-u8-ref", args[0])?;
    let k = index_arg(cx, "bytevector-u8-ref", args[1], bytes.len())?;
    match bytes.get(k) {
        Some(&b) => Ok(fixnum(usize::from(b))),
        None => Err(Error::scheme(format!(
            "bytevector-u8-ref: index {k} is out of range for length {}",
            bytes.len()
        ))),
    }
}

fn bytevector_u8_set<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let v = mutable_bytevector(cx, "bytevector-u8-set!", args[0])?;
    let b = byte_arg(cx, "bytevector-u8-set!", args[2])?;
    let len = bytevector_arg(cx, "bytevector-u8-set!", v)?.len();
    let k = index_arg(cx, "bytevector-u8-set!", args[1], len)?;
    if k == len {
        return Err(Error::scheme(format!(
            "bytevector-u8-set!: index {k} is out of range for length {len}"
        )));
    }
    // SAFETY: a live, mutable bytevector; nothing else holds its bytes.
    unsafe { cx.bytevector_bytes_mut(v)[k] = b };
    Ok(Value::UNSPECIFIED)
}

/// `(bytevector-copy bytevector [start [end]])`.
fn bytevector_copy<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let bytes = bytevector_arg(cx, "bytevector-copy", args[0])?;
    let (start, end) = range_args(cx, "bytevector-copy", args, 1, bytes.len())?;
    let copy = bytes[start..end].to_vec();
    Ok(cx.bytevector_raw(&copy)?)
}

/// `(bytevector-copy! to at from [start [end]])`, safe when `to` and `from`
/// are the same bytevector.
fn bytevector_copy_into<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let to = mutable_bytevector(cx, "bytevector-copy!", args[0])?;
    let to_len = bytevector_arg(cx, "bytevector-copy!", to)?.len();
    let at = index_arg(cx, "bytevector-copy!", args[1], to_len)?;
    let from = bytevector_arg(cx, "bytevector-copy!", args[2])?;
    let (start, end) = range_args(cx, "bytevector-copy!", args, 3, from.len())?;
    if at + (end - start) > to_len {
        return Err(Error::scheme(format!(
            "bytevector-copy!: {} bytes do not fit at {at} in a bytevector of length {to_len}",
            end - start
        )));
    }
    let piece = from[start..end].to_vec();
    // SAFETY: a live, mutable bytevector; the source was copied out first.
    unsafe { cx.bytevector_bytes_mut(to)[at..at + piece.len()].copy_from_slice(&piece) };
    Ok(Value::UNSPECIFIED)
}

fn bytevector_append<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let mut bytes = Vec::new();
    for &v in args {
        bytes.extend_from_slice(bytevector_arg(cx, "bytevector-append", v)?);
    }
    Ok(cx.bytevector_raw(&bytes)?)
}

const fn arity(required: u8, optional: u8, rest: bool) -> Arity {
    Arity {
        required,
        optional,
        rest,
    }
}

/// The procedures on strings, characters and bytevectors implemented in
/// Rust.
pub(crate) fn table<S: Strategy>() -> Vec<(&'static str, Arity, NativeFn<S>)> {
    let exactly = Arity::exactly;
    vec![
        ("string?", exactly(1), is_string),
        ("make-string", arity(1, 1, false), make_string),
        ("string", arity(0, 0, true), string),
        ("string-length", exactly(1), string_length),
        ("string-ref", exactly(2), string_ref),
        ("string-set!", exactly(3), string_set),
        ("string-fill!", arity(2, 2, false), string_fill),
        ("string-copy!", arity(3, 2, false), string_copy_into),
        ("string=?", arity(1, 0, true), string_eq),
        ("string<?", arity(1, 0, true), string_lt),
        ("string-ci=?", arity(1, 0, true), string_ci_eq),
        ("string-ci<?", arity(1, 0, true), string_ci_lt),
        ("string-ci>?", arity(1, 0, true), string_ci_gt),
        ("string-ci<=?", arity(1, 0, true), string_ci_le),
        ("string-ci>=?", arity(1, 0, true), string_ci_ge),
        ("string-append", arity(0, 0, true), string_append),
        ("substring", exactly(3), substring),
        ("string->list", exactly(1), string_to_list),
        ("list->string", exactly(1), list_to_string),
        ("string-upcase", exactly(1), string_upcase),
        ("string-downcase", exactly(1), string_downcase),
        ("string-foldcase", exactly(1), string_foldcase),
        ("string->utf8", arity(1, 2, false), string_to_utf8),
        ("utf8->string", arity(1, 2, false), utf8_to_string),
        ("symbol?", exactly(1), is_symbol),
        ("symbol->string", exactly(1), symbol_to_string),
        ("string->symbol", exactly(1), string_to_symbol),
        ("keyword?", exactly(1), is_keyword),
        ("keyword->string", exactly(1), keyword_to_string),
        ("string->keyword", exactly(1), string_to_keyword),
        ("char?", exactly(1), is_char),
        ("char->integer", exactly(1), char_to_integer),
        ("integer->char", exactly(1), integer_to_char),
        ("char=?", arity(1, 0, true), char_eq),
        ("char<?", arity(1, 0, true), char_lt),
        ("char-ci=?", arity(1, 0, true), char_ci_eq),
        ("char-ci<?", arity(1, 0, true), char_ci_lt),
        ("char-ci>?", arity(1, 0, true), char_ci_gt),
        ("char-ci<=?", arity(1, 0, true), char_ci_le),
        ("char-ci>=?", arity(1, 0, true), char_ci_ge),
        ("char-upcase", exactly(1), char_upcase),
        ("char-downcase", exactly(1), char_downcase),
        ("char-foldcase", exactly(1), char_foldcase),
        ("char-alphabetic?", exactly(1), char_alphabetic),
        ("char-numeric?", exactly(1), char_numeric),
        ("char-whitespace?", exactly(1), char_whitespace),
        ("char-upper-case?", exactly(1), char_upper_case),
        ("char-lower-case?", exactly(1), char_lower_case),
        ("digit-value", exactly(1), digit_value_proc),
        ("bytevector?", exactly(1), is_bytevector),
        ("make-bytevector", arity(1, 1, false), make_bytevector),
        ("bytevector", arity(0, 0, true), bytevector),
        ("bytevector-length", exactly(1), bytevector_length),
        ("bytevector-u8-ref", exactly(2), bytevector_u8_ref),
        ("bytevector-u8-set!", exactly(3), bytevector_u8_set),
        ("bytevector-copy", arity(1, 2, false), bytevector_copy),
        ("bytevector-copy!", arity(3, 2, false), bytevector_copy_into),
        ("bytevector-append", arity(0, 0, true), bytevector_append),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_values() {
        assert_eq!(digit_value('0'), Some(0));
        assert_eq!(digit_value('9'), Some(9));
        assert_eq!(digit_value('٣'), Some(3));
        assert_eq!(digit_value('\u{1d7d9}'), Some(1));
        assert_eq!(digit_value('a'), None);
        assert_eq!(digit_value('\u{2f}'), None);
        assert_eq!(digit_value('\u{3a}'), None);
        assert_eq!(digit_value('Ⅻ'), None);
    }

    #[test]
    fn folding() {
        assert_eq!(fold_char('Σ'), 'σ');
        assert_eq!(fold_char('ς'), 'σ');
        assert_eq!(fold_string("Straße ΣΑΣ"), "strasse σασ");
    }
}
