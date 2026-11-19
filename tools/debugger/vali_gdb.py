"""Vali's GDB extension: Scheme backtraces, frames and values.

Load it with `source tools/debugger/vali_gdb.py`, or from .gdbinit. It
adds three commands, which work whether or not vali-runtime was built with
native frames, as long as the build keeps the interpreter's locals in its
debug information:

    vali-bt          the Scheme frames, innermost first
    vali-frame N     Scheme frame N's procedure and slots, as values
    vali-print EXPR  EXPR, a Vali value or raw word, as Scheme writes it

and a pretty-printer for `vali_runtime::value::Value`.
"""

import os
import re
import sys

import gdb

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vali_debug  # noqa: E402


class GdbTarget(vali_debug.Target):
    def read(self, address, size):
        try:
            return gdb.selected_inferior().read_memory(address, size).tobytes()
        except gdb.MemoryError as e:
            raise MemoryError(str(e))


def is_interpreter(name):
    """Whether `name` is the VM's interpreter function.

    With full debug information the name has its module path
    (`vali_runtime::vm::...::interpret<...>`); with line tables only it
    is just `interpret<vali_runtime::...>`.
    """
    return re.search(r"(?:^|::)interpret(?:<|$)", name) is not None \
        and "vali_runtime" in name


def interpreter():
    """The innermost interpreter frame's `fp` and `pc`, from DWARF."""
    frame = gdb.newest_frame()
    while frame is not None:
        name = frame.name() or ""
        if is_interpreter(name):
            try:
                return int(frame.read_var("fp")), int(frame.read_var("pc"))
            except (ValueError, gdb.error):
                raise gdb.GdbError(
                    "the interpreter's fp and pc were optimised out: "
                    "build with full debug information")
        frame = frame.older()
    raise gdb.GdbError("no Vali interpreter frame on this thread")


class Backtrace(gdb.Command):
    """Show Vali's Scheme frames, innermost first."""

    def __init__(self):
        super().__init__("vali-bt", gdb.COMMAND_STACK)

    def invoke(self, argument, from_tty):
        fp, pc = interpreter()
        print(vali_debug.backtrace(GdbTarget(), fp, pc))


class FrameSlots(gdb.Command):
    """Show Vali Scheme frame N's procedure and slots: vali-frame N."""

    def __init__(self):
        super().__init__("vali-frame", gdb.COMMAND_STACK)

    def invoke(self, argument, from_tty):
        fp, pc = interpreter()
        n = int(argument) if argument.strip() else 0
        print(vali_debug.frame_slots(GdbTarget(), fp, pc, n))


def word_of(value):
    """A gdb.Value holding a Vali value, or a raw word, as an integer."""
    if value.type.strip_typedefs().code == gdb.TYPE_CODE_STRUCT:
        return int(value[value.type.fields()[0].name])
    return int(value)


def show(word):
    fp, pc = interpreter()
    target = GdbTarget()
    layout = vali_debug.layout_near(target, fp, pc)
    if layout is None:
        return "Value(%#x)" % word
    return vali_debug.show(target, layout, word)


class Print(gdb.Command):
    """Show a Vali value as Scheme writes it: vali-print EXPR."""

    def __init__(self):
        super().__init__("vali-print", gdb.COMMAND_DATA)

    def invoke(self, argument, from_tty):
        print(show(word_of(gdb.parse_and_eval(argument)) % (1 << 64)))


class ValuePrinter:
    def __init__(self, value):
        self.value = value

    def to_string(self):
        word = word_of(self.value) % (1 << 64)
        try:
            return show(word)
        except gdb.GdbError:
            return "Value(%#x)" % word


def lookup(value):
    name = str(value.type.strip_typedefs())
    if name == "vali_runtime::value::Value":
        return ValuePrinter(value)
    return None


Backtrace()
FrameSlots()
Print()
gdb.pretty_printers.append(lookup)
