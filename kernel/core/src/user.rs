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
use alloc::vec::Vec;
use crate::println;

/// Where a process's image goes: 0x400000, which is where aarch64 links a
/// non-PIE executable, so an ordinary binary needs no relocation to run here.
///
/// Getting to this address took narrowing the device map to the 34MB the
/// machine actually has, and then finding that user pages were mapped global
/// in an address space with ASID 0 -- so the kernel's own walking of the low
/// half poisoned the process's translations, under HVF only. See
/// docs/KERNEL.md.
pub const USER_BASE: u64 = 0x0040_0000;
pub const USER_STACK_TOP: u64 = 0x1000_0000;

/// Where anonymous mappings start, growing downward.
///
/// Between the heap growing up from the end of the image and this growing
/// down, the two run out of room by meeting -- which nk can detect and refuse
/// -- rather than by one silently landing on the other.
pub const USER_MMAP_TOP: u64 = USER_STACK_TOP - 16 * 1024 * 1024;

/// How much stack a process starts with. One page was enough for a program
/// written in assembly and is nowhere near enough for a libc, which sets up
/// TLS, locale and stdio buffers before it reaches `main`. Mapped up front
/// rather than grown on a fault, because nk has no fault handler that could
/// tell a stack from a wild pointer yet.
pub const USER_STACK_SIZE: usize = 256 * 1024;

/// The bootstrap process entry state and its own translation table.
pub struct Process {
    pub ttbr0: u64,
    pub entry: u64,
    pub stack: u64,
    /// The first address above the loaded image: where the heap starts.
    pub brk: u64,
}

extern "C" {
    static __user_blob_start: u8;
    static __user_blob_end: u8;
    fn enter_user(entry: u64, stack: u64, ttbr0: u64) -> !;
    #[cfg(nk_lkl)]
    fn enter_user_fresh(entry: u64, stack: u64, ttbr0: u64, kernel_sp: u64) -> !;
    #[cfg(nk_lkl)]
    fn resume_user(frame: *const Frame, ttbr0: u64, kernel_sp: u64) -> !;
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
    let stack = frames::alloc_contiguous(USER_STACK_SIZE / PAGE)
        .expect("no memory for the user stack");
    unsafe { core::ptr::copy_nonoverlapping(blob_start as *const u8, code, len) };

    unsafe {
        publish_code(code, PAGE);
    }
    let ttbr0 = paging::new_address_space();
    unsafe {
        paging::map_user(ttbr0, USER_BASE, code as u64, PAGE as u64, true);
        paging::map_user(
            ttbr0,
            USER_STACK_TOP - USER_STACK_SIZE as u64,
            stack as u64,
            USER_STACK_SIZE as u64,
            false,
        );
    }

    // The same initial stack a loaded binary gets. The smoke test is the same
    // program, and it checks the layout before it does anything else, so
    // building it only on the Linux path would leave this one entering EL0
    // with a stack pointer one byte past its own page.
    let random = stack_seed().expect("no stack guard");
    let sp = unsafe { crate::stack::Builder::new(stack, USER_STACK_TOP, USER_STACK_SIZE) }
        .build(
            &[b"/nk-init"],
            &[b"PATH=/bin", b"HOME=/"],
            &[(crate::stack::AT_PAGESZ, PAGE as u64), (crate::stack::AT_ENTRY, USER_BASE)],
            &random,
        )
        .expect("the initial stack does not fit in one page");

    println!(
        "  user:   {} bytes of program at {:#x}, stack at {:#x}, ttbr0 {:#x}",
        len, USER_BASE, sp, ttbr0
    );
    Process {
        ttbr0,
        entry: USER_BASE,
        stack: sp,
        brk: USER_BASE + PAGE as u64,
    }
}

