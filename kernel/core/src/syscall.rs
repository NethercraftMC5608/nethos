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
//! the ones that are not are the ones with pointers inside pointers, which
//! have to be walked rather than copied. `writev`/`readv`'s iovecs are done;
//! `execve`'s argv and envp belong to nk's own loader rather than to Linux,
//! and `sendmsg`'s control messages are still to come.
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
    /// Its previous contents are *not* copied in; use `InStruct` for that.
    Struct(usize),
    /// A structure of known size the kernel only reads -- `struct timespec`.
    InStruct(usize),
    /// An array of `struct iovec`, whose count is the argument at this index.
    /// The array is copied, and so is everything it points at. `write` is
    /// true for the direction Linux reads (`writev`), false for the direction
    /// it writes (`readv`).
    Iov { count: usize, write: bool },
}

use Arg::{In, InStruct, Iov, Out, Path, Scalar, Struct};

/// `struct stat` on aarch64. Fixed by the ABI, not by us.
const STAT_SIZE: usize = 128;
/// `struct utsname`: six fields of 65 bytes.
const UTSNAME_SIZE: usize = 390;
/// `struct timespec`: two 64-bit words.
const TIMESPEC_SIZE: usize = 16;
/// `struct iovec`: a pointer and a length.
const IOVEC_SIZE: usize = 16;
/// Linux's own `UIO_MAXIOV`. A process that asks for more gets EINVAL from
/// Linux anyway; refusing here keeps nk from allocating for the attempt.
const MAX_IOV: usize = 1024;

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
        17 => [Out(1), Scalar, Scalar, Scalar, Scalar, Scalar],  // getcwd
        23 | 24 => [Scalar; 6],                                  // dup, dup3
        25 => [Scalar; 6],                                       // fcntl
        34 => [Scalar, Path, Scalar, Scalar, Scalar, Scalar],    // mkdirat
        35 => [Scalar, Path, Scalar, Scalar, Scalar, Scalar],    // unlinkat
        37 => [Scalar, Path, Scalar, Path, Scalar, Scalar],      // linkat
        38 => [Scalar, Path, Scalar, Path, Scalar, Scalar],      // renameat
        48 => [Scalar, Path, Scalar, Scalar, Scalar, Scalar],    // faccessat
        49 => [Path, Scalar, Scalar, Scalar, Scalar, Scalar],    // chdir
        // pipe2 writes two ints and returns 0, so the size is the whole of it
        59 => [Struct(8), Scalar, Scalar, Scalar, Scalar, Scalar], // pipe2
        61 => [Scalar, Out(2), Scalar, Scalar, Scalar, Scalar],  // getdents64
        // readlinkat does not NUL-terminate; the return is what it wrote
        78 => [Scalar, Path, Out(3), Scalar, Scalar, Scalar],    // readlinkat
        82 | 83 => [Scalar; 6],                                  // fsync, fdatasync
        // --- scatter/gather ----------------------------------------------
        65 => [Scalar, Iov { count: 2, write: false }, Scalar, Scalar, Scalar, Scalar], // readv
        66 => [Scalar, Iov { count: 2, write: true }, Scalar, Scalar, Scalar, Scalar],  // writev
        // --- the system --------------------------------------------------
        160 => [Struct(UTSNAME_SIZE), Scalar, Scalar, Scalar, Scalar, Scalar], // uname
        278 => [Out(1), Scalar, Scalar, Scalar, Scalar, Scalar], // getrandom
        101 => [InStruct(TIMESPEC_SIZE), Struct(TIMESPEC_SIZE), Scalar, Scalar, Scalar, Scalar], // nanosleep
        113 | 114 => [Scalar, Struct(TIMESPEC_SIZE), Scalar, Scalar, Scalar, Scalar], // clock_gettime, clock_getres
        // --- identity: no pointers at all --------------------------------
        172 | 174 | 175 | 176 | 177 | 178 => [Scalar; 6],
        _ => return None,
    };
    Some(d)
}

/// What happens to a bounce buffer after the call.
enum Back {
    /// Nothing. The kernel only read it.
    None,
    /// The whole thing, if the call succeeded.
    Whole,
    /// As many bytes as the call says it produced.
    Prefix,
    /// As many bytes as the call says it produced, spread across these user
    /// ranges in order -- which is what a scatter read means.
    Scatter(Vec<(u64, usize)>),
}

/// A bounce buffer, and where its contents go afterwards.
struct Bounce {
    /// Which argument it replaced.
    index: usize,
    /// The user address to copy back to, if anything is copied back.
    user: u64,
    data: Vec<u8>,
    /// What an iovec array points at. Kept here so it outlives the call; the
    /// argument itself is `data`. A `Vec`'s allocation does not move when the
    /// `Vec` does, so pointers taken into this stay good once it is filled.
    payload: Vec<u8>,
    back: Back,
}

