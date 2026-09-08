//! Booting Linux on nk.
//!
//! Three functions. `lkl_init` hands Linux the struct of function pointers
//! that is its machine -- nk's threads, memory, clock and console, assembled
//! in `kernel/lkl/nk-host.c`. `lkl_start_kernel` boots it. `lkl_syscall` exposes
//! Linux services to kernel-owned argument buffers.
//!
//! This reuses Linux services, not its native userspace execution model.
//! nk must marshal user pointers and supply address spaces and process state.

extern "C" {
    /// Hand Linux its machine. Must be called before anything else.
    fn lkl_init(ops: *const u8) -> i32;
    /// Boot. Returns when the kernel is up and `lkl_syscall` may be called.
    fn lkl_start_kernel(cmdline: *const u8, ...) -> i32;
    /// One system call, by Linux's number, with Linux's argument order.
    fn lkl_syscall(no: i64, params: *mut i64) -> i64;
    /// The host operations table, in nk-host.c.
    static lkl_host_ops: u8;
}

pub fn boot() -> bool {
    crate::println!();
    crate::println!("  linux:  handing over the machine");
    let rc = unsafe { lkl_init(&raw const lkl_host_ops) };
    if rc != 0 {
        crate::println!("  linux:  lkl_init refused it: {}", rc);
        return false;
    }
    crate::println!("  linux:  starting the kernel");
    // Memory comes from nk's frame allocator through the host's page_alloc,
    // so this is how much of nk's RAM Linux is allowed to take.
    let rc = unsafe { lkl_start_kernel(c"mem=64M loglevel=8".as_ptr() as *const u8) };
    if rc != 0 {
        crate::println!("  linux:  the kernel did not start: {}", rc);
        return false;
    }
    true
}

/// Make a Linux system call. The signature every EL0 `svc` will route to.
pub fn syscall(no: i64, args: [i64; 6]) -> i64 {
    let mut params = args;
    unsafe { lkl_syscall(no, params.as_mut_ptr()) }
}

/// These helpers accept kernel-owned buffers only. They must never receive
/// an unchecked address from an EL0 exception frame.
pub fn write_file(path: &core::ffi::CStr, bytes: &[u8]) -> Result<(), i64> {
    let fd = syscall(56, [-100, path.as_ptr() as i64, 0o1101, 0o755, 0, 0]);
    if fd < 0 {
        return Err(fd);
    }
    let result = (|| {
        let mut off = 0;
        while off < bytes.len() {
            let n = syscall(
                64,
                [
                    fd,
                    bytes[off..].as_ptr() as i64,
                    (bytes.len() - off) as i64,
                    0,
                    0,
                    0,
                ],
            );
            if n <= 0 {
                return Err(if n == 0 { -5 } else { n });
            }
            off += n as usize;
        }
        Ok(())
    })();
    let close = syscall(57, [fd, 0, 0, 0, 0, 0]);
    result.and(if close < 0 { Err(close) } else { Ok(()) })
}
/// Hand Linux a virtio-mmio device at a real address.
///
/// This is the whole of how a device nk can see becomes a device Linux drives.
/// `arch/lkl` has no bus to enumerate and no device tree, so it takes the
/// address as an argument: `virtio_mmio_device_add` registers a platform
/// device with the memory and interrupt resources given, and Linux's ordinary
/// `virtio_mmio` driver binds to it and probes whatever is behind. The MMIO
/// itself reaches the hardware through `lkl_host_ops.iomem_access`, which nk
/// already implements, because nk is identity mapped and a physical address
/// is a pointer.
///
/// Returns the LKL interrupt number the device was given, which nk raises
/// when the GIC tells it that device spoke.
/// The interrupt must already be routed when this is called: registering the
/// device probes it, and a probe that has to wait for the device to answer
/// waits inside this call. Enabling the line afterwards is a boot that stops
/// with no message, in a driver that is doing exactly what it should.
pub fn add_virtio_mmio(base: u64, size: u64, irq: i32) -> bool {
    /// `__NR_arch_specific_syscall`, which is 244 on asm-generic, plus zero.
    const VIRTIO_MMIO_DEVICE_ADD: i64 = 244;
    syscall(
        VIRTIO_MMIO_DEVICE_ADD,
        [base as i64, size as i64, irq as i64, 0, 0, 0],
    ) >= 0
}