/// Run it. Does not return: every way out of EL0 is through a vector.
pub fn run(p: &Process) -> ! {
    println!();
    println!("  entering EL0...");
    println!();
    // The memory layout belongs to the running task, because `brk` and `mmap`
    // are answered from whichever thread makes the call, and this is it.
    crate::sched::set_user_memory(p.brk, USER_MMAP_TOP);
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
        let far: u64;
        unsafe { core::arch::asm!("mrs {}, far_el1", out(reg) far, options(nomem, nostack)) };
        println!("!! fault in user space: esr {:#x} ec {:#b} far {:#x}", esr, ec, far);
        println!("   pc {:#x}  sp {:#x}  lr {:#x}", frame.elr, frame.sp, frame.x[30]);
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
    // Only when the process has no real console. With one, descriptor 1 is an
    // ordinary Linux descriptor and may well be a file -- `ls > out` is a
    // dup2 onto it -- so answering by number would send a shell's output to
    // the UART no matter what it had been redirected to.
    let console = (frame.x[0] == 1 || frame.x[0] == 2) && !crate::sched::has_console();
    let ret = if frame.x[8] == 64 && console {
        sys_write(frame.x[0], frame.x[1], frame.x[2])
    } else if frame.x[8] == 66 && console {
        // The same rule as `write`, and it has to be here too because this is
        // the call a libc `printf` actually makes.
        sys_writev(frame.x[0], frame.x[1], frame.x[2])
    } else if frame.x[8] == 435 {
        // clone3 describes an EL0 context, not an LKL kernel-thread entry.
        // libc will fall back to clone; forwarding it can call a null fn.
        -38
    } else if frame.x[8] == 98 && cfg!(nk_lkl) {
        // The futex is nk's because the address space is: see futex.rs.
        futex(frame)
    } else if matches!(frame.x[8], 220 | 260) {
        // clone and wait4. Both are nk's for the same reason execve is: the
        // address space and the exit status are nk's, not Linux's.
        process(frame)
    } else if frame.x[8] == 221 && cfg!(nk_lkl) {
        // execve does not return, so it is not part of the dispatch below:
        // either it replaces the program or it fails and says why. It needs
        // Linux only to read the file; the replacing is nk's own.
        exec(frame.x[0], frame.x[1], frame.x[2])
    } else if matches!(frame.x[8], 214 | 222 | 215 | 226 | 96 | 99 | 293 | 261) {
        // The process's address space is nk's, not Linux's. LKL is one flat
        // region with no user half at all, so forwarding these would move
        // Linux's own break and hand back an address this process cannot
        // reach. They are the calls nk has to answer itself.
        match frame.x[8] {
            214 => sys_brk(frame.x[0]),
            222 => sys_mmap(frame.x[0], frame.x[1], frame.x[2], frame.x[3], frame.x[4] as i64, frame.x[5]),
            215 => sys_munmap(frame.x[0], frame.x[1]),
            226 => sys_mprotect(frame.x[0], frame.x[1], frame.x[2]),
            96 => sys_set_tid_address(),
            261 => sys_prlimit64(frame.x[2], frame.x[3]),
            // set_robust_list and rseq. Both are optimisations a libc asks
            // for and does without: glibc checks the return and falls back,
            // so -ENOSYS is the honest answer and pretending to have
            // registered a robust list nk would never walk is not.
            _ => -38,
        }
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
        None => -38, // -ENOSYS: no Linux to forward to
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

/// # writev(fd, iov, iovcnt)
///
/// The console's scatter/gather form. Each entry is checked and copied on its
/// own, so a bad pointer half way down refuses without having invented a
/// short write: nothing is emitted until every entry has been read.
fn sys_writev(fd: u64, iov: u64, count: u64) -> i64 {
    if fd != 1 && fd != 2 {
        return -9; // -EBADF
    }
    if count > 1024 {
        return -22; // -EINVAL, and Linux's own UIO_MAXIOV
    }
    let mut raw = alloc::vec![0u8; count as usize * 16];
    if crate::uaccess::copy_from_user(&mut raw, iov).is_err() {
        println!("  refused a user pointer into kernel memory (EFAULT)");
        return crate::uaccess::EFAULT;
    }

    let mut bytes: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    for i in 0..count as usize {
        let e = &raw[i * 16..];
        let base = u64::from_le_bytes(e[0..8].try_into().unwrap());
        let len = u64::from_le_bytes(e[8..16].try_into().unwrap()) as usize;
        if len == 0 {
            continue;
        }
        if bytes.len() + len > crate::uaccess::MAX_TRANSFER {
            return -22;
        }
        let at = bytes.len();
        bytes.resize(at + len, 0);
        if crate::uaccess::copy_from_user(&mut bytes[at..], base).is_err() {
            println!("  refused a user pointer into kernel memory (EFAULT)");
            return crate::uaccess::EFAULT;
        }
    }

    let uart = crate::uart::console();
    for b in &bytes {
        if *b == b'\n' {
            uart.put(b'\r');
        }
        uart.put(*b);
    }
    bytes.len() as i64
}

/// How much of the address space one process may claim. A runaway `brk` loop
/// should be told no, not allowed to exhaust the machine's memory on behalf
/// of a program that has already gone wrong.
const USER_MEM_LIMIT: u64 = 64 * 1024 * 1024;

/// # brk(addr)
///
/// Returns the break. `brk(0)` asks for it; anything else sets it, and the
/// return value is what it actually became -- Linux's brk reports failure by
/// returning the *old* break, never an errno, and a libc that gets an errno
/// here will not recognise it.
fn sys_brk(addr: u64) -> i64 {
    let (brk, brk_min, mmap_next) = crate::sched::user_memory();
    if brk_min == 0 {
        return brk as i64; // not a process with a heap
    }
    let want = addr.div_ceil(PAGE as u64) * PAGE as u64;
    if addr == 0 || want < brk_min || want >= mmap_next || want - brk_min > USER_MEM_LIMIT {
        return brk as i64;
    }
    if want > brk {
        if !map_anonymous(brk, want - brk) {
            return brk as i64;
        }
    } else if want < brk {
        unsafe { crate::paging::unmap_user(current_ttbr0(), want, brk - want) };
    }
    crate::sched::set_user_brk(want);
    // The *requested* address, not the page it rounded up to. Linux tracks
    // the break at byte granularity even though it maps whole pages, and a
    // libc told it got more than it asked for will hand the difference out
    // twice.
    addr as i64
}

/// # mmap(addr, len, prot, flags, fd, off)
///
/// Anonymous/private file mappings, eagerly populated through Linux pread.
/// MAP_SHARED and file-page coherence/writeback are not implemented.
fn sys_mmap(addr: u64, length: u64, prot: u64, flags: u64, fd: i64, offset: u64) -> i64 {
    // Private file mappings are snapshots: Linux reads its filesystem into
    // nk-owned pages. Shared/writeback mappings need a different contract.
    if length == 0 || length > USER_MEM_LIMIT || offset & 4095 != 0 || prot & !7 != 0 {
        return -22;
    }
    if flags & !(0x2 | 0x10 | 0x20 | 0x800 | 0x1000 | 0x20000) != 0 { return -95; }
    if flags & 3 != 2 { return -95; } // MAP_PRIVATE only
    if prot & 6 == 6 { return -13; }
    let len = length.div_ceil(4096)*4096;
    let (brk, _, next) = crate::sched::user_memory();
    let fixed = flags & 0x10 != 0;
    let at = if fixed { addr } else { match next.checked_sub(len) { Some(v) => v, None => return -12 } };
    let Some(end) = at.checked_add(len) else { return -22; };
    // The current low-half layout still contains QEMU's devices. Never
    // replace those inherited mappings, even for a caller using MAP_FIXED.
    if at & 4095 != 0 || at < brk || end > USER_MMAP_TOP || (at < 0x0a20_0000 && end > 0x0800_0000) {
        return -12;
    }
    let anonymous = flags & 0x20 != 0;
    let mut pages = Vec::new();
    for off in (0..len).step_by(PAGE) {
        let Some(page) = frames::alloc() else {
            for page in pages { unsafe { frames::free(page); } }
            return -12;
        };
        unsafe { core::ptr::write_bytes(page,0,PAGE); }
        if !anonymous {
            #[cfg(nk_lkl)]
            let rc = crate::lkl::syscall(67, [fd, page as i64, PAGE as i64,
                match offset.checked_add(off) { Some(v) if v <= i64::MAX as u64 => v as i64, _ => -1 },0,0]);
            #[cfg(not(nk_lkl))]
            let rc = -38;
            if rc < 0 {
                unsafe { frames::free(page); }
                for page in pages { unsafe { frames::free(page); } }
                return rc;
            }
        }
        if prot & 4 != 0 { unsafe { publish_code(page, PAGE); } }
        pages.push(page);
    }
    let root = current_ttbr0();
    if fixed { unsafe { paging::unmap_user(root,at,len); } }
    for (i,page) in pages.into_iter().enumerate() {
        unsafe { paging::map_user_permissions(root,at+i as u64*4096,page as u64,4096,prot&4!=0,prot&2!=0); }
    }
    if prot == 0 { unsafe { paging::protect_user_none(root, at, len); } }
    if !fixed { crate::sched::set_user_mmap_next(at); }
    at as i64
}

/// # munmap(addr, len)
///
/// Unmapping a hole is legal and returns success, which is what makes it safe
/// for a libc to call over a range it is not sure about. The addresses are
/// not reused: `mmap_next` only ever falls, so a freed region stays free
/// rather than being handed out again while something still holds a pointer
/// into it. That wastes address space and not memory, and the pages
/// themselves do go back.
fn sys_munmap(addr: u64, len: u64) -> i64 {
    let (_, brk_min, _) = crate::sched::user_memory();
    if brk_min == 0 || len == 0 {
        return -22; // -EINVAL
    }
    let start = addr & !(PAGE as u64 - 1);
    let end = (addr + len).div_ceil(PAGE as u64) * PAGE as u64;
    if start < brk_min || end <= start || end > USER_STACK_TOP {
        return -22;
    }
    unsafe { crate::paging::unmap_user(current_ttbr0(), start, end - start) };
    0
}

#[cfg(nk_lkl)]
fn futex(frame: &Frame) -> i64 {
    crate::futex::futex(
        frame.x[0],
        frame.x[1],
        frame.x[2] as u32,
        frame.x[3],
        frame.x[4],
        frame.x[5] as u32,
    )
}

#[cfg(not(nk_lkl))]
fn futex(_frame: &Frame) -> i64 {
    -38
}

/// clone and wait4, or -ENOSYS on a build with no Linux behind them.
#[cfg(nk_lkl)]
fn process(frame: &Frame) -> i64 {
    if frame.x[8] == 220 {
        fork(frame)
    } else {
        wait4(frame.x[0] as i64, frame.x[1], frame.x[2])
    }
}

#[cfg(not(nk_lkl))]
fn process(frame: &Frame) -> i64 {
    println!("  syscall {} needs a Linux task to attach to", frame.x[8]);
    -38
}

/// What a forked child needs to become itself, handed to its new nk thread.
#[cfg(nk_lkl)]
struct Forked {
    frame: Frame,
    ttbr0: u64,
    brk: u64,
    brk_min: u64,
    mmap_next: u64,
    tpidr: u64,
    /// Whose descriptors the child is to inherit.
    parent_pid: i64,
    /// A thread shares its creator's address space rather than owning a copy.
    /// The failure path has to know: freeing it would take the address space
    /// out from under the threads still running in it.
    shares_mm: bool,
    /// Addresses the clone asked to have the new thread id written to, or 0.
    /// Written by the thread itself rather than by its creator: the creator
    /// only regains control after the thread is already running, by which
    /// time a short thread may have exited and cleared the very word we are
    /// about to write. Linux writes these in copy_process for that reason.
    set_tid: (u64, u64),
    /// Signalled once the child has a Linux pid, because `fork` has to return
    /// that pid to the parent and only the child can obtain one: attaching
    /// binds the Linux task to the host thread it runs on.
    ready: &'static crate::sync::Semaphore,
    pid: &'static core::sync::atomic::AtomicI64,
}

/// # clone(flags, stack, ...) -- but only the shape `fork` uses
///
/// A real `clone` is a menu: sharing memory makes a thread, sharing nothing
/// makes a process, and the flags say which. nk implements the one column of
/// that menu it can honour completely, and refuses the rest rather than
/// silently giving a thread its own memory -- which would look like it worked
/// until two of them disagreed about a variable.
#[cfg(nk_lkl)]
fn fork(frame: &Frame) -> i64 {
    use core::sync::atomic::{AtomicI64, Ordering};

    // glibc's fork() is clone(CLONE_CHILD_CLEARTID|CLONE_CHILD_SETTID|SIGCHLD,
    // 0, ...). The tid flags concern a pointer nk does not write to, and
    // SIGCHLD is the exit signal, which nk does not deliver -- wait4 is how a
    // parent finds out here. Anything asking to share memory or files is a
    // thread and is refused.
    const CLONE_VM: u64 = 0x0100;
    const CLONE_THREAD: u64 = 0x00010000;
    if frame.x[0] & CLONE_THREAD != 0 {
        return thread(frame);
    }
    if frame.x[0] & CLONE_VM != 0 {
        // Sharing memory without being a thread is vfork, and vfork's promise
        // -- the parent stops until the child execs or exits -- is a thing nk
        // would have to implement rather than approximate.
        println!("  clone: CLONE_VM without CLONE_THREAD is not implemented");
        return -38; // -ENOSYS
    }

    let parent = current_ttbr0();
    let Some(ttbr0) = (unsafe { paging::copy_user_address_space(parent) }) else {
        return -12; // -ENOMEM
    };

    let (brk, brk_min, mmap_next) = crate::sched::user_memory();
    let tpidr: u64;
    unsafe { core::arch::asm!("mrs {}, tpidr_el0", out(reg) tpidr, options(nomem, nostack)) };

    // Leaked on purpose: the child reads them on its own thread after this
    // one has returned to EL0, so they cannot live in this stack frame. One
    // pair per fork is a real cost and the honest fix is a slab of them,
    // which is worth doing when fork is on a hot path and not before.
    let ready: &'static crate::sync::Semaphore =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(crate::sync::Semaphore::new(0)));
    let pid: &'static AtomicI64 =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(AtomicI64::new(0)));

    let mut child_frame = Frame { x: frame.x, elr: frame.elr, spsr: frame.spsr, sp: frame.sp };
    child_frame.x[0] = 0; // what fork returns in the child

    let arg = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(Forked {
        frame: child_frame,
        ttbr0,
        brk,
        brk_min,
        mmap_next,
        tpidr,
        parent_pid: crate::sched::linux_pid(crate::sched::current_id()),
        shares_mm: false,
        set_tid: (0, 0),
        ready,
        pid,
    })) as usize;

    let me = crate::sched::current_id();
    let flags = crate::sync::irq_save();
    let id = crate::sched::spawn("forked", forked_entry, arg);
    crate::sched::set_parent(id, me);
    unsafe { crate::sync::irq_restore(flags) };

    // Wait for the child to have a pid. fork returns it, and only the child
    // can get one -- attaching binds the Linux task to the host thread that
    // does the attaching.
    ready.down();
    pid.load(Ordering::Acquire)
}

#[cfg(nk_lkl)]
extern "C" fn forked_entry(arg: usize) {
    use core::sync::atomic::Ordering;
    let f = *unsafe { alloc::boxed::Box::from_raw(arg as *mut Forked) };

    // A Linux task of its own, so the child has its own pid and its own view
    // of the filesystem. This is *not* a copy of the parent's: descriptors
    // the parent had open are not inherited, which real fork does inherit and
    // a shell will need. It is a limitation, not a design.
    // A process that cannot fork is a failed fork, not a dead kernel: the
    // parent gets an errno and decides what to do about it.
    let pid = match crate::lkl::attach_process() {
        Ok(pid) => pid,
        Err(e) => {
            f.pid.store(e, Ordering::Release);
            f.ready.up();
            if !f.shares_mm {
                unsafe { paging::destroy_user_address_space(f.ttbr0) };
            }
            return;
        }
    };
    crate::sched::bind_linux_pid(pid);
    // Both before ready.up(): the creator must not observe the thread until
    // its id is where the caller asked for it, and the thread must not reach
    // user code — where it could exit and clear these words — before then.
    let bytes = (pid as u32).to_le_bytes();
    for addr in [f.set_tid.0, f.set_tid.1] {
        if addr != 0 {
            let _ = crate::uaccess::copy_to_user(addr, &bytes);
        }
    }
    // Before the parent is told the child exists, so the parent cannot close
    // a descriptor between forking and the child copying it.
    let inherited = crate::lkl::inherit_fds(f.parent_pid);
    f.pid.store(pid, Ordering::Release);
    f.ready.up();
    if inherited > 0 && !f.shares_mm {
        println!("  fork: child {} inherited {} descriptors", pid, inherited);
    }

    crate::sched::set_user_memory_full(f.brk, f.brk_min, f.mmap_next);
    unsafe {
        core::arch::asm!("msr tpidr_el0, {}", in(reg) f.tpidr, options(nomem, nostack));
        resume_user(
            &f.frame,
            f.ttbr0,
            crate::sched::kernel_stack_top() as u64,
        )
    }
}

/// # clone(flags, stack, parent_tid, tls, child_tid) -- the thread column
///
/// A thread is the same program in the same address space with a stack of its
/// own. So there is no copy here: the new task takes its creator's `TTBR0`
/// unchanged, and the scheduler is told not to tear that address space down
/// when the thread exits, because the threads it shares with are still in it.
///
/// What arrives in the new thread is the caller's register frame with three
/// things changed -- `x0` is zero, the stack pointer is the one the caller
/// passed, and `TPIDR_EL0` is the thread pointer it passed. glibc puts its
/// whole thread-local area behind that pointer, so a thread with its parent's
/// is a thread sharing its parent's `errno`.
///
/// On aarch64 the argument order is flags, stack, parent_tid, **tls**,
/// child_tid -- the last two are the other way round on x86, and getting it
/// wrong gives a thread whose thread pointer is a pointer to a thread id.
#[cfg(nk_lkl)]
fn thread(frame: &Frame) -> i64 {
    use core::sync::atomic::{AtomicI64, Ordering};
    const CLONE_PARENT_SETTID: u64 = 0x00100000;
    const CLONE_CHILD_CLEARTID: u64 = 0x00200000;
    const CLONE_CHILD_SETTID: u64 = 0x01000000;
    const CLONE_SETTLS: u64 = 0x00080000;

    let flags_arg = frame.x[0];
    let stack = frame.x[1];
    let parent_tid = frame.x[2];
    let tls = frame.x[3];
    let child_tid = frame.x[4];

    if stack == 0 {
        // A thread with no stack of its own would run on its creator's, which
        // is the one thing sharing an address space makes possible and fatal.
        return -22; // -EINVAL
    }

    let ready: &'static crate::sync::Semaphore =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(crate::sync::Semaphore::new(0)));
    let tid: &'static AtomicI64 =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(AtomicI64::new(0)));

    let mut new = Frame { x: frame.x, elr: frame.elr, spsr: frame.spsr, sp: stack };
    new.x[0] = 0;

    let (brk, brk_min, mmap_next) = crate::sched::user_memory();
    let mine: u64;
    unsafe { core::arch::asm!("mrs {}, tpidr_el0", out(reg) mine, options(nomem, nostack)) };

    let arg = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(Forked {
        frame: new,
        ttbr0: current_ttbr0(),
        brk,
        brk_min,
        mmap_next,
        tpidr: if flags_arg & CLONE_SETTLS != 0 { tls } else { mine },
        parent_pid: crate::sched::linux_pid(crate::sched::current_id()),
        shares_mm: true,
        set_tid: (
            if flags_arg & CLONE_PARENT_SETTID != 0 { parent_tid } else { 0 },
            if flags_arg & CLONE_CHILD_SETTID != 0 { child_tid } else { 0 },
        ),
        ready,
        pid: tid,
    })) as usize;

    let me = crate::sched::current_id();
    let flags = crate::sync::irq_save();
    let id = crate::sched::spawn("thread", forked_entry, arg);
    crate::sched::set_parent(id, me);
    crate::sched::set_thread(
        id,
        if flags_arg & CLONE_CHILD_CLEARTID != 0 { child_tid } else { 0 },
    );
    unsafe { crate::sync::irq_restore(flags) };

    ready.down();
    let got = tid.load(Ordering::Acquire);
    if got < 0 {
        return got;
    }

    got
}

