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
pub fn read_file(path: &core::ffi::CStr) -> Result<alloc::vec::Vec<u8>, i64> {
    let fd = syscall(56, [-100, path.as_ptr() as i64, 0, 0, 0, 0]);
    if fd < 0 {
        return Err(fd);
    }
    let result = (|| {
        let mut bytes = alloc::vec::Vec::new();
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
            if bytes.len() + n as usize > 1024 * 1024 {
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
pub fn attach_process() -> Result<i64, i64> {
    let rc = syscall(245, [0; 6]);
    if rc < 0 {
        return Err(rc);
    }
    let rc = syscall(97, [0x200 | 0x400, 0, 0, 0, 0, 0]);
    if rc < 0 {
        return Err(rc);
    }
    let pid = syscall(172, [0; 6]);
    if pid <= 1 || pid != syscall(178, [0; 6]) {
        return Err(-22);
    }
    Ok(pid)
}
