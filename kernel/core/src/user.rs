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
    let console = frame.x[0] == 1 || frame.x[0] == 2;
    let ret = if frame.x[8] == 64 && console {
        sys_write(frame.x[0], frame.x[1], frame.x[2])
    } else if frame.x[8] == 66 && console {
        // The same rule as `write`, and it has to be here too because this is
        // the call a libc `printf` actually makes.
        sys_writev(frame.x[0], frame.x[1], frame.x[2])
    } else if matches!(frame.x[8], 214 | 222 | 215 | 226 | 96 | 99 | 293 | 261) {
        // The process's address space is nk's, not Linux's. LKL is one flat
        // region with no user half at all, so forwarding these would move
        // Linux's own break and hand back an address this process cannot
        // reach. They are the calls nk has to answer itself.
        match frame.x[8] {
            214 => sys_brk(frame.x[0]),
            222 => sys_mmap(frame.x[0], frame.x[1], frame.x[3]),
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
/// Anonymous private mappings only, which is what a libc's malloc asks for.
/// A file mapping needs the page cache to be nk's problem as well as Linux's
/// and is a separate piece of work; refusing it is better than returning
/// memory that does not contain the file.
fn sys_mmap(addr: u64, len: u64, flags: u64) -> i64 {
    const MAP_ANONYMOUS: u64 = 0x20;
    const ENOMEM: i64 = -12;
    const EINVAL: i64 = -22;
    if flags & MAP_ANONYMOUS == 0 {
        println!("  mmap of a file is not implemented");
        return -38; // -ENOSYS
    }
    // MAP_FIXED would have to unmap whatever is there and honour the exact
    // address; nothing needs it yet, and quietly ignoring the hint would give
    // a caller that does need it the wrong answer.
    if flags & 0x10 != 0 {
        return EINVAL;
    }
    let _ = addr;
    if len == 0 || len > USER_MEM_LIMIT {
        return EINVAL;
    }
    let len = len.div_ceil(PAGE as u64) * PAGE as u64;
    let (_, brk_min, mmap_next) = crate::sched::user_memory();
    if brk_min == 0 || mmap_next < len {
        return ENOMEM;
    }
    let at = mmap_next - len;
    let (brk, _, _) = crate::sched::user_memory();
    if at <= brk {
        return ENOMEM; // the heap and the mappings have met
    }
    if !map_anonymous(at, len) {
        return ENOMEM;
    }
    crate::sched::set_user_mmap_next(at);
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
    crate::lkl::write_file(c"/nk-init", fixture).map_err(|_| "rootfs write failed")?;
    let bytes = crate::lkl::read_file(c"/nk-init").map_err(|_| "rootfs read failed")?;
    if bytes != fixture {
        return Err("rootfs round trip differs");
    }
    println!(
        "  rootfs: /nk-init read back through Linux VFS ({} bytes)",
        bytes.len()
    );
    let image = crate::elf::parse(&bytes, USER_BASE, USER_MMAP_TOP)?;
    let ttbr0 = paging::new_address_space();
    for s in &image.segments {
        let base = s.address & !(PAGE as u64 - 1);
        let end = (s.address + s.memsz as u64).div_ceil(PAGE as u64) * PAGE as u64;
        for va in (base..end).step_by(PAGE) {
            let page = frames::alloc().expect("no memory for ELF");
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
    let stack = frames::alloc_contiguous(USER_STACK_SIZE / PAGE)
        .ok_or("no memory for the ELF stack")?;
    unsafe { core::ptr::write_bytes(stack, 0, USER_STACK_SIZE) };
    let sp = build_initial_stack(stack, &image)?;
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
    Ok(Process { ttbr0, entry: image.entry, stack: sp, brk })
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
fn build_initial_stack(page: *mut u8, image: &crate::elf::Image) -> Result<u64, &'static str> {
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
        // No interpreter: the loader refuses dynamic images, so nothing was
        // mapped for one and AT_BASE has to say so rather than lie.
        (AT_BASE, 0),
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
        .build(&[b"/nk-init"], &[b"PATH=/bin", b"HOME=/"], &aux, &random)
        .ok_or("the initial stack does not fit")?;
    println!(
        "  stack: argc 1, 2 environment entries, {} auxv pairs, sp {:#x}",
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
