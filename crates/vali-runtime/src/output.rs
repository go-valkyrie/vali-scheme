//! Output for `(scheme write)`: `display`, `write` and `newline` write into
//! the isolate's output buffer, which the host takes with
//! [`Cx::take_output`]. Writing is not ambient authority (ADR 0009); ports
//! come later. An optional port argument is accepted and ignored.

use crate::cx::Cx;
use crate::handle::Error;
use crate::strategy::Strategy;
use crate::value::Value;
use crate::vm::type_error;

impl<S: Strategy> Cx<S> {
    /// What has been written since the host last took it.
    pub fn take_output(&mut self) -> String {
        std::mem::take(&mut self.output)
    }
}

pub(crate) fn display<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: a valid value.
    let v = unsafe { cx.root(args[0]) };
    let text = cx.display_string(&v)?;
    cx.output.push_str(&text);
    Ok(Value::UNSPECIFIED)
}

pub(crate) fn write<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    // SAFETY: a valid value.
    let v = unsafe { cx.root(args[0]) };
    let text = cx.write_string(&v)?;
    cx.output.push_str(&text);
    Ok(Value::UNSPECIFIED)
}

pub(crate) fn newline<S: Strategy>(cx: &mut Cx<S>, _args: &[Value]) -> Result<Value, Error> {
    cx.output.push('\n');
    Ok(Value::UNSPECIFIED)
}

pub(crate) fn write_string<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    if args[0].tag() != vali_abi::Tag::String {
        return Err(type_error("write-string", "a string", args[0]));
    }
    // SAFETY: a live string.
    let text = unsafe { cx.string_str(args[0]) }.to_owned();
    cx.output.push_str(&text);
    Ok(Value::UNSPECIFIED)
}

pub(crate) fn write_char<S: Strategy>(cx: &mut Cx<S>, args: &[Value]) -> Result<Value, Error> {
    let Some(c) = args[0].as_character() else {
        return Err(type_error("write-char", "a character", args[0]));
    };
    cx.output.push(c);
    Ok(Value::UNSPECIFIED)
}
