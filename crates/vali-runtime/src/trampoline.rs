//! The VM's entry trampoline and the marker stubs of interpreted frames
//! (ADR 0004 #6).
//!
//! The VM is entered through `vali_vm_enter`, a native function with a
//! standard frame record. While Scheme code runs, the VM keeps that record
//! pointing at the current Scheme frame, so a native unwinder stopped in the
//! interpreter walks from the interpreter's own frames, through the
//! trampoline, into the Scheme frames on the stack segment, and back to the
//! host.
//!
//! Interpreted frames' return addresses point into marker stubs: code that
//! never runs, with call-frame information describing an interpreted frame.
//! `vali_interpreted` describes an ordinary frame: the frame pointer holds
//! the frame record. `vali_interpreted_entry` describes the bottom frame,
//! whose caller is the host: the host's stack pointer is kept in the word
//! three above the frame record, because a frame on a stack segment cannot
//! say where the host's stack was.
//!
//! The assembly exists for x86-64 Linux so far. Elsewhere, the VM runs
//! without it: interpreted frames are still native frame records, but
//! unwinders cannot reach them from the interpreter.

/// Where the VM keeps the trampoline's frame record up to date, and what it
/// must restore there before returning to the host.
#[derive(Debug)]
pub(crate) struct Link {
    /// The trampoline's frame record: its saved frame pointer, then its
    /// return address.
    pub(crate) record: *mut u64,
    /// The host's frame pointer, saved by the trampoline.
    pub(crate) host_fp: u64,
    /// The trampoline's return address into the host.
    pub(crate) host_return: u64,
    /// The host's stack pointer before it called the trampoline.
    pub(crate) host_cfa: u64,
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod imp {
    use std::ffi::c_void;

    std::arch::global_asm!(
        // vali_vm_enter(state: *mut c_void, body: fn(*mut c_void)):
        // stores the address of its own frame record in `*state`, then calls
        // `body(state)`.
        ".text",
        ".globl vali_vm_enter",
        ".type vali_vm_enter,@function",
        "vali_vm_enter:",
        ".cfi_startproc",
        // A signal frame, to unwinders: the frames beyond it are on another
        // stack, so they may sit at lower addresses. GDB stops at such a
        // frame unless it is a signal frame.
        ".cfi_signal_frame",
        "push rbp",
        ".cfi_def_cfa_offset 16",
        ".cfi_offset rbp, -16",
        "mov rbp, rsp",
        ".cfi_def_cfa_register rbp",
        "mov qword ptr [rdi], rbp",
        "call rsi",
        "pop rbp",
        ".cfi_def_cfa rsp, 8",
        "ret",
        ".cfi_endproc",
        ".size vali_vm_enter, .-vali_vm_enter",
        // An ordinary interpreted frame: the frame pointer holds its record.
        // The nop keeps the address before the return address inside the
        // rows where the frame pointer is the CFA's base.
        ".globl vali_interpreted",
        ".type vali_interpreted,@function",
        "vali_interpreted:",
        ".cfi_startproc",
        "push rbp",
        ".cfi_def_cfa_offset 16",
        ".cfi_offset rbp, -16",
        "mov rbp, rsp",
        ".cfi_def_cfa_register rbp",
        "nop",
        // Return addresses are 8-byte aligned, so they read as fixnums.
        ".p2align 3, 0x90",
        ".globl vali_interpreted_resume",
        "vali_interpreted_resume:",
        "ud2",
        ".cfi_endproc",
        ".size vali_interpreted, .-vali_interpreted",
        // The first frame of a chained segment, which may sit anywhere
        // relative to the segment its caller is on: an ordinary interpreted
        // frame, marked as a signal frame for the same reason as the
        // trampoline.
        ".globl vali_interpreted_boundary",
        ".type vali_interpreted_boundary,@function",
        "vali_interpreted_boundary:",
        ".cfi_startproc",
        ".cfi_signal_frame",
        "push rbp",
        ".cfi_def_cfa_offset 16",
        ".cfi_offset rbp, -16",
        "mov rbp, rsp",
        ".cfi_def_cfa_register rbp",
        "nop",
        ".p2align 3, 0x90",
        ".globl vali_interpreted_boundary_resume",
        "vali_interpreted_boundary_resume:",
        "ud2",
        ".cfi_endproc",
        ".size vali_interpreted_boundary, .-vali_interpreted_boundary",
        // The bottom interpreted frame, whose caller is the host:
        //   CFA = *(rbp + 24), the host's stack pointer;
        //   the caller's rbp is at rbp + 0, its return address at rbp + 8.
        ".globl vali_interpreted_entry",
        ".type vali_interpreted_entry,@function",
        "vali_interpreted_entry:",
        ".cfi_startproc",
        // DW_CFA_def_cfa_expression: DW_OP_breg6 24; DW_OP_deref
        ".cfi_escape 0x0f, 0x03, 0x76, 0x18, 0x06",
        // DW_CFA_expression rbp: DW_OP_breg6 0
        ".cfi_escape 0x10, 0x06, 0x02, 0x76, 0x00",
        // DW_CFA_expression return address (16): DW_OP_breg6 8
        ".cfi_escape 0x10, 0x10, 0x02, 0x76, 0x08",
        "nop",
        ".p2align 3, 0x90",
        ".globl vali_interpreted_entry_resume",
        "vali_interpreted_entry_resume:",
        "ud2",
        ".cfi_endproc",
        ".size vali_interpreted_entry, .-vali_interpreted_entry",
    );

    unsafe extern "C-unwind" {
        fn vali_vm_enter(state: *mut c_void, body: unsafe extern "C-unwind" fn(*mut c_void));
        static vali_interpreted_resume: u8;
        static vali_interpreted_entry_resume: u8;
        static vali_interpreted_boundary_resume: u8;
    }

    /// The return address of the first frame in a chained segment.
    pub(crate) fn boundary_resume() -> u64 {
        &raw const vali_interpreted_boundary_resume as u64
    }

    /// The return address of ordinary interpreted frames.
    pub(crate) fn resume() -> u64 {
        &raw const vali_interpreted_resume as u64
    }

    /// The return address of the bottom interpreted frame.
    pub(crate) fn entry_resume() -> u64 {
        &raw const vali_interpreted_entry_resume as u64
    }

    /// Calls `body(state)` through the trampoline. `state` must start with a
    /// `*mut u64` field, which receives the trampoline's frame record.
    ///
    /// # Safety
    ///
    /// `body` must restore the trampoline's frame record before returning.
    pub(crate) unsafe fn enter(state: *mut c_void, body: unsafe extern "C-unwind" fn(*mut c_void)) {
        // SAFETY: passed on to the caller.
        unsafe { vali_vm_enter(state, body) }
    }
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
mod imp {
    use std::ffi::c_void;

    pub(crate) fn resume() -> u64 {
        0
    }

    pub(crate) fn entry_resume() -> u64 {
        0
    }

    pub(crate) fn boundary_resume() -> u64 {
        0
    }

    /// Calls `body(state)` directly, with a dummy frame record.
    pub(crate) unsafe fn enter(state: *mut c_void, body: unsafe extern "C-unwind" fn(*mut c_void)) {
        let mut record = [0u64; 2];
        // SAFETY: `state` starts with a `*mut u64` field.
        unsafe {
            *(state as *mut *mut u64) = record.as_mut_ptr();
            body(state);
        }
    }
}

pub(crate) use imp::{boundary_resume, enter, entry_resume, resume};