/// # wait4(pid, status, options, rusage)
///
/// nk's, because nk owns the exit status: a process's status is recorded when
/// it calls `exit`, and Linux's own task is torn down with `do_exit(0)`
/// underneath. Only children of the caller, which is why the scheduler
/// records who forked whom.
#[cfg(nk_lkl)]
fn wait4(pid: i64, status: u64, options: u64) -> i64 {
    const WNOHANG: u64 = 1;
    let me = crate::sched::current_id();
    loop {
        if let Some(id) = crate::sched::finished_child(me, pid) {
            let child = crate::sched::linux_pid(id);
            let code = crate::sched::exit_status(id);
            crate::sched::reap_process(id);
            if status != 0 {
                // A wait status is not an exit code: the low byte says how it
                // died and the second says with what, so a normal exit is the
                // code shifted up by eight. A libc's WEXITSTATUS undoes
                // exactly this and gets nonsense from a plain code.
                let w = ((code as u32 & 0xff) << 8).to_le_bytes();
                if crate::uaccess::copy_to_user(status, &w).is_err() {
                    return crate::uaccess::EFAULT;
                }
            }
            return child;
        }
        if !crate::sched::has_live_child(me, pid) {
            return -10; // -ECHILD
        }
        if options & WNOHANG != 0 {
            return 0;
        }
        crate::sched::yield_now();
    }
}