impl Bounce {
    fn plain(index: usize, user: u64, data: Vec<u8>, back: Back) -> Bounce {
        Bounce { index, user, data, payload: Vec::new(), back }
    }
}

/// Copy an iovec array and everything it points at into one flat buffer.
///
/// The kernel sees a normal iovec array whose entries point into a single
/// allocation -- Linux does not care that they are contiguous, and it makes
/// the copy back a walk over offsets rather than a second set of allocations.
fn bounce_iov(index: usize, user_iov: u64, count: u64, write: bool) -> Result<Bounce, i64> {
    const EINVAL: i64 = -22;
    let count = count as usize;
    if count > MAX_IOV {
        return Err(EINVAL);
    }

    let mut raw = vec![0u8; count * IOVEC_SIZE];
    uaccess::copy_from_user(&mut raw, user_iov)?;

    // Sum first, and refuse the whole call rather than truncating one entry:
    // a short writev is a legitimate result and would hide the refusal.
    let mut iov: Vec<(u64, usize)> = Vec::new();
    let mut total: usize = 0;
    for i in 0..count {
        let e = &raw[i * IOVEC_SIZE..];
        let base = u64::from_le_bytes(e[0..8].try_into().unwrap());
        let len = u64::from_le_bytes(e[8..16].try_into().unwrap()) as usize;
        total = match total.checked_add(len) {
            Some(t) if t <= MAX_TRANSFER => t,
            _ => return Err(EINVAL),
        };
        iov.push((base, len));
    }

    let mut payload = vec![0u8; total];
    let mut off = 0;
    for &(base, len) in &iov {
        if len == 0 {
            continue;
        }
        if write {
            uaccess::copy_from_user(&mut payload[off..off + len], base)?;
        } else {
            // Checked before the call for the same reason `Out` is: a read
            // that succeeds and then cannot deliver has already happened.
            uaccess::copy_to_user(base, &[])?;
        }
        off += len;
    }

    // Now the kernel's own array, pointing into the buffer just filled.
    let base = payload.as_ptr() as u64;
    let mut off = 0u64;
    for i in 0..count {
        let e = &mut raw[i * IOVEC_SIZE..];
        e[0..8].copy_from_slice(&(base + off).to_le_bytes());
        off += iov[i].1 as u64;
    }

    let back = if write { Back::None } else { Back::Scatter(iov) };
    Ok(Bounce { index, user: user_iov, data: raw, payload, back })
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
                Bounce::plain(i, args[i], buf, Back::None)
            }
            In(_) | InStruct(_) => {
                let len = match *arg {
                    InStruct(size) => size,
                    In(len_arg) => (args[len_arg] as usize).min(MAX_TRANSFER),
                    _ => unreachable!(),
                };
                let mut buf = vec![0u8; len];
                if let Err(e) = uaccess::copy_from_user(&mut buf, args[i]) {
                    return Some(e);
                }
                Bounce::plain(i, args[i], buf, Back::None)
            }
            Out(len_arg) => {
                let len = (args[len_arg] as usize).min(MAX_TRANSFER);
                // Checked before the call, not after: a call that succeeds
                // and then cannot deliver its results has already happened,
                // and there is nothing honest to return.
                if len > 0 && uaccess::copy_to_user(args[i], &[]).is_err() {
                    return Some(EFAULT);
                }
                Bounce::plain(i, args[i], vec![0u8; len], Back::Prefix)
            }
            Struct(size) => Bounce::plain(i, args[i], vec![0u8; size], Back::Whole),
            Iov { count, write } => match bounce_iov(i, args[i], args[count], write) {
                Ok(b) => b,
                Err(e) => return Some(e),
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
        match &b.back {
            Back::None => {}
            // Only what the call actually produced. `read` returning 12 of a
            // 4096-byte request must not scribble the other 4084 bytes over
            // whatever the process had there.
            Back::Whole | Back::Prefix => {
                let n = match b.back {
                    Back::Whole if ret >= 0 => b.data.len(),
                    Back::Prefix if ret > 0 => (ret as usize).min(b.data.len()),
                    _ => continue,
                };
                if uaccess::copy_to_user(b.user, &b.data[..n]).is_err() {
                    return Some(EFAULT);
                }
            }
            Back::Scatter(iov) => {
                if ret <= 0 {
                    continue;
                }
                // The kernel filled the flat buffer in iovec order, so it
                // comes back out in iovec order, stopping where the call did.
                let mut left = (ret as usize).min(b.payload.len());
                let mut off = 0;
                for &(base, len) in iov {
                    if left == 0 {
                        break;
                    }
                    let n = len.min(left);
                    if uaccess::copy_to_user(base, &b.payload[off..off + n]).is_err() {
                        return Some(EFAULT);
                    }
                    off += len;
                    left -= n;
                }
            }
        }
    }

    Some(ret)
}
