"""Vali's LLDB extension: Scheme backtraces, frames and values.

Load it with `command script import tools/debugger/vali_lldb.py`, or from
.lldbinit. It adds three commands, which work whether or not vali-runtime
was built with native frames. They find the innermost Scheme frame from the
records the VM keeps (the exported symbol VALI_DEBUG_STATES), which needs no
debug information, or from the interpreter's locals where the build has full
debug information. Set VALI_DEBUGGER_NO_DWARF=1 to use the records only:

    vali-bt          the Scheme frames, innermost first
    vali-frame N     Scheme frame N's procedure and slots, as values
    vali-print EXPR  EXPR, a Vali value or raw word, as Scheme writes it

and a summary for `vali_runtime::value::Value`.
"""

import os
import re
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


def is_interpreter(name):
    """Whether `name` is the VM's interpreter function.

    With full debug information the name has its module path
    (`vali_runtime::vm::...::interpret<...>`); with line tables only it
    is just `interpret<vali_runtime::...>`.
    """
    return re.search(r"(?:^|::)interpret(?:<|$)", name) is not None \
        and "vali_runtime" in name


def from_dwarf(thread):
    """The innermost interpreter frame's `fp` and `pc`, from DWARF locals,
    or None where the build lacks them (or they were optimised out)."""
    for frame in thread:
        name = frame.GetFunctionName() or ""
        if is_interpreter(name):
            fp, pc = frame.FindVariable("fp"), frame.FindVariable("pc")
            if not (fp.IsValid() and pc.IsValid()):
                return None
            return fp.GetValueAsUnsigned(), pc.GetValueAsUnsigned()
    return None


def from_records(thread):
    """The innermost Scheme frame's `fp` and `pc`, from the VM's records,
    found through the exported symbol."""
    process = thread.GetProcess()
    target = process.GetTarget()
    symbols = target.FindSymbols(vali_debug.STATE_SYMBOL)
    if symbols.GetSize() == 0:
        raise RuntimeError(
            "no %s symbol: this is not a Vali host, or its symbols were "
            "stripped (keep that symbol, or build with full debug "
            "information)" % vali_debug.STATE_SYMBOL)
    symbol = symbols.GetContextAtIndex(0).GetSymbol()
    address = symbol.GetStartAddress().GetLoadAddress(target)
    reader = LldbTarget(process)
    try:
        return vali_debug.current_state(
            reader, reader.u64(address), thread.GetThreadID())
    except (LookupError, MemoryError) as e:
        raise RuntimeError(str(e))


def interpreter(thread):
    """The innermost Scheme frame's `fp` and `pc`: from the interpreter's
    DWARF locals when the build has them, else from the VM's records."""
    if not os.environ.get("VALI_DEBUGGER_NO_DWARF"):
        found = from_dwarf(thread)
        if found is not None:
            return found
    return from_records(thread)


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