/// `execve`, or -ENOSYS on a build with no Linux to read the file through.
#[cfg(nk_lkl)]
fn exec(path: u64, argv: u64, envp: u64) -> i64 {
    sys_execve(path, argv, envp)
}

#[cfg(not(nk_lkl))]
fn exec(_path: u64, _argv: u64, _envp: u64) -> i64 {
    println!("  execve needs a filesystem to read from");
    -38
}

/// How many arguments and environment entries `execve` will carry, and how
/// many bytes of them. Linux's own limits are a quarter of the stack rlimit
/// and 32 pages per string; these are smaller because nk's initial stack is
/// one fixed allocation and everything has to fit in it beside the vector.
#[cfg(nk_lkl)]
const MAX_ARGS: usize = 64;
#[cfg(nk_lkl)]
const MAX_ARG_BYTES: usize = 16 * 1024;

/// Copy a NULL-terminated array of user string pointers.
///
/// This is the shape the marshalling table cannot describe: the argument is a
/// pointer to an array of pointers, each into user memory, with no length
/// anywhere -- the array ends at a NULL and each string at a NUL. So it is
/// walked, one `copy_from_user` per pointer and one per string, with a bound
/// on both counts because a process that asks for a million arguments should
/// be told no rather than answered.
#[cfg(nk_lkl)]
fn copy_string_array(mut at: u64, budget: &mut usize) -> Result<alloc::vec::Vec<alloc::vec::Vec<u8>>, i64> {
    use alloc::vec::Vec;
    let mut out: Vec<Vec<u8>> = Vec::new();
    if at == 0 {
        return Ok(out); // a null argv is an empty one, and legal
    }
    loop {
        if out.len() >= MAX_ARGS {
            return Err(-7); // -E2BIG
        }
        let mut word = [0u8; 8];
        crate::uaccess::copy_from_user(&mut word, at)?;
        let ptr = u64::from_le_bytes(word);
        if ptr == 0 {
            return Ok(out);
        }
        let mut buf = alloc::vec![0u8; (*budget).min(crate::uaccess::PATH_MAX)];
        let len = crate::uaccess::copy_cstr_from_user(ptr, &mut buf)?;
        if len > *budget {
            return Err(-7);
        }
        *budget -= len;
        buf.truncate(len);
        out.push(buf);
        at += 8;
    }
}

