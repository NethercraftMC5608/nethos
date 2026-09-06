//! Forwarding a system call to Linux.
//!
//! There is nothing here to keep up to date, and that is the point.
//!
//! This file used to hold a table describing every system call nk forwarded:
//! which arguments were pointers, which direction the data travelled, and how
//! long it was. It had to, because `arch/lkl` selects `UACCESS_MEMCPY` --
//! Linux assumed kernel and user shared one flat address space, so its
//! `copy_from_user` was a `memcpy` and handing it an EL0 pointer would have
//! bypassed every protection nk has. So nk copied each argument across
//! itself, which meant knowing what each argument *was*, which meant a list of
//! every system call a program might ever make.
//!
//! Linux already knows. It marks user pointers `__user` and reaches them
//! through `copy_from_user`, on every architecture, for every one of the four
//! hundred and fifty calls. `ldk lkl` patches `arch/lkl` to ask nk for that
//! rather than assume it away (see `useraccess.rs`), and the table stopped
//! having anything to say.
//!
//! What is left is the flag that tells `useraccess` these pointers came from
//! EL0 and must be treated as such.

use crate::lkl;
use crate::sched;

/// Hand a system call to Linux with its arguments untouched.
///
/// `Option` for the caller's sake rather than nk's: it used to mean "no
/// descriptor", and now it means only that a build without Linux has nothing
/// to forward to.
pub fn forward(nr: u64, args: &[u64; 6]) -> Option<i64> {
    let was = sched::set_user_syscall(true);
    let ret = lkl::syscall(nr as i64, args.map(|a| a as i64));
    sched::set_user_syscall(was);
    Some(ret)
}
