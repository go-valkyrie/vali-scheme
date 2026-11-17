//! Per-function marker stubs, named through the GDB JIT interface
//! (ADR 0004 #6).
//!
//! Each bytecode function gets 16 bytes of executable memory holding a copy
//! of `vali_interpreted`'s prologue: `push rbp; mov rbp, rsp; nop; ud2`.
//! The code never runs. An ordinary interpreted frame's return address
//! points just after the `nop`, so GDB and LLDB, which find no call-frame
//! information for it, analyse the prologue, see a standard frame, and
//! unwind it through the frame pointer. The stubs are registered under
//! their functions' Scheme names through the GDB JIT interface, which both
//! debuggers read, as an in-memory ELF object holding only a symbol table.
//!
//! Only x86-64 Linux so far; elsewhere every function shares the
//! trampoline's `vali_interpreted` stub.

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod imp {
    use std::ptr::null_mut;
    use std::sync::Mutex;

    /// Bytes per stub.
    const STUB: usize = 16;
    /// `push rbp; mov rbp, rsp; nop` four times, `ud2`, padded with `int3`.
    const CODE: [u8; STUB] = [
        0x55, 0x48, 0x89, 0xe5, 0x90, 0x90, 0x90, 0x90, 0x0f, 0x0b, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc,
        0xcc,
    ];
    /// Where a frame's return address points: just after the `nop`s, 8-byte
    /// aligned, so a return address left in a frame's slots reads as a
    /// fixnum to an epoch.
    const RESUME: usize = 8;

    #[repr(C)]
    struct JitCodeEntry {
        next: *mut JitCodeEntry,
        prev: *mut JitCodeEntry,
        symfile_addr: *const u8,
        symfile_size: u64,
    }

    #[repr(C)]
    pub struct JitDescriptor {
        version: u32,
        action_flag: u32,
        relevant_entry: *mut JitCodeEntry,
        first_entry: *mut JitCodeEntry,
    }

    // SAFETY: only touched under `REGISTRATION`, and by debuggers while the
    // process is stopped.
    unsafe impl Sync for JitDescriptor {}

    /// Debuggers set a breakpoint here, and read the descriptor when it is
    /// hit.
    #[unsafe(no_mangle)]
    #[inline(never)]
    pub extern "C" fn __jit_debug_register_code() {
        std::hint::black_box(());
    }

    /// The GDB JIT interface's descriptor, version 1.
    #[unsafe(no_mangle)]
    pub static mut __jit_debug_descriptor: JitDescriptor = JitDescriptor {
        version: 1,
        action_flag: 0,
        relevant_entry: null_mut(),
        first_entry: null_mut(),
    };

    static REGISTRATION: Mutex<()> = Mutex::new(());

    /// One module's stubs. They live as long as the process: modules are
    /// never unloaded, and debuggers keep the symbols.
    #[derive(Debug)]
    pub(crate) struct Stubs {
        base: usize,
    }

    impl Stubs {
        /// Stubs for functions named `names`, registered with debuggers.
        /// `None` if executable memory cannot be had.
        pub(crate) fn new(names: &[&str]) -> Option<Stubs> {
            if names.is_empty() {
                return None;
            }
            let len = (names.len() * STUB).next_multiple_of(4096);
            // SAFETY: an anonymous private mapping, written then made
            // read-only and executable before any address escapes.
            let base = unsafe {
                let p = libc::mmap(
                    null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                );
                if p == libc::MAP_FAILED {
                    return None;
                }
                let bytes = std::slice::from_raw_parts_mut(p as *mut u8, len);
                for chunk in bytes.chunks_mut(STUB) {
                    chunk.copy_from_slice(&CODE);
                }
                if libc::mprotect(p, len, libc::PROT_READ | libc::PROT_EXEC) != 0 {
                    libc::munmap(p, len);
                    return None;
                }
                p as usize
            };
            register(elf(base, names));
            Some(Stubs { base })
        }

        /// The return address for frames of function `index`.
        pub(crate) fn resume(&self, index: usize) -> u64 {
            (self.base + index * STUB + RESUME) as u64
        }
    }

    /// Hands `object` to debuggers through the GDB JIT interface. The object
    /// and its entry are leaked: debuggers may read them at any time.
    fn register(object: Vec<u8>) {
        let object = Box::leak(object.into_boxed_slice());
        let entry = Box::leak(Box::new(JitCodeEntry {
            next: null_mut(),
            prev: null_mut(),
            symfile_addr: object.as_ptr(),
            symfile_size: object.len() as u64,
        }));
        let _guard = REGISTRATION.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the descriptor is only changed under the lock.
        unsafe {
            let descriptor = &raw mut __jit_debug_descriptor;
            entry.next = (*descriptor).first_entry;
            if !entry.next.is_null() {
                (*entry.next).prev = entry;
            }
            (*descriptor).first_entry = entry;
            (*descriptor).relevant_entry = entry;
            (*descriptor).action_flag = 1; // JIT_REGISTER_FN
            __jit_debug_register_code();
            (*descriptor).action_flag = 0;
        }
    }

    /// A relocatable ELF object whose `.text` section, with no contents,
    /// sits at `base`, and whose symbol table names each 16-byte stub.
    fn elf(base: usize, names: &[&str]) -> Vec<u8> {
        let mut strtab = vec![0u8];
        let mut symtab = vec![0u8; 24]; // the null symbol
        for (i, name) in names.iter().enumerate() {
            let name_offset = strtab.len() as u32;
            strtab.extend_from_slice(name.as_bytes());
            strtab.push(0);
            symtab.extend_from_slice(&name_offset.to_le_bytes());
            symtab.push(0x12); // STB_GLOBAL, STT_FUNC
            symtab.push(0);
            symtab.extend_from_slice(&1u16.to_le_bytes()); // .text
            symtab.extend_from_slice(&((i * STUB) as u64).to_le_bytes());
            symtab.extend_from_slice(&(STUB as u64).to_le_bytes());
        }
        let shstrtab = b"\0.text\0.symtab\0.strtab\0.shstrtab\0";
        let (symtab_at, strtab_at) = (64, 64 + symtab.len());
        let shstrtab_at = strtab_at + strtab.len();
        let headers_at = (shstrtab_at + shstrtab.len()).next_multiple_of(8);

        let mut out = Vec::with_capacity(headers_at + 5 * 64);
        out.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&1u16.to_le_bytes()); // ET_REL
        out.extend_from_slice(&62u16.to_le_bytes()); // EM_X86_64
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes()); // entry
        out.extend_from_slice(&0u64.to_le_bytes()); // program headers
        out.extend_from_slice(&(headers_at as u64).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // flags
        for half in [64u16, 0, 0, 64, 5, 4] {
            out.extend_from_slice(&half.to_le_bytes());
        }
        out.extend_from_slice(&symtab);
        out.extend_from_slice(&strtab);
        out.extend_from_slice(shstrtab);
        out.resize(headers_at, 0);

        let mut section = |name: u32,
                           kind: u32,
                           flags: u64,
                           addr: u64,
                           offset: usize,
                           size: usize,
                           link: u32,
                           info: u32,
                           align: u64,
                           entsize: u64| {
            out.extend_from_slice(&name.to_le_bytes());
            out.extend_from_slice(&kind.to_le_bytes());
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&addr.to_le_bytes());
            out.extend_from_slice(&(offset as u64).to_le_bytes());
            out.extend_from_slice(&(size as u64).to_le_bytes());
            out.extend_from_slice(&link.to_le_bytes());
            out.extend_from_slice(&info.to_le_bytes());
            out.extend_from_slice(&align.to_le_bytes());
            out.extend_from_slice(&entsize.to_le_bytes());
        };
        section(0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        // .text: SHT_NOBITS, allocated and executable, at the stubs.
        section(1, 8, 0x6, base as u64, 0, names.len() * STUB, 0, 0, 16, 0);
        // .symtab, linked to .strtab; every symbol after the null one is
        // global.
        section(7, 2, 0, 0, symtab_at, symtab.len(), 3, 1, 8, 24);
        section(15, 3, 0, 0, strtab_at, strtab.len(), 0, 0, 1, 0);
        section(23, 3, 0, 0, shstrtab_at, shstrtab.len(), 0, 0, 1, 0);
        out
    }
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
mod imp {
    #[derive(Debug)]
    pub(crate) struct Stubs;

    impl Stubs {
        pub(crate) fn new(_names: &[&str]) -> Option<Stubs> {
            None
        }

        pub(crate) fn resume(&self, _index: usize) -> u64 {
            0
        }
    }
}

pub(crate) use imp::Stubs;