/// Create `/dev/console` and open it as descriptors 0, 1 and 2.
///
/// Linux's `/dev/console` is major 5 minor 1, and what it reaches is decided
/// by `console_device()`: it walks the registered consoles and asks each for
/// the tty driver behind it. LKL's own console has no such driver, so the one
/// `arch/lkl/drivers/nk-console.c` adds is the one that answers.
///
/// Returns whether the process now has a console. When it does not -- an
/// initrd with no `/dev`, or a kernel built without the driver -- nk falls
/// back to answering writes to 1 and 2 itself, which is enough to print and
/// not enough to redirect.
pub fn open_console() -> bool {
    const MKNODAT: i64 = 33;
    const S_IFCHR: i64 = 0o020000;
    const DUP3: i64 = 24;
    // Linux packs a device number oddly: minor bits 0..7 and 12..31, with
    // major in between. For 5:1 it is just 0x501, but spelling it out is what
    // makes that not a magic number.
    let dev = (5i64 << 8) | 1;

    let _ = mkdir(c"/dev", 0o755);
    // Not an error when it is already there: an initrd may ship one.
    syscall(MKNODAT, [-100, c"/dev/console".as_ptr() as i64, S_IFCHR | 0o600, dev, 0, 0]);

    let fd = syscall(56, [-100, c"/dev/console".as_ptr() as i64, 0o2, 0, 0, 0]);
    if fd < 0 {
        return false;
    }
    // Onto 0, 1 and 2. dup3 refuses to duplicate a descriptor onto itself, so
    // the one case that needs no work is also the one that would fail.
    for want in 0..3 {
        if want != fd && syscall(DUP3, [fd, want, 0, 0, 0, 0]) < 0 {
            return false;
        }
    }
    if fd > 2 {
        syscall(57, [fd, 0, 0, 0, 0, 0]);
    }
    true
}

/// Give the calling task a copy of another task's open descriptors.
///
/// A forked child should inherit its parent's descriptors -- redirection in a
/// shell is a `dup2` in the child of a file the parent opened -- and LKL does
/// not provide that. Its `new_thread_group_leader` clones from LKL's own init
/// task, never from the caller, so every nk process starts with an empty
/// table no matter who forked it.
///
/// Rather than patch LKL's task creation, this uses the syscalls Linux
/// already has for reaching into another process's descriptor table:
/// `pidfd_open` to name the parent and `pidfd_getfd` to pull each descriptor
/// across. That is what a debugger or a container runtime does, it needs no
/// kernel change, and it copies the *description* -- the child shares the
/// file offset with its parent, which is what fork means and what makes two
/// processes appending to the same log not overwrite each other.
///
/// Returns how many it inherited.
pub fn inherit_fds(parent_pid: i64) -> usize {
    const PIDFD_OPEN: i64 = 434;
    const PIDFD_GETFD: i64 = 438;
    const DUP3: i64 = 24;
    const CLOSE: i64 = 57;
    const FCNTL: i64 = 25;
    const F_DUPFD: i64 = 0;
    // A linear scan, because there is no "list the open descriptors" syscall
    // -- only asking about one. 64 was far more than anything here opened
    // until WebKit, which holds a library per descriptor and hands its child
    // a socket well above that: the descriptor the child was told to use
    // simply was not there, and `posix_spawn` reported EBADF.
    //
    // So the ceiling is high and the scan stops early instead: descriptors
    // are dense in practice, and a run of misses this long past the last hit
    // means there is nothing above it.
    const MAX_FD: i64 = 512;
    const GIVE_UP_AFTER: i64 = 96;

    let pidfd = syscall(PIDFD_OPEN, [parent_pid, 0, 0, 0, 0, 0]);
    if pidfd < 0 {
        return 0;
    }
    // Move the pidfd out of the range about to be filled in, and this is not
    // tidiness -- it is the whole correctness of the loop below.
    //
    // `pidfd_open` returns the lowest free descriptor, which in a child that
    // has just attached is 3. The loop then places the parent's descriptors
    // at their own numbers, and the parent's descriptor 3 landed on top of
    // the pidfd: from there every `pidfd_getfd` was asking a socket for a
    // descriptor and failing. The child got the parent's 0..4 and nothing
    // else, and WebKit's spawn died on a `dup3` of a descriptor that was
    // never copied -- EBADF, from a call that had nothing wrong with it.
    let moved = syscall(FCNTL, [pidfd, F_DUPFD, MAX_FD + 64, 0, 0, 0]);
    let pidfd = if moved >= 0 {
        syscall(CLOSE, [pidfd, 0, 0, 0, 0, 0]);
        moved
    } else {
        pidfd
    };
    let mut n = 0;
    let mut misses = 0;
    for fd in 0..MAX_FD {
        let got = syscall(PIDFD_GETFD, [pidfd, fd, 0, 0, 0, 0]);
        if got < 0 {
            // the parent has nothing there
            misses += 1;
            if misses >= GIVE_UP_AFTER {
                break;
            }
            continue;
        }
        misses = 0;
        // pidfd_getfd allocates the lowest free descriptor, which is not
        // necessarily the number the parent used -- and the number is what a
        // program depends on. Descriptors already placed are occupied, so the
        // lowest free is never one of them and moving this one cannot
        // dislodge an earlier one.
        if got != fd {
            if syscall(DUP3, [got, fd, 0, 0, 0, 0]) < 0 {
                syscall(CLOSE, [got, 0, 0, 0, 0, 0]);
                continue;
            }
            syscall(CLOSE, [got, 0, 0, 0, 0, 0]);
        }
        n += 1;
    }
    syscall(CLOSE, [pidfd, 0, 0, 0, 0, 0]);
    n
}

