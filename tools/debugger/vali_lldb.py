"""Vali's LLDB extension: Scheme backtraces, frames and values.

Load it with `command script import tools/debugger/vali_lldb.py`, or from
.lldbinit. It adds three commands, which work whether or not vali-runtime
was built with native frames, as long as the build keeps the interpreter's
locals in its debug information:

    vali-bt          the Scheme frames, innermost first
    vali-frame N     Scheme frame N's procedure and slots, as values
    vali-print EXPR  EXPR, a Vali value or raw word, as Scheme writes it

and a summary for `vali_runtime::value::Value`.
"""

import os
import sys

import lldb

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vali_debug  # noqa: E402


class LldbTarget(vali_debug.Target):
    def __init__(self, process):
        self.process = process

    def read(self, address, size):
        error = lldb.SBError()
        data = self.process.ReadMemory(address, size, error)
        if not error.Success():
            raise MemoryError(error.GetCString())
        return data


def interpreter(thread):
    """The innermost interpreter frame's `fp` and `pc`, from DWARF."""
    for frame in thread:
        name = frame.GetFunctionName() or ""
        if "::interpret" in name and "vali_runtime" in name:
            fp, pc = frame.FindVariable("fp"), frame.FindVariable("pc")
            if not (fp.IsValid() and pc.IsValid()):
                raise RuntimeError(
                    "the interpreter's fp and pc were optimised out: "
                    "build with full debug information")
            return fp.GetValueAsUnsigned(), pc.GetValueAsUnsigned()
    raise RuntimeError("no Vali interpreter frame on this thread")


def run(debugger, result, f):
    process = debugger.GetSelectedTarget().GetProcess()
    try:
        fp, pc = interpreter(process.GetSelectedThread())
        result.AppendMessage(f(LldbTarget(process), fp, pc))
    except (RuntimeError, ValueError) as e:
        result.SetError(str(e))


def backtrace(debugger, command, result, internal_dict):
    run(debugger, result, vali_debug.backtrace)


def frame(debugger, command, result, internal_dict):
    n = int(command) if command.strip() else 0
    run(debugger, result, lambda t, fp, pc: vali_debug.frame_slots(t, fp, pc, n))


def show(process, word):
    fp, pc = interpreter(process.GetSelectedThread())
    target = LldbTarget(process)
    layout = vali_debug.layout_near(target, fp, pc)
    if layout is None:
        return "Value(%#x)" % word
    return vali_debug.show(target, layout, word)


def word_of(value):
    """An SBValue holding a Vali value, or a raw word, as an integer."""
    if value.GetNumChildren() == 1:
        value = value.GetChildAtIndex(0)
    return value.GetValueAsUnsigned()


def print_value(debugger, command, result, internal_dict):
    frame = debugger.GetSelectedTarget().GetProcess().GetSelectedThread().GetSelectedFrame()
    value = frame.EvaluateExpression(command)
    if not value.GetError().Success():
        result.SetError(value.GetError().GetCString())
        return
    process = debugger.GetSelectedTarget().GetProcess()
    try:
        result.AppendMessage(show(process, word_of(value)))
    except (RuntimeError, ValueError) as e:
        result.SetError(str(e))


def value_summary(value, internal_dict):
    word = word_of(value)
    try:
        return show(value.GetProcess(), word)
    except (RuntimeError, ValueError):
        return "Value(%#x)" % word


def __lldb_init_module(debugger, internal_dict):
    module = __name__
    debugger.HandleCommand("command script add -o -f %s.backtrace vali-bt" % module)
    debugger.HandleCommand("command script add -o -f %s.frame vali-frame" % module)
    debugger.HandleCommand("command script add -o -f %s.print_value vali-print" % module)
    debugger.HandleCommand(
        "type summary add -F %s.value_summary vali_runtime::value::Value" % module)
