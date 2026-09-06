//! Moving bytes across the user/kernel boundary, checked.
//!
//! Everything a real program does passes a pointer: `openat` a path, `read` a
//! buffer, `fstat` a struct. Those pointers are user addresses, and they
//! cannot simply be handed to Linux.
//!
//! **Why not, precisely.** Under LKL Linux lives in one flat address space and
//! its own `copy_from_user` is a `memcpy`. During a system call the calling
//! process's `TTBR0` is still installed, so a user pointer *would* resolve --
//! and it would resolve without any permission check at all, which is the
//! first problem. The second is worse: Linux blocks inside system calls. When
//! it does, nk switches to another task and `TTBR0` changes underneath it; by
//! the time Linux resumes, that pointer is either meaningless or is pointing
//! into a different process. It would work in a test and corrupt under load.
//!
//! So the data is copied. In on the way down, out on the way back, through a
//! kernel buffer Linux can hold across a switch, with every page checked as
//! EL0 would see it.
//!
//! The checks are `AT S1E0R` and `AT S1E0W`, which ask the MMU to translate
//! with EL0's permissions. Reads use the first, writes the second -- a
//! read-only user page answers yes to one and no to the other, and copying
//! results back through the read check would let a process ask the kernel to
//! write into its own text.

use crate::paging;

/// -EFAULT. The errno a bad user pointer produces, everywhere.
pub const EFAULT: i64 = -14;

/// Nothing a process asks for may be larger than this.
///
/// The buffer comes out of the kernel heap, and its size is a number the
/// process chose. Without a ceiling, `read(fd, buf, huge)` is a process
/// deciding how much kernel memory to consume.
pub const MAX_TRANSFER: usize = 64 * 1024;

/// Longest path, matching Linux's PATH_MAX so that a path this side accepts
/// is one Linux would too.
pub const PATH_MAX: usize = 4096;

/// Copy `len` bytes from user memory at `src` into `dst`.
///
/// Page at a time, not byte at a time: each translation is an `AT`
/// instruction and an `ISB`, which is far too expensive to pay per byte --
/// the first version of `write` did exactly that and it is why the console
/// path was capped at 4KB.
pub fn copy_from_user(dst: &mut [u8], src: u64) -> Result<(), i64> {
    let mut done = 0usize;
    while done < dst.len() {
        let va = src.checked_add(done as u64).ok_or(EFAULT)?;
        let pa = paging::user_to_phys(va).ok_or(EFAULT)?;
        // Up to the end of this page, or the end of the request.
        let in_page = (0x1000 - (va & 0xfff)) as usize;
        let n = in_page.min(dst.len() - done);
        unsafe {
            core::ptr::copy_nonoverlapping(pa as *const u8, dst[done..].as_mut_ptr(), n);
        }
        done += n;
    }
    Ok(())
}

/// Copy `src` into user memory at `dst`, checking write permission.
pub fn copy_to_user(dst: u64, src: &[u8]) -> Result<(), i64> {
    let mut done = 0usize;
    while done < src.len() {
        let va = dst.checked_add(done as u64).ok_or(EFAULT)?;
        let pa = paging::user_to_phys_write(va).ok_or(EFAULT)?;
        let in_page = (0x1000 - (va & 0xfff)) as usize;
        let n = in_page.min(src.len() - done);
        unsafe {
            core::ptr::copy_nonoverlapping(src[done..].as_ptr(), pa as *mut u8, n);
        }
        done += n;
    }
    Ok(())
}

/// Read a NUL-terminated string from user memory.
///
/// The length is not known in advance, so this scans -- but only as far as
/// `PATH_MAX`, and it stops at the first unreadable page rather than at the
/// first fault. A process that passes a string with no terminator gets
/// -ENAMETOOLONG, not a kernel that walks off the end of its address space.
pub fn copy_cstr_from_user(ptr: u64, out: &mut [u8]) -> Result<usize, i64> {
    let mut len = 0usize;
    while len < out.len() {
        let va = ptr.checked_add(len as u64).ok_or(EFAULT)?;
        let pa = paging::user_to_phys(va).ok_or(EFAULT)?;
        let byte = unsafe { core::ptr::read_volatile(pa as *const u8) };
        out[len] = byte;
        if byte == 0 {
            return Ok(len);
        }
        len += 1;
    }
    Err(-36) // -ENAMETOOLONG
}