/// # execve(path, argv, envp)
///
/// nk's, not Linux's. LKL has no user space to exec into -- forwarding this
/// would ask Linux to replace an address space it does not have -- so nk reads
/// the file through Linux's VFS and does the replacing itself.
///
/// The order is the whole difficulty. argv and envp live in the address space
/// being replaced, so they are copied first. The new address space is built
/// second, and only when it is complete and `TTBR0` points at it is the old
/// one torn down: the kernel is mapped through the same tables, so a process
/// that frees its own address space before leaving it does not survive to
/// report the mistake.
///
/// On success this does not return. On failure it returns an errno and the
/// caller carries on with everything it had, which is what execve promises.
#[cfg(nk_lkl)]
fn sys_execve(path: u64, argv: u64, envp: u64) -> i64 {
    use crate::uaccess::{self, PATH_MAX};

    let mut name = alloc::vec![0u8; PATH_MAX];
    let len = match uaccess::copy_cstr_from_user(path, &mut name) {
        Ok(n) => n,
        Err(e) => return e,
    };
    name.truncate(len + 1); // keep the NUL: Linux's openat expects one

    let mut budget = MAX_ARG_BYTES;
    let args = match copy_string_array(argv, &mut budget) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let envs = match copy_string_array(envp, &mut budget) {
        Ok(v) => v,
        Err(e) => return e,
    };

    let Ok(cpath) = core::ffi::CStr::from_bytes_with_nul(&name) else {
        return -22; // -EINVAL: an embedded NUL is not a path
    };
    let bytes = match crate::lkl::read_file(cpath) {
        Ok(b) => b,
        Err(e) => {
            // Named, because "execve returned an errno" is the least useful
            // thing a kernel can say when a program will not start.
            println!("  execve: cannot read {:?}: {}", cpath, e);
            return e;
        }
    };

    // A script names the program that can read it, and that program is what
    // is actually loaded. execve of a script with no argv at all is not a
    // thing a libc does, but argv[0] is what gets replaced by the script's
    // path, so there has to be one to replace.
    let mut args = args;
    if args.is_empty() {
        args.push(name[..name.len() - 1].to_vec());
    }
    let (bytes, args) =
        match follow_interpreters(name[..name.len() - 1].to_vec(), bytes, args) {
            Ok(both) => both,
            Err(e) => return e,
        };
    // Borrowed views, because the loader wants slices and the owners are the
    // vectors above -- which must outlive the stack that is built from them.
    let argv: alloc::vec::Vec<&[u8]> = args.iter().map(|v| v.as_slice()).collect();
    let envv: alloc::vec::Vec<&[u8]> = envs.iter().map(|v| v.as_slice()).collect();

    let p = match load(&bytes, &argv, &envv) {
        Ok(p) => p,
        // The image was rejected before anything was replaced, so the caller
        // still has everything it had. That is what makes a failed execve
        // survivable and why the loader validates before it allocates.
        Err(why) => {
            println!("  execve: {}", why);
            return -8; // -ENOEXEC
        }
    };

    let old = current_ttbr0();
    unsafe {
        // The new tables first, then the old ones freed. The other order
        // unmaps the kernel from under the code doing the freeing.
        core::arch::asm!(
            "msr ttbr0_el1, {}", "dsb ishst", "tlbi vmalle1", "dsb ish", "isb",
            in(reg) p.ttbr0, options(nostack)
        );
        paging::destroy_user_address_space(old);
        // The thread pointer belonged to the program that is gone. A libc
        // sets its own before it needs one; leaving the old value would give
        // the new program a pointer into memory that has just been freed.
        core::arch::asm!("msr tpidr_el0, xzr", options(nomem, nostack));
    }
    crate::sched::set_user_memory(p.brk, USER_MMAP_TOP);
    println!("  execve: replaced this process with {} bytes at {:#x}", bytes.len(), p.entry);
    unsafe { enter_user_fresh(p.entry, p.stack, p.ttbr0, crate::sched::kernel_stack_top() as u64) }
}

/// # mprotect(addr, len, prot)
///
/// Nothing gets both write and execute. A libc asks for this to make its
/// relocated GOT read-only after startup -- GNU_RELRO -- which is the one
/// thing standing between a static binary and running here.
fn sys_mprotect(addr: u64, len: u64, prot: u64) -> i64 {
    const PROT_WRITE: u64 = 2;
    const PROT_EXEC: u64 = 4;
    let (_, brk_min, _) = crate::sched::user_memory();
    if brk_min == 0 || len == 0 || addr & (PAGE as u64 - 1) != 0 {
        return -22; // -EINVAL
    }
    let size = len.div_ceil(PAGE as u64) * PAGE as u64;
    if addr >= USER_STACK_TOP || size > USER_STACK_TOP - addr {
        return -22;
    }
    let exec = prot & PROT_EXEC != 0;
    let writable = prot & PROT_WRITE != 0;

    if exec && writable {
        return -13; // -EACCES, and nk will not make an exception
    }
    if prot == 0 { return if unsafe { paging::protect_user_none(current_ttbr0(), addr, size) } { 0 } else { -12 }; }
    if unsafe { !paging::protect_user(current_ttbr0(), addr, size, exec, writable) } {
        return -12; // -ENOMEM: Linux's answer for a hole in the range
    }
    0
}

/// # set_tid_address(ptr)
///
/// Answered here rather than forwarded because the pointer is a user address
/// and Linux, flat, would store it and later write through it into whatever
/// happens to be at that offset in the kernel. Nothing writes to it: the
/// address is where Linux would clear the tid when the thread dies, and nk
/// has no threads to clear it for yet. The *return* is what the caller
/// actually uses, and it is the real tid.
fn sys_set_tid_address() -> i64 {
    crate::sched::linux_pid(crate::sched::current_id())
}

