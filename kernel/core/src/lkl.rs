//! Booting Linux on nk.
//!
//! Three functions. `lkl_init` hands Linux the struct of function pointers
//! that is its machine -- nk's threads, memory, clock and console, assembled
//! in `kernel/lkl/nk-host.c`. `lkl_start_kernel` boots it. `lkl_syscall` is
//! the ABI every existing binary was compiled against.
//!
//! That last one is the whole point. nk's own syscall table has two entries
//! and would have needed two hundred more with real semantics behind them --
//! years of work, and what gVisor and Fuchsia's Starnix and FreeBSD's
//! Linuxulator each spent them on. Routing `svc` here instead answers all of
//! them at once, with Linux's own implementations, including the ones nobody
//! documents.

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
