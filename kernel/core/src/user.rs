//! User space: an address space, a program in it, and the syscalls it makes.
//!
//! This is the gate. Everything nk has done so far -- the drivers, the shim,
//! the block and network stacks -- lives entirely inside the kernel and is
//! reachable only by nk's own code calling it. A desktop is not that. It is
//! several hundred existing binaries, compiled years ago against Linux's
//! syscall ABI, that will never be recompiled. Nothing above the driver layer
//! is possible until a program can run at EL0 and be answered.
//!
//! So the numbers here are Linux's, not nk's: `write` is 64 and `exit` is 93
//! because that is what aarch64 Linux uses and what those binaries contain.
//! Inventing a cleaner numbering would be inventing a system that nothing can
//! be run on, which is the whole thing being avoided.
//!
//! **What this is not, yet.** One process, no fork, no exec, no scheduler
//! involvement, no filesystem, no signals, and the program is a few
//! instructions in the kernel image rather than an ELF file. The address
//! space is built by hand and never torn down. Each of those is a real piece
//! of work and each is separable; what is here is the mechanism they all
//! attach to.

use crate::frames::{self, PAGE};
use crate::paging;
use crate::println;

/// Where a process's image goes.
///
/// Above RAM and below the vmemmap, in a gap that nothing else uses. Not a
/// design so much as an available hole: the kernel is identity mapped over
/// the bottom of the address space, so user addresses cannot start at zero
/// the way they do on Linux without colliding with it. Moving the kernel to
/// the top half is what fixes that, and it is the next structural change.
/// 512GiB: the second top-level table entry, which the kernel does not use.
/// Chosen so that building a process's mappings cannot touch the tables the
/// kernel shares with every other address space.
pub const USER_BASE: u64 = 0x80_0000_0000;
pub const USER_STACK_TOP: u64 = USER_BASE + 0x10_0000;

/// A process. One page of code, one of stack, and its own translation table.
pub struct Process {
    pub ttbr0: u64,
    pub entry: u64,
    pub stack: u64,
}

extern "C" {
    static __user_blob_start: u8;
    static __user_blob_end: u8;
    fn enter_user(entry: u64, stack: u64, ttbr0: u64) -> !;
}

/// Build an address space containing only the program.
///
/// Only the program: no kernel mapping at all. That is possible because the
/// kernel runs from TTBR1's half in Linux, and in nk it works for a
/// different reason -- exceptions from EL0 switch to SP_EL1 and run kernel
/// code that is mapped by *the kernel's own* TTBR0, which is restored on the
/// way out. It is a real constraint rather than a design: it means kernel
/// code cannot currently read a user pointer directly, which is the first
/// thing `write` would want to do.
pub fn spawn() -> Process {
    let blob_start = &raw const __user_blob_start as usize;
    let blob_end = &raw const __user_blob_end as usize;
    let len = blob_end - blob_start;
    assert!(len <= PAGE, "the user program outgrew one page: {len} bytes");

    let code = frames::alloc().expect("no memory for the user program");
    let stack = frames::alloc().expect("no memory for the user stack");
    unsafe { core::ptr::copy_nonoverlapping(blob_start as *const u8, code, len) };

    let ttbr0 = paging::new_address_space();
    unsafe {
        paging::map_user(ttbr0, USER_BASE, code as u64, PAGE as u64, true);
        paging::map_user(ttbr0, USER_STACK_TOP - PAGE as u64, stack as u64, PAGE as u64, false);
    }

    println!(
        "  user:   {} bytes of program at {:#x}, stack at {:#x}, ttbr0 {:#x}",
        len, USER_BASE, USER_STACK_TOP, ttbr0
    );
    Process { ttbr0, entry: USER_BASE, stack: USER_STACK_TOP }
}

/// Run it. Does not return: every way out of EL0 is through a vector.
pub fn run(p: &Process) -> ! {
    println!();
    println!("  entering EL0...");
    println!();
    unsafe { enter_user(p.entry, p.stack, p.ttbr0) }
}

/// The register frame `el0_sync_entry` builds, in the order it writes it.
#[repr(C)]
pub struct Frame {
    pub x: [u64; 31],
    pub elr: u64,
    pub spsr: u64,
    pub sp: u64,
}

/// Put SP_EL0 back to meaning "the current task" now that the user's value is
/// safely in the frame. Every Linux file the shim compiles reads its stack
/// canary through it, so kernel code cannot run correctly without it.
#[no_mangle]
pub extern "C" fn nk_enter_kernel() {
    crate::sched::restore_task_ptr();
}

/// Everything that arrives from EL0 synchronously: system calls, and faults.
#[no_mangle]
pub extern "C" fn rust_el0_sync(frame: &mut Frame) {
    let esr: u64;
    unsafe { core::arch::asm!("mrs {}, esr_el1", out(reg) esr, options(nomem, nostack)) };
    let ec = (esr >> 26) & 0x3f;

    // 0b010101 is SVC from AArch64. Anything else from EL0 is a fault, and a
    // fault in user space must not stop the kernel -- reporting it and
    // stopping the *process* is the whole difference between the two
    // privilege levels being worth having.
    if ec != 0b010101 {
        println!();
        println!("!! fault in user space: esr {:#x} ec {:#b}", esr, ec);
        println!("   pc {:#x}  sp {:#x}", frame.elr, frame.sp);
        // The process is what should die here, not the machine. nk has
        // nothing else to run yet, so it stops -- but reporting it as a user
        // fault rather than a kernel one is the distinction the whole
        // privilege boundary exists to make.
        crate::stop();
    }

    let ret = syscall(frame.x[8], &frame.x[..6]);
    frame.x[0] = ret as u64;
}

/// The syscall table. Two entries, and the numbers are Linux's.
fn syscall(nr: u64, args: &[u64]) -> i64 {
    match nr {
        64 => sys_write(args[0], args[1], args[2]),
        93 => sys_exit(args[0] as i32),
        _ => {
            // Named rather than silently refused: the interesting question
            // from here on is *which* calls a real binary makes, and a log of
            // the ones nk does not have is the list of what to do next.
            println!("  syscall {} is not implemented", nr);
            -38 // -ENOSYS
        }
    }
}

/// # write(fd, buf, count)
///
/// The buffer is a *user* pointer, and the kernel is not running in the
/// process's address space -- TTBR0 was restored to the kernel's on the way
/// in. So it cannot simply be dereferenced, and the copy has to go through
/// the process's own translation. That is `copy_from_user`, and this is the
/// smallest possible version of it.
fn sys_write(fd: u64, buf: u64, count: u64) -> i64 {
    if fd != 1 && fd != 2 {
        return -9; // -EBADF
    }
    let mut copied = 0usize;
    let uart = crate::uart::console();
    while copied < count as usize {
        let Some(pa) = paging::user_to_phys(buf + copied as u64) else {
            return if copied == 0 { -14 } else { copied as i64 }; // -EFAULT
        };
        let byte = unsafe { core::ptr::read_volatile(pa as *const u8) };
        if byte == b'\n' {
            uart.put(b'\r');
        }
        uart.put(byte);
        copied += 1;
    }
    copied as i64
}

fn sys_exit(status: i32) -> ! {
    println!();
    println!("  the process exited with status {}", status);
    crate::stop()
}
