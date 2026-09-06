//! The initial process stack: argc, argv, envp, and the auxiliary vector.
//!
//! This is the one interface every Linux binary uses and none of them
//! declares. A libc's `_start` does not receive arguments -- it reads them
//! off the stack at a layout the kernel is simply expected to have built, and
//! the auxiliary vector after them is how the kernel tells it the page size,
//! where its own program headers are, and where to find sixteen bytes of
//! randomness for the stack guard. Get the shape wrong and the program does
//! not fail at a syscall nk could name; it dereferences whatever was there.
//!
//! The layout, from the stack pointer upwards:
//!
//! ```text
//!   sp -> argc
//!         argv[0] .. argv[argc-1]
//!         NULL
//!         envp[0] .. envp[n-1]
//!         NULL
//!         a_type, a_val   (pairs, AT_NULL terminated)
//!         ...
//!         the strings themselves, and AT_RANDOM's sixteen bytes
//! ```
//!
//! `sp` must be sixteen-byte aligned at entry; AAPCS requires it and the
//! first `stp` in any real `_start` will fault if it is not.

use crate::frames::PAGE;
use alloc::vec::Vec;

pub const AT_NULL: u64 = 0;
pub const AT_PHDR: u64 = 3;
pub const AT_PHENT: u64 = 4;
pub const AT_PHNUM: u64 = 5;
pub const AT_PAGESZ: u64 = 6;
pub const AT_BASE: u64 = 7;
pub const AT_FLAGS: u64 = 8;
pub const AT_ENTRY: u64 = 9;
pub const AT_UID: u64 = 11;
pub const AT_EUID: u64 = 12;
pub const AT_GID: u64 = 13;
pub const AT_EGID: u64 = 14;
pub const AT_HWCAP: u64 = 16;
pub const AT_CLKTCK: u64 = 17;
pub const AT_SECURE: u64 = 23;
pub const AT_RANDOM: u64 = 25;

/// Builds the image of a stack page in kernel memory, filling downward.
///
/// The page is written through the kernel's identity alias, but every address
/// *stored* in it has to be the address the process will see -- so the
/// builder carries both, and `user_of` is the only place the two are related.
pub struct Builder {
    page: *mut u8,
    /// The user address the page's first byte will have.
    base: u64,
    /// How far down from the top of the page has been used.
    off: usize,
}

impl Builder {
    /// # Safety
    /// `page` must be a writable mapping of `PAGE` bytes that the process
    /// will see at `top - PAGE`.
    pub unsafe fn new(page: *mut u8, top: u64) -> Builder {
        Builder { page, base: top - PAGE as u64, off: PAGE }
    }

    fn user_of(&self, off: usize) -> u64 {
        self.base + off as u64
    }

    /// Copy bytes in and return the address the process will see them at.
    ///
    /// Everything here is bounded by the one page, so a program with a
    /// preposterous environment is refused rather than writing off the end.
    fn push(&mut self, bytes: &[u8]) -> Option<u64> {
        if self.off < bytes.len() {
            return None;
        }
        self.off -= bytes.len();
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.page.add(self.off), bytes.len());
        }
        Some(self.user_of(self.off))
    }

    /// The NUL goes down first, because the stack fills downward and the
    /// terminator has to end up *after* the string in memory.
    fn push_str(&mut self, s: &[u8]) -> Option<u64> {
        self.push(&[0])?;
        self.push(s)
    }

    fn align(&mut self, to: usize) {
        self.off &= !(to - 1);
    }

    /// Lay the whole thing out and return the stack pointer the process
    /// should start with.
    ///
    /// AT_RANDOM and AT_NULL are appended here rather than left to the
    /// caller. A missing terminator is a libc walking the stack until it
    /// faults, a long way from where the mistake was; and AT_RANDOM has to
    /// point at bytes this function is the one placing.
    pub fn build(
        mut self,
        args: &[&[u8]],
        envs: &[&[u8]],
        aux: &[(u64, u64)],
        random: &[u8; 16],
    ) -> Option<u64> {
        let mut argv: Vec<u64> = Vec::new();
        let mut envv: Vec<u64> = Vec::new();
        for a in args {
            argv.push(self.push_str(a)?);
        }
        for e in envs {
            envv.push(self.push_str(e)?);
        }

        // A libc reads its stack guard straight out of these sixteen bytes,
        // so a constant would give every process on the machine the same
        // canary and quietly undo the feature.
        let at_random = self.push(random)?;

        // sp has to be sixteen-byte aligned, and sp is what the vector starts
        // at -- so the vector's size has to be known before it is written.
        let words = 1 + argv.len() + 1 + envv.len() + 1 + (aux.len() + 2) * 2;
        let bytes = words * 8;
        self.align(16);
        if self.off < bytes {
            return None;
        }
        self.off -= bytes;
        self.align(16);
        let sp = self.user_of(self.off);

        let mut at = self.off;
        for v in [argv.len() as u64]
            .into_iter()
            .chain(argv.iter().copied())
            .chain([0])
            .chain(envv.iter().copied())
            .chain([0])
            .chain(aux.iter().flat_map(|&(t, v)| [t, v]))
            .chain([AT_RANDOM, at_random, AT_NULL, 0])
        {
            unsafe { core::ptr::write_unaligned(self.page.add(at) as *mut u64, v) };
            at += 8;
        }
        Some(sp)
    }
}
