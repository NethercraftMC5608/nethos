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
//! Each LKL process has a dedicated nk thread and Linux task.
//! Fork, exec replacement and userspace signals are not implemented.
//! LKL builds load an ELF fixture through Linux's rootfs; standalone builds
//! retain the raw smoke-test program. Neither supports a desktop runtime yet.

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

/// The bootstrap process entry state and its own translation table.
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

/// Build the standalone smoke-test address space, sharing EL1-only kernel
/// mappings. Exceptions retain this TTBR0; AT S1E0R checks EL0 permissions.
pub fn spawn() -> Process {
    let blob_start = &raw const __user_blob_start as usize;
    let blob_end = &raw const __user_blob_end as usize;
    let len = blob_end - blob_start;
    assert!(
        len <= PAGE,
        "the user program outgrew one page: {len} bytes"
    );

    let code = frames::alloc().expect("no memory for the user program");
    let stack = frames::alloc().expect("no memory for the user stack");
    unsafe { core::ptr::copy_nonoverlapping(blob_start as *const u8, code, len) };

    unsafe {
        publish_code(code, PAGE);
    }
    let ttbr0 = paging::new_address_space();
    unsafe {
        paging::map_user(ttbr0, USER_BASE, code as u64, PAGE as u64, true);
        paging::map_user(
            ttbr0,
            USER_STACK_TOP - PAGE as u64,
            stack as u64,
            PAGE as u64,
            false,
        );
    }

    println!(
        "  user:   {} bytes of program at {:#x}, stack at {:#x}, ttbr0 {:#x}",
        len, USER_BASE, USER_STACK_TOP, ttbr0
    );
    Process {
        ttbr0,
        entry: USER_BASE,
        stack: USER_STACK_TOP,
    }
}

/// Run it. Does not return: every way out of EL0 is through a vector.
pub fn run(p: &Process) -> ! {
    #[cfg(nk_lkl)]
    assert!(
        crate::sched::linux_pid(crate::sched::current_id()) > 1,
        "EL0 must run on an attached Linux process thread"
    );
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
        sys_exit(-11);
    }

    // nk owns the exit status; the host TLS destructor performs Linux's
    // do_exit for this process's backing task before the nk thread finishes.
    if matches!(frame.x[8], 93 | 94) {
        sys_exit(frame.x[0] as i32);
    }

    // The console is nk's, and only the console.
    //
    // File descriptors 1 and 2 have no meaning to Linux here: nk's process
    // has a Linux task, but nothing has opened a terminal for it and there is
    // no terminal to open. So writes to them go to nk's UART, and everything
    // else -- including writes to a descriptor the process opened itself --
    // is Linux's. This is the one place nk still answers on Linux's behalf,
    // and it goes away when there is a real console device.
    let ret = if frame.x[8] == 64 && (frame.x[0] == 1 || frame.x[0] == 2) {
        sys_write(frame.x[0], frame.x[1], frame.x[2])
    } else {
        forward(frame.x[8], &frame.x[..6].try_into().unwrap())
    };

    frame.x[0] = ret as u64;
}

/// Hand a call to Linux, with its pointers copied across.
#[cfg(nk_lkl)]
fn forward(nr: u64, args: &[u64; 6]) -> i64 {
    match crate::syscall::forward(nr, args) {
        Some(ret) => ret,
        None => {
            // Named, not merely refused. The list of what to describe next is
            // written by whatever real binary runs here, which is a better
            // order than guessing at it.
            println!("  syscall {} has no descriptor yet", nr);
            -38 // -ENOSYS
        }
    }
}

/// Without Linux there is nothing to forward to, and nk's own table is two
/// entries: the console, and exit.
#[cfg(not(nk_lkl))]
fn forward(nr: u64, _args: &[u64; 6]) -> i64 {
    println!("  syscall {} is not implemented", nr);
    -38
}

/// # write(fd, buf, count)
///
/// The exception retains the process TTBR0. Translate with EL0 permissions
/// before reading through the kernel identity map; an EL1 dereference alone
/// would also allow the caller to read kernel pages.
fn sys_write(fd: u64, buf: u64, count: u64) -> i64 {
    if fd != 1 && fd != 2 {
        return -9; // -EBADF
    }
    let count = (count as usize).min(crate::uaccess::MAX_TRANSFER);
    let mut bytes = alloc::vec![0u8; count];
    if crate::uaccess::copy_from_user(&mut bytes, buf).is_err() {
        // Said out loud, because a refusal that only shows up as an errno in
        // an exit status is a thing nobody reads.
        println!("  refused a user pointer into kernel memory (EFAULT)");
        return crate::uaccess::EFAULT;
    }
    let uart = crate::uart::console();
    for b in &bytes {
        if *b == b'\n' {
            uart.put(b'\r');
        }
        uart.put(*b);
    }
    count as i64
}

