//! The marshalling layer: what each system call's arguments mean.
//!
//! Linux implements the calls; nk only has to describe them. For each one it
//! forwards, nk needs to know which arguments are pointers, which direction
//! the data travels, and how long it is -- and that is all. `uaccess` does the
//! copying, `lkl` does the call, and Linux does the work.
//!
//! **The size of the remaining job.** A desktop needs perhaps a hundred and
//! fifty system calls with real semantics. Every one of them already exists,
//! inside `lkl.o`. What is left is this table, and most entries are one line;
//! the ones that are not are the ones with pointers inside pointers --
//! `execve`'s argv and envp, `writev`'s iovecs, `sendmsg`'s control messages
//! -- which have to be walked rather than copied. None of that is here yet
//! and each is a known, separable piece of work.
//!
//! Calls with no descriptor are refused with -ENOSYS *and their number is
//! printed*, which is how the list of what to add next gets written by a real
//! binary rather than guessed at.

use crate::lkl;
use crate::uaccess::{self, EFAULT, MAX_TRANSFER, PATH_MAX};
use alloc::vec;
use alloc::vec::Vec;

/// What one argument is.
#[derive(Clone, Copy, PartialEq)]
enum Arg {
    /// An integer. Passed through untouched.
    Scalar,
    /// A NUL-terminated string the kernel reads: a path, mostly.
    Path,
    /// A buffer the kernel reads, whose length is the argument at this index.
    In(usize),
    /// A buffer the kernel writes, whose length is the argument at this
    /// index. Only the bytes the call says it produced are copied back.
    Out(usize),
    /// A structure of known size that the kernel fills in -- `struct stat`,
    /// `struct utsname`. Copied back whole, and only when the call succeeded.
    Struct(usize),
}

use Arg::{In, Out, Path, Scalar, Struct};

/// `struct stat` on aarch64. Fixed by the ABI, not by us.
const STAT_SIZE: usize = 128;
/// `struct utsname`: six fields of 65 bytes.
const UTSNAME_SIZE: usize = 390;

/// Everything nk knows how to forward.
///
/// Deliberately short. It grows when a real program asks for something and
/// says so, which is a better order than guessing at what a program might
/// want.
fn describe(nr: u64) -> Option<[Arg; 6]> {
    let d = match nr {
        // --- files -------------------------------------------------------
        56 => [Scalar, Path, Scalar, Scalar, Scalar, Scalar], // openat
        57 => [Scalar; 6],                                    // close
        62 => [Scalar; 6],                                    // lseek
        63 => [Scalar, Out(2), Scalar, Scalar, Scalar, Scalar], // read
        64 => [Scalar, In(2), Scalar, Scalar, Scalar, Scalar], // write
        79 => [Scalar, Path, Struct(STAT_SIZE), Scalar, Scalar, Scalar], // newfstatat
        80 => [Scalar, Struct(STAT_SIZE), Scalar, Scalar, Scalar, Scalar], // fstat
        // --- the system --------------------------------------------------
        160 => [Struct(UTSNAME_SIZE), Scalar, Scalar, Scalar, Scalar, Scalar], // uname
        278 => [Out(1), Scalar, Scalar, Scalar, Scalar, Scalar], // getrandom
        // --- identity: no pointers at all --------------------------------
        172 | 174 | 175 | 176 | 177 | 178 => [Scalar; 6],
        _ => return None,
    };
    Some(d)
}

/// A bounce buffer, and where its contents go afterwards.
struct Bounce {
    /// Which argument it replaced.
    index: usize,
    /// The user address to copy back to, if anything is copied back.
    user: u64,
    data: Vec<u8>,
    copy_back: bool,
    /// A `Struct` is copied back whole on success; an `Out` buffer only as
    /// far as the call says it wrote.
    whole: bool,
}

/// Forward one system call to Linux, copying its arguments across.
///
/// Returns None when nk has no descriptor for it -- the caller reports that,
/// because it wants to name the number.
pub fn forward(nr: u64, args: &[u64; 6]) -> Option<i64> {
    let desc = describe(nr)?;
    let mut call = args.map(|a| a as i64);
    let mut bounces: Vec<Bounce> = Vec::new();

    for (i, arg) in desc.iter().enumerate() {
        // A null pointer is a null pointer: several calls accept one and
        // mean something by it, and bouncing it would turn that into a
        // pointer to an empty buffer, which means something else.
        if *arg != Scalar && args[i] == 0 {
            continue;
        }
        let bounce = match *arg {
            Scalar => continue,
            Path => {
                let mut buf = vec![0u8; PATH_MAX];
                let len = match uaccess::copy_cstr_from_user(args[i], &mut buf) {
                    Ok(n) => n,
                    Err(e) => return Some(e),
                };
                buf.truncate(len + 1); // keep the NUL: Linux expects one
                Bounce { index: i, user: args[i], data: buf, copy_back: false, whole: false }
            }
            In(len_arg) => {
                let len = (args[len_arg] as usize).min(MAX_TRANSFER);
                let mut buf = vec![0u8; len];
                if let Err(e) = uaccess::copy_from_user(&mut buf, args[i]) {
                    return Some(e);
                }
                Bounce { index: i, user: args[i], data: buf, copy_back: false, whole: false }
            }
            Out(len_arg) => {
                let len = (args[len_arg] as usize).min(MAX_TRANSFER);
                // Checked before the call, not after: a call that succeeds
                // and then cannot deliver its results has already happened,
                // and there is nothing honest to return.
                if len > 0 && uaccess::copy_to_user(args[i], &vec![0u8; 0]).is_err() {
                    return Some(EFAULT);
                }
                Bounce {
                    index: i,
                    user: args[i],
                    data: vec![0u8; len],
                    copy_back: true,
                    whole: false,
                }
            }
            Struct(size) => Bounce {
                index: i,
                user: args[i],
                data: vec![0u8; size],
                copy_back: true,
                whole: true,
            },
        };
        bounces.push(bounce);
    }

    // The kernel addresses Linux will see. Taken after every buffer exists,
    // because a Vec that grows moves.
    for b in &bounces {
        call[b.index] = b.data.as_ptr() as i64;
    }

    let ret = lkl::syscall(nr as i64, call);

    for b in &bounces {
        if !b.copy_back {
            continue;
        }
        // Only what the call actually produced. `read` returning 12 of a
        // 4096-byte request must not scribble the other 4084 bytes over
        // whatever the process had there.
        let n = if b.whole {
            if ret < 0 {
                continue;
            }
            b.data.len()
        } else {
            if ret <= 0 {
                continue;
            }
            (ret as usize).min(b.data.len())
        };
        if uaccess::copy_to_user(b.user, &b.data[..n]).is_err() {
            return Some(EFAULT);
        }
    }

    Some(ret)
}