/// Create a directory, treating "it is already there" as success.
///
/// A cpio archive lists directories before their contents, but it does not
/// have to list them at all, so the unpacker creates parents as it goes and
/// meets the same directory twice as a matter of course.
pub fn mkdir(path: &core::ffi::CStr, mode: i64) -> Result<(), i64> {
    match syscall(34, [-100, path.as_ptr() as i64, mode, 0, 0, 0]) {
        0 => Ok(()),
        -17 => Ok(()), // -EEXIST
        e => Err(e),
    }
}

/// Write a file, creating it with the mode given rather than a fixed one.
pub fn write_file_mode(
    path: &core::ffi::CStr,
    bytes: &[u8],
    mode: i64,
) -> Result<(), i64> {
    let fd = syscall(56, [-100, path.as_ptr() as i64, 0o1101, mode, 0, 0]);
    if fd < 0 {
        return Err(fd);
    }
    let result = write_all(fd, bytes);
    let close = syscall(57, [fd, 0, 0, 0, 0, 0]);
    result.and(if close < 0 { Err(close) } else { Ok(()) })
}

fn write_all(fd: i64, bytes: &[u8]) -> Result<(), i64> {
    let mut off = 0;
    while off < bytes.len() {
        let n = syscall(
            64,
            [fd, bytes[off..].as_ptr() as i64, (bytes.len() - off) as i64, 0, 0, 0],
        );
        if n <= 0 {
            return Err(if n == 0 { -5 } else { n });
        }
        off += n as usize;
    }
    Ok(())
}

/// The largest file nk will read into its own heap in one piece. Generous
/// enough for a real init -- busybox is two megabytes and a static glibc
/// program not much less -- and bounded because this is nk's heap, not a
/// mapping, and a program is under no obligation to be a sensible size.
const MAX_FILE: i64 = 16 * 1024 * 1024;