fn sys_exit(status: i32) -> ! {
    println!();
    println!("  the process exited with status {}", status);
    #[cfg(nk_lkl)]
    {
        crate::sched::set_exit_status(status);
        crate::sched::exit_current()
    }
    #[cfg(not(nk_lkl))]
    crate::stop()
}

/// Load the bootstrap ELF through Linux's existing rootfs and VFS. The file
/// is seeded from the kernel image until a persistent root disk is attached.
#[cfg(nk_lkl)]
pub fn spawn_from_rootfs() -> Result<Process, &'static str> {
    extern "C" {
        static __user_elf_start: u8;
        static __user_elf_end: u8;
    }
    let start = &raw const __user_elf_start;
    let size = (&raw const __user_elf_end as usize) - start as usize;
    let fixture = unsafe { core::slice::from_raw_parts(start, size) };
    crate::lkl::write_file(c"/nk-init", fixture).map_err(|_| "rootfs write failed")?;
    let bytes = crate::lkl::read_file(c"/nk-init").map_err(|_| "rootfs read failed")?;
    if bytes != fixture {
        return Err("rootfs round trip differs");
    }
    println!(
        "  rootfs: /nk-init read back through Linux VFS ({} bytes)",
        bytes.len()
    );
    let image = crate::elf::parse(&bytes, USER_BASE, USER_STACK_TOP - 2 * PAGE as u64)?;
    let ttbr0 = paging::new_address_space();
    for s in &image.segments {
        let base = s.address & !(PAGE as u64 - 1);
        let end = (s.address + s.memsz as u64).div_ceil(PAGE as u64) * PAGE as u64;
        for va in (base..end).step_by(PAGE) {
            let page = frames::alloc().expect("no memory for ELF");
            let lo = va.max(s.address);
            let hi = (va + PAGE as u64).min(s.address + s.filesz as u64);
            if hi > lo {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        bytes.as_ptr().add(s.offset + (lo - s.address) as usize),
                        page.add((lo - va) as usize),
                        (hi - lo) as usize,
                    );
                }
            }
            if s.executable {
                unsafe {
                    publish_code(page, PAGE);
                }
            }
            unsafe {
                paging::map_user_permissions(
                    ttbr0,
                    va,
                    page as u64,
                    PAGE as u64,
                    s.executable,
                    s.writable,
                );
            }
        }
    }
    let stack = frames::alloc().expect("no memory for ELF stack");
    // Empty argc/argv/envp/auxv terminators. Dynamic libc startup is not yet
    // supported; this fixture uses the syscall ABI directly.
    unsafe {
        paging::map_user(
            ttbr0,
            USER_STACK_TOP - PAGE as u64,
            stack as u64,
            PAGE as u64,
            false,
        );
        core::arch::asm!("dsb ish", "ic iallu", "dsb ish", "isb", options(nostack));
    }
    println!(
        "  ELF: {} PT_LOAD segment(s), entry {:#x}, zero-filled BSS",
        image.segments.len(),
        image.entry
    );
    Ok(Process {
        ttbr0,
        entry: image.entry,
        stack: USER_STACK_TOP - 48,
    })
}

/// Publish freshly copied instructions through the physical identity alias.
/// D-cache writes must reach PoU before invalidating the I-cache; a barrier
/// alone does not clean dirty cache lines on machines without IDC coherence.
unsafe fn publish_code(start: *mut u8, len: usize) {
    let ctr: u64;
    core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nostack, nomem));
    let line = 4usize << ((ctr >> 16) & 15);
    for address in ((start as usize & !(line - 1))..start as usize + len).step_by(line) {
        core::arch::asm!("dc cvau, {}", in(reg) address, options(nostack));
    }
    core::arch::asm!("dsb ish", "ic iallu", "dsb ish", "isb", options(nostack));
}

#[cfg(nk_lkl)]
pub fn launch(p: Process, prepare: Option<fn(i64)>) -> usize {
    let arg = alloc::boxed::Box::into_raw(alloc::boxed::Box::new((p, prepare))) as usize;
    let flags = crate::sync::irq_save();
    let id = crate::sched::spawn("process", process_entry, arg);
    unsafe {
        crate::sync::irq_restore(flags);
    }
    id
}

#[cfg(nk_lkl)]
extern "C" fn process_entry(arg: usize) {
    let (p, prepare) =
        *unsafe { alloc::boxed::Box::from_raw(arg as *mut (Process, Option<fn(i64)>)) };
    let pid = crate::lkl::attach_process().expect("Linux process attach failed");
    crate::sched::bind_linux_pid(pid);
    println!(
        "  process: nk {} Linux pid {} tid {}",
        crate::sched::current_id(),
        pid,
        crate::lkl::syscall(178, [0; 6])
    );
    if let Some(prepare) = prepare {
        prepare(pid);
    }
    run(&p);
}

#[no_mangle]
pub extern "C" fn rust_el0_irq() {
    crate::sched::record_user_irq();
    crate::exceptions::rust_irq();
}