/// # prlimit64(pid, resource, new, old)
///
/// Only the stack limit, and only reading it. A libc asks so it knows how far
/// it may let the stack grow before it should guard; telling it the truth --
/// that the stack is exactly what nk mapped -- is better than -ENOSYS, which
/// makes it assume a default it will not get.
fn sys_prlimit64(new: u64, old: u64) -> i64 {
    const RLIM_INFINITY: u64 = !0;
    if new != 0 {
        return -1; // -EPERM: nothing may raise a limit here
    }
    if old == 0 {
        return 0;
    }
    let mut buf = [0u8; 16];
    buf[..8].copy_from_slice(&(USER_STACK_SIZE as u64).to_le_bytes());
    buf[8..].copy_from_slice(&RLIM_INFINITY.to_le_bytes());
    if crate::uaccess::copy_to_user(old, &buf).is_err() {
        return crate::uaccess::EFAULT;
    }
    0
}

fn current_ttbr0() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {}, ttbr0_el1", out(reg) v, options(nomem, nostack)) };
    v
}

/// Back a user range with fresh zeroed pages. All or nothing: a partial
/// mapping would leave the process holding an address range that faults half
/// way through, which is worse than being told no.
fn map_anonymous(at: u64, len: u64) -> bool {
    let ttbr0 = current_ttbr0();
    let mut done = 0;
    while done < len {
        let Some(page) = frames::alloc() else {
            unsafe { crate::paging::unmap_user(ttbr0, at, done) };
            return false;
        };
        unsafe {
            core::ptr::write_bytes(page, 0, PAGE);
            paging::map_user_permissions(ttbr0, at + done, page as u64, PAGE as u64, false, true);
        }
        done += PAGE as u64;
    }
    true
}

fn sys_exit(status: i32) -> ! {
    #[cfg(nk_lkl)]
    {
        // What pthread_join is waiting for.
        //
        // CLONE_CHILD_CLEARTID asks the kernel to zero a word in the thread's
        // descriptor when it dies and wake anything sleeping on it. That is
        // the entire mechanism behind a join: glibc does not poll, it futexes
        // on that word, and a kernel that forgets this half gives a join that
        // never returns and no other symptom.
        let tidptr = crate::sched::clear_child_tid();
        if tidptr != 0 {
            let _ = crate::uaccess::copy_to_user(tidptr, &0u32.to_le_bytes());
            crate::futex::futex(tidptr, 1, u32::MAX, 0, 0, u32::MAX);
        }
        crate::futex::forget(crate::sched::current_id());
    }
    // A thread exiting is not the process exiting, and saying so would be a
    // lie in the middle of a program that is still running.
    #[cfg(nk_lkl)]
    let thread = crate::sched::clear_child_tid() != 0;
    #[cfg(not(nk_lkl))]
    let thread = false;
    if !thread {
        println!();
        println!("  the process exited with status {}", status);
    }
    #[cfg(nk_lkl)]
    {
        crate::sched::set_exit_status(status);
        crate::sched::exit_current()
    }
    #[cfg(not(nk_lkl))]
    crate::stop()
}

/// Whether an initrd already put a program at `/nk-init`.
///
/// If it did, nk must not write over it; if it did not, nk has to seed
/// something before there is anything to load. These are two different
/// questions from the one below and conflating them cost a debugging round:
/// a `--init` binary is not the fixture *and* still has to be seeded.
#[cfg(nk_lkl)]
static mut INIT_FROM_INITRD: bool = false;

/// Whether the program at `/nk-init` is nk's own fixture.
///
/// The self-checks that follow a run are claims about the fixture -- that it
/// exits with its own Linux pid, that it spun long enough to be preempted --
/// and they are not true of an arbitrary binary. `--init` settles this at
/// build time and an initrd settles it at run time.
#[cfg(nk_lkl)]
pub fn ran_fixture() -> bool {
    !cfg!(nk_init) && !unsafe { INIT_FROM_INITRD }
}

/// Unpack a cpio archive into Linux's rootfs, and return how many entries it
/// held.
///
/// This is what makes a program on nk something other than part of the
/// kernel. Until now `/nk-init` was seeded from a fixture inside `nk.bin`,
/// which is fine for a program written to be a test and useless for a
/// userland: a real one is many files, and they have to come from outside.
///
/// Directories first is not assumed. A cpio archive usually lists a directory
/// before its contents and is not required to, so each file's parents are
/// created as it goes -- which also means the same directory is met more than
/// once as a matter of course, and `mkdir` treats that as success.
#[cfg(nk_lkl)]
pub fn unpack_initrd(archive: &[u8]) -> Result<usize, &'static str> {
    use alloc::vec::Vec;
    let mut count = 0;
    crate::cpio::each(archive, |e| {
        // The names in an archive are relative ("bin/sh"); the rootfs wants
        // them absolute, and a leading "./" is how most archives spell the
        // same thing.
        let name = e.name.strip_prefix("./").unwrap_or(e.name);
        if name.is_empty() {
            return;
        }
        let mut path = Vec::with_capacity(name.len() + 2);
        path.push(b'/');
        path.extend_from_slice(name.as_bytes());
        path.push(0);

        // Every parent, in order, before the entry itself.
        for i in 1..path.len() - 1 {
            if path[i] != b'/' {
                continue;
            }
            let saved = path[i];
            path[i] = 0;
            let _ = crate::lkl::mkdir(cstr(&path[..=i]), 0o755);
            path[i] = saved;
        }

        let c = cstr(&path);
        // Whether the archive supplies the init has to be settled here,
        // while the rootfs is still empty. Asking later -- "does /nk-init
        // exist?" -- gets the wrong answer the moment nk has seeded its own
        // fixture, and the second process to start then reports that the
        // first one's file came from an archive that was never there.
        if name == "nk-init" && e.is_file() {
            unsafe { INIT_FROM_INITRD = true };
        }
        let ok = if e.is_dir() {
            crate::lkl::mkdir(c, e.perms() as i64).is_ok()
        } else if e.is_file() {
            crate::lkl::write_file_mode(c, e.data, e.perms() as i64).is_ok()
        } else if e.is_symlink() {
            // newc stores the target without a NUL. Keep it relative so a
            // bin/cat -> busybox applet resolves beside bin/busybox.
            if e.data.contains(&0) || e.data.is_empty() {
                false
            } else {
                let mut target = e.data.to_vec();
                target.push(0);
                crate::lkl::syscall(36, [target.as_ptr() as i64, -100,
                    c.as_ptr() as i64, 0, 0, 0]) == 0 // symlinkat
            }
        } else {
            // Devices and fifos. Nothing needs them yet and
            // pretending to create one by making an empty file would be
            // worse than leaving it out visibly.
            false
        };
        if ok {
            count += 1;
        }
    })?;
    Ok(count)
}

