//! What Linux asks nk for when it touches user memory.
//!
//! This is the whole of the interface between Linux's idea of user space and
//! nk's. Linux already knows which of a system call's arguments are user
//! pointers -- it marks them `__user` and reaches them through
//! `copy_from_user` -- so once nk answers that, every system call works for
//! the same reason it works on real hardware, and there is nothing to
//! enumerate.
//!
//! What it replaced is worth remembering. `arch/lkl` selects
//! `UACCESS_MEMCPY`: it assumes kernel and user share one flat address space,
//! which is true of every host LKL was written for and false of one that runs
//! its processes at EL0 with their own translation tables. So nk carried a
//! table describing each syscall's arguments and bounced its buffers across
//! -- a list of every call a program might make, which is the thing this
//! project exists to avoid writing. `ldk lkl` patches `arch/lkl` to call
//! these three functions instead.
//!
//! **The same three functions serve two callers.** A program at EL0, whose
//! pointers must be translated with EL0's permissions and refused if they
//! name kernel memory; and nk itself, which calls into Linux with kernel
//! buffers to seed the rootfs and read an ELF. `sched::in_user_syscall` says
//! which, and getting it backwards either breaks nk's own calls or hands a
//! process the kernel.
//!
//! All three return **the number of bytes not transferred**, which is Linux's
//! convention and the opposite of most people's first guess.

use crate::sched;
use crate::uaccess;

/// Translation uses the live `TTBR0`, which is the calling process's: Linux
/// runs a system call on the thread that made it, and nk restores each task's
/// `TTBR0` when it schedules it. So whenever this code runs on behalf of a
/// process, that process's address space is the one installed.
///
/// The exception is Linux touching user memory from some *other* task -- a
/// workqueue finishing an asynchronous operation, say. That fails with EFAULT
/// rather than reading the wrong process, because `AT S1E0R` against the
/// wrong tables does not find the address. Loud and wrong beats quiet and
/// wrong, and no syscall a program has made here has needed it yet.
#[no_mangle]
pub extern "C" fn lkl_copy_from_user(to: *mut u8, from: usize, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    if !sched::in_user_syscall() {
        unsafe { core::ptr::copy(from as *const u8, to, n) };
        return 0;
    }
    let dst = unsafe { core::slice::from_raw_parts_mut(to, n) };
    match uaccess::copy_from_user(dst, from as u64) {
        Ok(()) => 0,
        Err(_) => n,
    }
}

#[no_mangle]
pub extern "C" fn lkl_copy_to_user(to: usize, from: *const u8, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    if !sched::in_user_syscall() {
        unsafe { core::ptr::copy(from, to as *mut u8, n) };
        return 0;
    }
    let src = unsafe { core::slice::from_raw_parts(from, n) };
    match uaccess::copy_to_user(to as u64, src) {
        Ok(()) => 0,
        Err(_) => n,
    }
}

/// `clear_user`, which is `memset(0)` with the same permission check. Linux
/// uses it to zero the tail of a partially read page and the unwritten part
/// of a structure, so it is on the path of ordinary reads and not an edge
/// case.
#[no_mangle]
pub extern "C" fn lkl_clear_user(to: usize, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    if !sched::in_user_syscall() {
        unsafe { core::ptr::write_bytes(to as *mut u8, 0, n) };
        return 0;
    }
    // A page at a time out of one zeroed page, rather than a buffer as large
    // as the request: `clear_user` is asked for whole pages often enough that
    // allocating one to hold zeroes would be a strange way to spend memory.
    const ZEROES: [u8; 4096] = [0; 4096];
    let mut done = 0;
    while done < n {
        let step = (n - done).min(ZEROES.len());
        if uaccess::copy_to_user((to + done) as u64, &ZEROES[..step]).is_err() {
            return n - done;
        }
        done += step;
    }
    0
}