pub fn read_file(path: &core::ffi::CStr) -> Result<alloc::vec::Vec<u8>, i64> {
    let fd = syscall(56, [-100, path.as_ptr() as i64, 0, 0, 0, 0]);
    if fd < 0 {
        return Err(fd);
    }
    let result = (|| {
        // Ask how big it is rather than growing into it. A Vec that doubles
        // its way to two megabytes holds three of them at the moment it
        // reallocates, and nk's heap is not large enough to be careless about
        // that -- which is how a 1MB ceiling ended up here, and why busybox
        // would not start.
        let mut st = [0u8; 128];
        let rc = syscall(80, [fd, st.as_mut_ptr() as i64, 0, 0, 0, 0]);
        if rc < 0 {
            return Err(rc);
        }
        let size = i64::from_le_bytes(st[48..56].try_into().unwrap());
        if !(0..MAX_FILE).contains(&size) {
            return Err(-27); // -EFBIG
        }

        let mut bytes = alloc::vec::Vec::with_capacity(size as usize);
        let mut chunk = [0u8; 4096];
        loop {
            let n = syscall(
                63,
                [fd, chunk.as_mut_ptr() as i64, chunk.len() as i64, 0, 0, 0],
            );
            if n < 0 {
                return Err(n);
            }
            if n == 0 {
                return Ok(bytes);
            }
            // The file can still grow under us between the fstat and the
            // read, and a Vec past its capacity reallocates rather than
            // failing, so the ceiling is checked here as well.
            if bytes.len() + n as usize > MAX_FILE as usize {
                return Err(-27);
            }
            bytes.extend_from_slice(&chunk[..n as usize]);
        }
    })();
    let close = syscall(57, [fd, 0, 0, 0, 0, 0]);
    if close < 0 {
        return Err(close);
    }
    result
}

/// Must be the first LKL call on a fresh nk thread. LKL's private syscall
/// 245 (arch_specific_syscall + 1) creates a thread-group leader via TLS.
/// It still shares fs/files with host0 until Linux unshare separates them.
/// Attach a *thread*: a Linux task of its own that shares its creator's
/// descriptor table.
///
/// The difference from `attach_process` is one line and the whole of
/// `CLONE_FILES`. A process wants a table of its own, so it unshares the one
/// it was cloned from; a thread must share its creator's, because that is
/// what `pthread_create` asked for. A thread with a private copy cannot see
/// descriptors opened after it started, and its `close` is not a close --
/// the creator's duplicate holds the file open, so a socket a worker
/// finished with never sends FIN and its peer waits for an EOF that never
/// comes. That is a hung HTTP response, and it is what stopped Python's
/// ThreadingHTTPServer on nk while the same sequence in one thread worked.
///
/// `share_files` is nk's, added to `arch/lkl` by `patch-lkl.py`: LKL clones
/// every task from host0 rather than from the caller, so by the time nk
/// knows whose thread this is there is no clone left to pass a flag to.
///
/// Still deliberately unshared: `CLONE_FS`. A thread gets its own cwd and
/// root, which POSIX says it should not. Nothing has needed it yet and it is
/// a separate change; this one is the descriptor table.
pub fn attach_thread(creator: i64) -> Result<i64, i64> {
    const SHARE_FILES: i64 = 246;
    let rc = syscall(245, [0; 6]);
    if rc < 0 {
        crate::println!("  attach: new_thread_group_leader -> {}", rc);
        return Err(rc);
    }
    // CLONE_FS only. Unsharing files here would undo the sharing below.
    let rc = syscall(97, [0x200, 0, 0, 0, 0, 0]);
    if rc < 0 {
        crate::println!("  attach: unshare(fs) -> {}", rc);
        return Err(rc);
    }
    let rc = syscall(SHARE_FILES, [creator, 0, 0, 0, 0, 0]);
    if rc < 0 {
        crate::println!("  attach: share_files({}) -> {}", creator, rc);
        return Err(rc);
    }
    let pid = syscall(172, [0; 6]);
    if pid <= 1 {
        return Err(-22);
    }
    Ok(pid)
}

pub fn attach_process() -> Result<i64, i64> {
    let rc = syscall(245, [0; 6]);
    if rc < 0 {
        crate::println!("  attach: new_thread_group_leader -> {}", rc);
        return Err(rc);
    }
    let rc = syscall(97, [0x200 | 0x400, 0, 0, 0, 0, 0]);
    if rc < 0 {
        crate::println!("  attach: unshare -> {}", rc);
        return Err(rc);
    }
    let pid = syscall(172, [0; 6]);
    if pid <= 1 || pid != syscall(178, [0; 6]) {
        return Err(-22);
    }
    Ok(pid)
}