/// A NUL-terminated slice as a `CStr`, for paths built a byte at a time.
#[cfg(nk_lkl)]
fn cstr(bytes: &[u8]) -> &core::ffi::CStr {
    core::ffi::CStr::from_bytes_until_nul(bytes).unwrap_or(c"/")
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
    #[cfg(not(nk_init))]
    let fixture = unsafe { core::slice::from_raw_parts(start, size) };
    // An externally built binary, when `run-kernel.sh --init` supplied one.
    // It travels inside the kernel image because the rootfs is memory-backed
    // and there is no disk to read it from yet.
    #[cfg(nk_init)]
    let fixture: &[u8] = {
        let _ = (start, size);
        include_bytes!(concat!(env!("OUT_DIR"), "/nk-init.bin"))
    };
    // An initrd's /nk-init wins. Seeding the fixture over it would throw away
    // the whole point of having unpacked an archive, and doing that silently
    // is worse than not supporting it.
    let bytes = if !unsafe { INIT_FROM_INITRD } {
        crate::lkl::write_file(c"/nk-init", fixture).map_err(|_| "rootfs write failed")?;
        let b = crate::lkl::read_file(c"/nk-init").map_err(|_| "rootfs read failed")?;
        if b != fixture {
            return Err("rootfs round trip differs");
        }
        println!(
            "  rootfs: /nk-init read back through Linux VFS ({} bytes)",
            b.len()
        );
        b
    } else {
        let b = crate::lkl::read_file(c"/nk-init").map_err(|_| "rootfs read failed")?;
        println!("  rootfs: /nk-init came from the initrd ({} bytes)", b.len());
        b
    };
    // The init may be a script, and usually is. Following the `#!` here as
    // well as in execve means an initrd can ship `#!/bin/busybox sh` and no
    // part of nk has to know what busybox is.
    let (bytes, args) = follow_interpreters(
        b"/nk-init".to_vec(),
        bytes,
        alloc::vec![b"/nk-init".to_vec()],
    )
    .map_err(|_| "the init names an interpreter that cannot be read")?;
    let argv: alloc::vec::Vec<&[u8]> = args.iter().map(|v| v.as_slice()).collect();
    load(&bytes, &argv, &[b"PATH=/bin", b"HOME=/"])
}

/// Follow `#!` lines until something is an ELF.
///
/// An init is usually a shell script, and a script is not a thing a loader can
/// enter: the file names the program that can read it. Linux resolves this in
/// `binfmt_script`, and the rules are its rules -- the first line only, up to
/// a fixed length, the first word is the interpreter and *everything after it
/// is one argument* however many spaces it contains, and the script's own path
/// is inserted as the interpreter's first argument while `argv[0]` is
/// discarded.
///
/// That last part is why busybox as `/nk-init` exits 127 without this: it
/// chooses its applet from `basename(argv[0])`, nk passes the path it loaded,
/// and there is no applet called `nk-init`. `#!/bin/busybox sh` says what was
/// meant.
///
/// Bounded at four, as Linux is. A script whose interpreter is itself is not
/// an error anyone can act on, and following it is a loop.
#[cfg(nk_lkl)]
fn follow_interpreters(
    mut path: alloc::vec::Vec<u8>,
    mut bytes: alloc::vec::Vec<u8>,
    mut args: alloc::vec::Vec<alloc::vec::Vec<u8>>,
) -> Result<(alloc::vec::Vec<u8>, alloc::vec::Vec<alloc::vec::Vec<u8>>), i64> {
    use alloc::vec::Vec;
    /// Linux's own BINPRM_BUF_SIZE. A longer first line is not truncated into
    /// something plausible; it is refused, because a truncated interpreter
    /// path is a different program.
    const LINE: usize = 256;

    for _ in 0..4 {
        if bytes.len() < 2 || &bytes[..2] != b"#!" {
            return Ok((bytes, args));
        }
        let end = bytes.iter().take(LINE).position(|&c| c == b'\n').ok_or(-8i64)?;
        let line = &bytes[2..end];
        let line = &line[line.iter().take_while(|c| **c == b' ' || **c == b'\t').count()..];
        if line.is_empty() {
            return Err(-8); // -ENOEXEC: "#!" and nothing to run
        }
        let split = line.iter().position(|&c| c == b' ' || c == b'\t').unwrap_or(line.len());
        let interp = line[..split].to_vec();
        let rest = line[split..].iter().copied().skip_while(|c| *c == b' ' || *c == b'\t')
            .collect::<Vec<u8>>();
        let rest = match rest.iter().rposition(|c| *c != b' ' && *c != b'\t') {
            Some(last) => rest[..=last].to_vec(),
            None => Vec::new(),
        };

        // argv[0] is the interpreter, then its one optional argument, then the
        // script's path, then whatever the caller passed after argv[0].
        let mut next: Vec<Vec<u8>> = alloc::vec![interp.clone()];
        if !rest.is_empty() {
            next.push(rest);
        }
        next.push(path.clone());
        next.extend(args.into_iter().skip(1));
        args = next;

        let mut c = interp.clone();
        c.push(0);
        let name = core::ffi::CStr::from_bytes_with_nul(&c).map_err(|_| -22i64)?;
        bytes = crate::lkl::read_file(name)?;
        path = interp;
    }
    Err(-36) // -ENAMETOOLONG, which is what Linux returns for too many levels
}

/// Build a fresh address space around an ELF image and the arguments it is to
/// start with.
///
/// Everything a process is: its mapped segments, a stack with argc/argv/envp
/// and the auxiliary vector on it, and the address at which to begin. It does
/// not touch the *current* address space, which is what lets `execve` build
/// the replacement before tearing down what it replaces -- the order matters,
/// because the kernel is mapped through the same tables and the process
/// cannot survive unmapping itself half way through.
#[cfg(nk_lkl)]
pub fn load(
    bytes: &[u8],
    args: &[&[u8]],
    envs: &[&[u8]],
) -> Result<Process, &'static str> {
    let image = crate::elf::parse(bytes, USER_BASE, USER_MMAP_TOP)?;
    if image.segments.iter().any(|s| s.address < 0x0a20_0000 && s.address+s.memsz as u64 > 0x0800_0000) {
        return Err("ELF overlaps kernel device mappings");
    }
    let ttbr0 = paging::new_address_space();
    if let Err(e) = map_elf_segments(ttbr0, &image, bytes) {
        unsafe { paging::destroy_user_address_space(ttbr0); }
        return Err(e);
    }
    let mut entry = image.entry;
    let mut interpreter_base = 0;
    if let Some(path) = &image.interpreter {
        let result = (|| {
            let bytes = crate::lkl::read_file(core::ffi::CStr::from_bytes_with_nul(path).map_err(|_| "interpreter path")?)
                .map_err(|_| "cannot read ELF interpreter")?;
            let loader = crate::elf::parse_at(&bytes, USER_MMAP_TOP, USER_STACK_TOP-USER_STACK_SIZE as u64, USER_MMAP_TOP)?;
            if loader.interpreter.is_some() { return Err("recursive ELF interpreter"); }
            map_elf_segments(ttbr0, &loader, &bytes)?;
            Ok((loader.entry, loader.load_bias))
        })();
        match result { Ok((pc, bias)) => { entry = pc; interpreter_base = bias; },
            Err(e) => { unsafe { paging::destroy_user_address_space(ttbr0); } return Err(e); } }
    }
    let Some(stack) = frames::alloc_contiguous(USER_STACK_SIZE / PAGE) else {
        unsafe { paging::destroy_user_address_space(ttbr0); }
        return Err("no memory for the ELF stack");
    };
    unsafe { core::ptr::write_bytes(stack, 0, USER_STACK_SIZE) };
    let sp = match build_initial_stack(stack, &image, interpreter_base, args, envs) {
        Ok(sp) => sp,
        Err(e) => {
            unsafe {
                for off in (0..USER_STACK_SIZE).step_by(PAGE) { frames::free(stack.add(off)); }
                paging::destroy_user_address_space(ttbr0);
            }
            return Err(e);
        }
    };
    unsafe {
        paging::map_user(
            ttbr0,
            USER_STACK_TOP - USER_STACK_SIZE as u64,
            stack as u64,
            USER_STACK_SIZE as u64,
            false,
        );
        core::arch::asm!("dsb ish", "ic iallu", "dsb ish", "isb", options(nostack));
    }
    println!(
        "  ELF: {} PT_LOAD segment(s), entry {:#x}, zero-filled BSS",
        image.segments.len(),
        image.entry
    );
    // The heap starts on the first page boundary past everything the image
    // asked for, so growing it can never land on the program's own bss.
    let brk = image
        .segments
        .iter()
        .map(|s| s.address + s.memsz as u64)
        .max()
        .unwrap_or(USER_BASE)
        .div_ceil(PAGE as u64)
        * PAGE as u64;
    Ok(Process { ttbr0, entry, stack: sp, brk })
}

#[cfg(nk_lkl)]
fn map_elf_segments(ttbr0: u64, image: &crate::elf::Image, bytes: &[u8]) -> Result<(), &'static str> {
    for s in &image.segments {
        let base = s.address & !(PAGE as u64 - 1);
        let end = (s.address + s.memsz as u64).div_ceil(PAGE as u64) * PAGE as u64;
        for va in (base..end).step_by(PAGE) {
            let page = frames::alloc().ok_or("no memory for ELF")?;
            // Zero first, always. A PT_LOAD's memsz runs past its filesz --
            // that tail is the .bss -- and a frame handed back by the
            // allocator holds whatever the last user of it left there. The
            // fault this caused was a long way from here: glibc's exit path
            // walks a btree rooted in .bss, so a program that touched none of
            // its own uninitialised data still died on the way out.
            unsafe { core::ptr::write_bytes(page, 0, PAGE) };
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
    Ok(())
}

/// Sixteen bytes for AT_RANDOM, from which a libc takes its stack guard.
///
/// GRND_INSECURE, and it is not optional. Plain `getrandom` *blocks* until
/// the CRNG is seeded, and on a machine whose only entropy is a virtual timer
/// it may never be: the first attempt deadlocked the boot thread inside Linux
/// with every other task idle, which is what the watchdog exists to report.
/// GRND_INSECURE is Linux's own answer to exactly this -- bytes now, from a
/// pool that says it is not yet trustworthy.
#[cfg(nk_lkl)]
fn stack_seed() -> Result<[u8; 16], &'static str> {
    const GRND_INSECURE: i64 = 0x0004;
    let mut random = [0u8; 16];
    let args = [random.as_mut_ptr() as i64, random.len() as i64, GRND_INSECURE, 0, 0, 0];
    if crate::lkl::syscall(278, args) != random.len() as i64 {
        return Err("getrandom did not fill the stack guard");
    }
    Ok(random)
}

/// Without Linux there is no generator, and the virtual counter is the only
/// thing on this machine that differs between one boot and the next. It is
/// not entropy and is not claimed to be; it is here so the standalone smoke
/// test gets a stack of the same *shape*, which is what it is checking.
#[cfg(not(nk_lkl))]
fn stack_seed() -> Result<[u8; 16], &'static str> {
    let mut random = [0u8; 16];
    for (i, chunk) in random.chunks_mut(8).enumerate() {
        let t: u64;
        unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) t, options(nomem, nostack)) };
        chunk.copy_from_slice(&(t ^ (0x9e37_79b9_7f4a_7c15u64.wrapping_mul(i as u64 + 1))).to_le_bytes());
    }
    Ok(random)
}

/// argc, argv, envp and the auxiliary vector, as a libc's `_start` expects
/// to find them.
///
/// Nothing declares this interface: `_start` takes no arguments and reads it
/// off the stack at a shape the kernel is simply expected to have built. So
/// the failure mode for getting it wrong is not a syscall nk could name, it
/// is the program dereferencing whatever happened to be there.
#[cfg(nk_lkl)]
fn build_initial_stack(
    page: *mut u8,
    image: &crate::elf::Image,
    interpreter_base: u64,
    args: &[&[u8]],
    envs: &[&[u8]],
) -> Result<u64, &'static str> {
    use crate::stack::*;

    // AT_PHDR is the *address* the program headers ended up at, which is only
    // knowable from the segment that happens to contain them -- usually the
    // first, because it starts at file offset 0 and so covers the headers.
    let phdr = image
        .segments
        .iter()
        .find(|s| image.phoff >= s.offset && image.phoff < s.offset + s.filesz)
        .map(|s| s.address + (image.phoff - s.offset) as u64)
        .unwrap_or(0);

    let random = stack_seed()?;

    let aux = [
        (AT_PHDR, phdr),
        (AT_PHENT, 56),
        (AT_PHNUM, image.phnum as u64),
        (AT_PAGESZ, PAGE as u64),
        // The executable owns AT_ENTRY/PHDR; ld.so owns the initial PC
        // and gets its relocation base separately.
        (AT_BASE, interpreter_base),
        (AT_FLAGS, 0),
        (AT_ENTRY, image.entry),
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        // Claiming no optional CPU features is always safe; claiming one nk
        // has not enabled at EL0 -- FP, SVE -- is a trap the libc springs
        // itself, on its first instruction that uses it.
        (AT_HWCAP, 0),
        (AT_CLKTCK, 100),
        (AT_SECURE, 0),
    ];

    let sp = unsafe { crate::stack::Builder::new(page, USER_STACK_TOP, USER_STACK_SIZE) }
        .build(args, envs, &aux, &random)
        .ok_or("the initial stack does not fit")?;
    println!(
        "  stack: argc {}, {} environment entries, {} auxv pairs, sp {:#x}",
        args.len(),
        envs.len(),
        aux.len() + 2,
        sp
    );
    Ok(sp)
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
    if crate::lkl::open_console() {
        crate::sched::set_has_console(true);
    }
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
