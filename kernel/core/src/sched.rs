//! Threads, and a round robin over them.
//!
//! Kernel threads and single-threaded EL0 processes share the scheduler.
//! Each task retains its translation root; Linux task ownership lives in
//! the host TLS associated with that same nk thread.
//!
//! Preemptive, from the timer interrupt. Cooperative scheduling would be less
//! code and would be a trap: a driver that spins waiting for a device would
//! hang the machine rather than merely be slow, and the shim cannot fix that
//! from outside.

use crate::frames::{self, PAGE};
use crate::println;
use core::sync::atomic::{AtomicBool, Ordering};

/// Four pages. Generous for what runs here, and cheap; a Linux driver's probe
/// path is not shy with stack, and a kernel stack overflow with no guard page
/// silently corrupts whatever is below it.
const STACK_PAGES: usize = 16;
/// Sixty-four, because Linux wants them.
///
/// Sixteen was plenty for nk's own threads and is nowhere near enough for a
/// booting Linux: init, kthreadd, the RCU threads, the per-subsystem
/// workqueues, kdevtmpfs, kblockd, writeback and the rest are two dozen
/// before anything useful runs. Sixty-four is also the width of the waiter
/// bitmask in `sync.rs`, and the two must stay equal -- a task whose slot is
/// past the end of that mask can be blocked and never woken.
pub const MAX_TASKS: usize = 64;

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    Unused,
    Ready,
    Running,
    /// Waiting for something. The scheduler will not pick it until somebody
    /// calls `wake`.
    ///
    /// Everything in nk until now waited by yielding in a loop, which works
    /// and is wrong in a way that matters as soon as there is more than a
    /// demo running: a spinning task is indistinguishable from a busy one, so
    /// the machine can never be idle and a lock held across a long operation
    /// burns every remaining slice. A real block is what a semaphore, a mutex
    /// and a wait queue are all built from, and Linux's core assumes all
    /// three.
    Blocked,
    Finished,
}

#[derive(Clone, Copy)]
pub struct Task {
    pub sp: usize,
    pub stack: usize,
    /// What SP_EL0 points at while this task runs.
    ///
    /// On arm64 Linux, SP_EL0 in kernel mode holds the current task_struct --
    /// `current` is literally a read of it -- and kbuild compiles every driver
    /// with `-mstack-protector-guard=sysreg -mstack-protector-guard-reg=sp_el0
    /// -mstack-protector-guard-offset=1344`, so the stack canary is read from
    /// SP_EL0 + 1344 on entry to almost every function. With SP_EL0 left at
    /// zero, the first Linux function called dereferences address 1344.
    ///
    /// So each task gets a page. nk does not have task_structs and the page is
    /// zeroed, which makes the canary a consistent zero -- weaker than Linux's
    /// per-task random value, and still catches the linear overflow the canary
    /// exists for. A driver that follows `current` into it reads zeroes, which
    /// is a real limitation and will need a proper shadow task_struct the
    /// first time one does.
    pub shadow: usize,
    pub state: State,
    pub name: &'static str,
    pub slices: u64,
    /// What this task is blocked on, when it is blocked: the id of a
    /// semaphore or mutex. Zero when it blocked for some other reason.
    pub waiting_on: u32,
    pub ttbr0: u64,
    pub linux_pid: i64,
    pub exit_status: i32,
    pub user_irqs: u64,
    /// The process's heap break, and the lowest value `brk` may return to.
    /// Zero for a kernel thread, which has neither.
    pub brk: u64,
    pub brk_min: u64,
    /// Where the next anonymous mapping goes. Grows downward, away from the
    /// heap, so the two run out of room by meeting rather than by silently
    /// overwriting one another.
    pub mmap_next: u64,
    /// The task that forked this one, or 0. `wait4` needs it: a process may
    /// only wait for its own children, and "its own" is a fact nothing else
    /// records -- Linux knows about its tasks but nk owns the address spaces
    /// and the exit statuses, so the relation has to live where those do.
    pub parent: usize,
    /// Whether this task shares its address space with the one that made it.
    ///
    /// A thread does. It must not have the address space torn down when it
    /// exits, because the threads it shares with are still using it -- and
    /// the tables are the same tables, not a copy.
    /// How many times this slot has been used.
    ///
    /// A task id is a slot index and slots are reused, so an id alone does
    /// not identify a thread over time: a task exits, its slot is taken by
    /// the next one, and the two are indistinguishable. That is fine inside
    /// nk, which only ever asks about live tasks -- and wrong for Linux,
    /// which holds `cpu.owner` across a task's whole life and compares it
    /// with `thread_self()` to decide whether it already owns the CPU lock.
    /// An alias there is not a cosmetic mix-up: matching a dead owner takes
    /// the lock without acquiring it, and failing to match one waits on a
    /// thread that no longer exists. See `uid`.
    pub generation: u64,
    pub shares_mm: bool,
    /// A leaked `Semaphore` the vfork parent is blocked on, or 0.
    ///
    /// vfork's promise is that the parent does not run again until the child
    /// has stopped using the address space they share -- by `execve` or by
    /// exiting. This is how the child says so.
    pub vfork_done: usize,
    /// Where to write a zero and wake a futex when this task exits, if
    /// `CLONE_CHILD_CLEARTID` asked for it. That write is what `pthread_join`
    /// is waiting for.
    pub clear_child_tid: u64,
    /// Whether this process has descriptors 0, 1 and 2 open on a real
    /// console. When it has not, nk answers writes to 1 and 2 itself, which
    /// prints and cannot redirect.
    pub has_console: bool,
    /// Whether this task is inside a system call made *from EL0*.
    ///
    /// Linux asks nk to copy user memory for it, and the same code path
    /// serves two callers that must be treated differently: a program at EL0,
    /// whose pointers have to be translated with its own permissions and
    /// refused if they name kernel memory, and nk itself, which calls into
    /// Linux with kernel buffers to seed the rootfs and load an ELF. The flag
    /// says which, and it is per task because both can be happening at once.
    pub user_syscall: bool,
    /// The process's thread pointer, `TPIDR_EL0`.
    ///
    /// nk never reads it, which is exactly why it has to be saved here: it
    /// belongs entirely to EL0, so nothing in the kernel would notice it
    /// being wrong. A libc puts its whole thread-local area behind it --
    /// `errno`, the malloc tcache, the locale -- so leaving one process's
    /// value in place while another runs hands the second process the first
    /// one's heap bookkeeping, and the crash lands some distance away in
    /// malloc rather than anywhere near the switch.
    pub tpidr: u64,
    /// Shared mappings this task holds: where each is, and which pool region
    /// it points at. A thread shares its creator's list -- it is the same
    /// address space -- while a forked child gets a copy of the entries
    /// (its pages are private copies; see `user.rs`). Bounded: a process
    /// with more shared mappings than this is told no at `mmap` time.
    pub shared: [crate::user::SharedMap; 16],
    pub nshared: usize,
    /// Signal state. `actions` is shared across threads the way the address
    /// space is -- dispositions belong to the process -- while `mask` and
    /// `pending` are per task. A forked child inherits a copy of all three.
    pub sig_actions: [crate::signal::Action; 64],
    pub sig_mask: u64,
    pub sig_pending: u64,
    /// `si_code` and sender pid for the pending signals, so `siginfo` in the
    /// delivered frame says something true. `SI_USER` for a `kill`,
    /// `SI_KERNEL` for a `SIGCHLD` from `sys_exit`.
    pub sig_code: [i32; 64],
    pub sig_sender: [i32; 64],
    /// Alternate signal stack from `sigaltstack`, and whether a handler is
    /// running on it now (nested `SA_ONSTACK` handlers reuse it -- Linux
    /// refuses the second, nk notes it and carries on).
    pub sig_alt_base: u64,
    pub sig_alt_size: u64,
    pub sig_alt_in_use: bool,
    /// What `rt_sigreturn` restores: the mask saved at delivery, and whether
    /// the handler ran on the alternate stack.
    pub sig_return_mask: u64,
    pub sig_return_alt: bool,
}

static mut TASKS: [Task; MAX_TASKS] = [Task {
    sp: 0,
    stack: 0,
    shadow: 0,
    state: State::Unused,
    name: "",
    slices: 0,
    waiting_on: 0,
    ttbr0: 0,
    linux_pid: 0,
    exit_status: 0,
    user_irqs: 0,
    brk: 0,
    brk_min: 0,
    mmap_next: 0,
    parent: 0,
    generation: 0,
    shares_mm: false,
    vfork_done: 0,
    clear_child_tid: 0,
    has_console: false,
    user_syscall: false,
    tpidr: 0,
    shared: [crate::user::SharedMap {
        start: 0,
        len: 0,
        region: 0,
        fd: -1,
        anonymous: false,
    }; 16],
    nshared: 0,
    sig_actions: [crate::signal::Action::default(); 64],
    sig_mask: 0,
    sig_pending: 0,
    sig_code: [0; 64],
    sig_sender: [0; 64],
    sig_alt_base: 0,
    sig_alt_size: 0,
    sig_alt_in_use: false,
    sig_return_mask: 0,
    sig_return_alt: false,
}; MAX_TASKS];

static mut CURRENT: usize = 0;
static ENABLED: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn cpu_switch(prev_sp: *mut usize, next_sp: usize);
    fn task_start();
}

/// Slot 0 is whatever was already running when the scheduler started -- the
/// boot path itself. It needs no stack allocated because it is already on one,
/// and it needs an entry in the table because it is what the first switch
/// switches *away from*.
pub fn init() {
    unsafe {
        let t = &mut (*(&raw mut TASKS))[0];
        t.state = State::Running;
        t.name = "boot";
        t.shadow = frames::alloc().expect("no memory for the boot task shadow") as usize;
        CURRENT = 0;
        set_shadow(t.shadow);
    }
}

/// Put SP_EL0 back to the current task's shadow after a return from EL0,
/// where it held the user stack pointer instead.
pub fn restore_task_ptr() {
    unsafe { set_shadow((*(&raw const TASKS))[CURRENT].shadow) };
}

/// Point SP_EL0 at this task's shadow page. Must happen before any Linux code
/// runs on the task, and before every switch to it.
#[inline]
fn set_shadow(addr: usize) {
    unsafe { core::arch::asm!("msr sp_el0, {}", in(reg) addr, options(nomem, nostack)) };
}

/// Create a task. `entry` is called with `arg`, and falling off the end of it
/// is fine -- task_start catches the return.
pub fn spawn(name: &'static str, entry: extern "C" fn(usize), arg: usize) -> usize {
    unsafe {
        let tasks = &mut *(&raw mut TASKS);
        let slot = tasks
            .iter()
            .position(|t| t.state == State::Unused)
            .expect("no free task slots");

        // Bumped before the task exists, so no id for this incarnation can
        // ever equal one handed out for the last occupant of the slot.
        let generation = tasks[slot].generation.wrapping_add(1);

        let stack = frames::alloc_contiguous(STACK_PAGES).expect("out of memory for a task stack");
        let top = stack as usize + STACK_PAGES * PAGE;

        // The frame cpu_switch will pop: twelve callee-saved registers, laid
        // out exactly as its `stp` sequence writes them. x30 is task_start, so
        // the `ret` at the end of cpu_switch lands there; x19 and x20 carry
        // what task_start needs, because there is no other way to pass an
        // argument through a `ret`.
        let sp = top - 96;
        let f = sp as *mut usize;
        f.add(0).write(entry as *const () as usize); // x19
        f.add(1).write(arg); // x20
        for i in 2..10 {
            f.add(i).write(0); // x21..x28
        }
        f.add(10).write(0); // x29, the frame pointer: a task has no caller
        f.add(11).write(task_start as *const () as usize); // x30

        let shadow = frames::alloc().expect("no memory for a task shadow") as usize;
        tasks[slot] = Task {
            sp,
            stack: stack as usize,
            shadow,
            state: State::Ready,
            name,
            slices: 0,
            waiting_on: 0,
            ttbr0: 0,
            linux_pid: 0,
            exit_status: 0,
            user_irqs: 0,
            brk: 0,
            brk_min: 0,
            mmap_next: 0,
            parent: 0,
            generation,
            shares_mm: false,
            vfork_done: 0,
            clear_child_tid: 0,
            has_console: false,
            user_syscall: false,
            tpidr: 0,
            shared: [crate::user::SharedMap {
                start: 0,
                len: 0,
                region: 0,
                fd: -1,
                anonymous: false,
            }; 16],
            nshared: 0,
            sig_actions: [crate::signal::Action::default(); 64],
            sig_mask: 0,
            sig_pending: 0,
            sig_code: [0; 64],
            sig_sender: [0; 64],
            sig_alt_base: 0,
            sig_alt_size: 0,
            sig_alt_in_use: false,
            sig_return_mask: 0,
            sig_return_alt: false,
        };
        tasks[slot].ttbr0 = crate::paging::kernel_address_space();
        slot
    }
}

pub fn schedule() {
    let flags = crate::sync::irq_save();
    unsafe {
        // No exclusive reference survives cpu_switch: another task mutates
        // this table while the outgoing one sleeps.
        let cur = CURRENT;
        let mut next = None;
        for i in 1..=MAX_TASKS {
            let candidate = (cur + i) % MAX_TASKS;
            if TASKS[candidate].state == State::Ready {
                next = Some(candidate);
                break;
            }
        }
        if let Some(next) = next {
            if TASKS[cur].state == State::Running {
                TASKS[cur].state = State::Ready;
            }
            TASKS[next].state = State::Running;
            TASKS[next].slices += 1;
            core::arch::asm!("mrs {}, ttbr0_el1", out(reg) TASKS[cur].ttbr0, options(nostack));
            core::arch::asm!("mrs {}, tpidr_el0", out(reg) TASKS[cur].tpidr, options(nostack));
            CURRENT = next;
            set_shadow(TASKS[next].shadow);
            core::arch::asm!("msr ttbr0_el1, {}", "dsb ishst", "tlbi vmalle1", "dsb ish", "isb",
                in(reg) TASKS[next].ttbr0, options(nostack));
            core::arch::asm!("msr tpidr_el0, {}", in(reg) TASKS[next].tpidr, options(nostack));
            cpu_switch(&raw mut TASKS[cur].sp, TASKS[next].sp);
        }
        crate::sync::irq_restore(flags);
    }
}

/// The memory layout of one address space: heap break, its floor, and where
/// the next anonymous mapping goes.
///
/// Per address space, keyed by `TTBR0` -- not per task. Threads share their
/// creator's tables, so two threads allocating at once must draw from the
/// same pool: per-task copies diverge, hand out overlapping regions, and one
/// thread's committed page lands in another's `PROT_NONE` reserve (an EL0
/// permission fault in a thread that did nothing wrong, while the committing
/// `mprotect` prints success a line later). `fork` copies the entry for the
/// child's new root; a new thread shares its creator's.
///
/// Fixed-size and linear: one entry per live process at most, scanned with
/// interrupts masked. Entries are dropped when the address space is
/// destroyed (`reap_process` for processes, `execve` for the old root).
#[derive(Clone, Copy)]
struct MmLayout {
    ttbr0: u64,
    brk: u64,
    brk_min: u64,
    mmap_next: u64,
    live: bool,
}

static mut MM: [MmLayout; MAX_TASKS] = [MmLayout {
    ttbr0: 0,
    brk: 0,
    brk_min: 0,
    mmap_next: 0,
    live: false,
}; MAX_TASKS];

/// The layout for `ttbr0`, if it has one.
fn mm_find(ttbr0: u64) -> Option<(u64, u64, u64)> {
    unsafe {
        (*(&raw const MM))
            .iter()
            .find(|e| e.live && e.ttbr0 == ttbr0)
            .map(|e| (e.brk, e.brk_min, e.mmap_next))
    }
}

fn mm_current_ttbr0() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {}, ttbr0_el1", out(reg) v, options(nomem, nostack)) };
    v
}

/// Give an address space its memory layout, just before a task enters EL0
/// in it. The root is passed explicitly: at every call site the new tables
/// exist but are not installed yet (spawn, `execve`), so reading TTBR0 would
/// name the wrong address space. It is set here rather than carried in the
/// `Process` because `brk` and `mmap` are answered from whatever thread is
/// running, and that is this one.
pub fn set_user_memory_for(ttbr0: u64, brk: u64, mmap_top: u64) {
    // One entry per root: a stale one for a recycled table address is
    // dropped first (table pages are reused after destroy).
    mm_drop(ttbr0);
    mm_create(ttbr0, brk, mmap_top);
}

/// A fresh layout for new tables. The tables are new, so no other task can
/// hold this root yet and no lock is needed beyond the write itself.
fn mm_create(ttbr0: u64, brk: u64, mmap_top: u64) {
    let flags = crate::sync::irq_save();
    unsafe {
        if let Some(e) = (*(&raw mut MM)).iter_mut().find(|e| !e.live) {
            *e = MmLayout {
                ttbr0,
                brk,
                brk_min: brk,
                mmap_next: mmap_top,
                live: true,
            };
        } else {
            panic!("out of address-space layouts");
        }
        crate::sync::irq_restore(flags);
    }
}

/// Copy the calling task's layout to a new root, for `fork`. The values are
/// read under the same mask that every reservation takes, so a sibling
/// allocating concurrently cannot slip an update between the read and the
/// copy.
fn mm_copy_to(new_ttbr0: u64) {
    let flags = crate::sync::irq_save();
    unsafe {
        let cur = mm_current_ttbr0();
        let found = (*(&raw const MM))
            .iter()
            .find(|e| e.live && e.ttbr0 == cur)
            .copied();
        if let Some(src) = found {
            if let Some(e) = (*(&raw mut MM)).iter_mut().find(|e| !e.live) {
                *e = MmLayout {
                    ttbr0: new_ttbr0,
                    brk: src.brk,
                    brk_min: src.brk_min,
                    mmap_next: src.mmap_next,
                    live: true,
                };
            } else {
                panic!("out of address-space layouts");
            }
        }
        crate::sync::irq_restore(flags);
    }
}

/// Drop the layout for `ttbr0`, if any. Called when the tables are destroyed.
/// Threads must not call this: the address space outlives any one of them.
fn mm_drop(ttbr0: u64) {
    let flags = crate::sync::irq_save();
    unsafe {
        if let Some(e) = (*(&raw mut MM))
            .iter_mut()
            .find(|e| e.live && e.ttbr0 == ttbr0)
        {
            e.live = false;
        }
        crate::sync::irq_restore(flags);
    }
}

/// Give the current task a process's memory layout, just before it enters
/// EL0. It is set here rather than carried in the `Process` because `brk` and
/// `mmap` are answered from whatever thread is running, and that is this one.
pub fn set_user_memory(brk: u64, mmap_top: u64) {
    let ttbr0 = mm_current_ttbr0();
    // A replaced-in-place root (execve reuses the caller's tables until it
    // switches) keeps one entry: drop any stale one first.
    mm_drop(ttbr0);
    mm_create(ttbr0, brk, mmap_top);
}

/// (brk, brk_min, mmap_next) for the running task's address space.
pub fn user_memory() -> (u64, u64, u64) {
    let ttbr0 = mm_current_ttbr0();
    mm_find(ttbr0).unwrap_or((0, 0, 0))
}

/// Reserve `len` bytes of anonymous mapping space, growing down from the top.
/// One critical section from read to store: two threads reserving at once
/// must not read the same `mmap_next`. Returns the base, or `None` when the
/// address space has no room. A later mapping failure leaks the reservation
/// (address space, not memory); failures there are OOM/IO, not the path.
pub fn reserve_mmap(len: u64) -> Option<u64> {
    let ttbr0 = mm_current_ttbr0();
    let flags = crate::sync::irq_save();
    let at = unsafe {
        (*(&raw mut MM))
            .iter_mut()
            .find(|e| e.live && e.ttbr0 == ttbr0)
            .and_then(|e| {
                let at = e.mmap_next.checked_sub(len)?;
                e.mmap_next = at;
                Some(at)
            })
    };
    unsafe { crate::sync::irq_restore(flags) };
    at
}

/// Commit a grown break previously mapped by the caller. Re-checks the range
/// under the mask: a sibling that moved `brk` or `mmap_next` meanwhile makes
/// this fail rather than silently overlap it. Returns the break to report.
pub fn commit_brk(addr: u64, page_want: u64) -> u64 {
    let ttbr0 = mm_current_ttbr0();
    let flags = crate::sync::irq_save();
    let ret = unsafe {
        match (*(&raw mut MM))
            .iter_mut()
            .find(|e| e.live && e.ttbr0 == ttbr0)
        {
            Some(e) if page_want >= e.brk_min && page_want < e.mmap_next => {
                e.brk = page_want;
                addr
            }
            Some(e) => e.brk,
            None => 0,
        }
    };
    unsafe { crate::sync::irq_restore(flags) };
    ret as i64 as u64
}

pub fn set_user_brk(v: u64) {
    let ttbr0 = mm_current_ttbr0();
    let flags = crate::sync::irq_save();
    unsafe {
        if let Some(e) = (*(&raw mut MM))
            .iter_mut()
            .find(|e| e.live && e.ttbr0 == ttbr0)
        {
            e.brk = v;
        }
        crate::sync::irq_restore(flags);
    }
}

pub fn set_user_mmap_next(v: u64) {
    let ttbr0 = mm_current_ttbr0();
    let flags = crate::sync::irq_save();
    unsafe {
        if let Some(e) = (*(&raw mut MM))
            .iter_mut()
            .find(|e| e.live && e.ttbr0 == ttbr0)
        {
            e.mmap_next = v;
        }
        crate::sync::irq_restore(flags);
    }
}

/// Mark the running task as being inside a system call from EL0, and return
/// what the flag was, so it can be put back. Nested is possible: `execve`
/// reads a file through Linux while itself serving a user syscall.
pub fn set_user_syscall(on: bool) -> bool {
    unsafe {
        let was = TASKS[CURRENT].user_syscall;
        TASKS[CURRENT].user_syscall = on;
        was
    }
}

pub fn set_has_console(on: bool) {
    unsafe { TASKS[CURRENT].has_console = on }
}

pub fn has_console() -> bool {
    unsafe { TASKS[CURRENT].has_console }
}

pub fn in_user_syscall() -> bool {
    unsafe { TASKS[CURRENT].user_syscall }
}

/// Record who forked whom, and answer questions about it.
/// Mark a task as sharing its creator's address space, and say where to
/// clear a thread id when it goes.
pub fn set_thread(id: usize, clear_child_tid: u64) {
    unsafe {
        TASKS[id].shares_mm = true;
        TASKS[id].clear_child_tid = clear_child_tid;
    }
}

/// Mark a task as sharing an address space it does not own, with a parent
/// waiting for it to stop doing so. The vfork half of `set_thread`.
pub fn set_vfork_child(id: usize, done: usize) {
    unsafe {
        TASKS[id].shares_mm = true;
        TASKS[id].vfork_done = done;
    }
}

/// Whether the running task is in an address space belonging to somebody
/// else -- a thread of it, or a vfork child that has not exec'd yet.
pub fn shares_mm_current() -> bool {
    unsafe { TASKS[CURRENT].shares_mm }
}

/// The running task now owns its address space: `execve` gave it one of its
/// own, so its exit must tear that one down.
pub fn own_mm_current() {
    unsafe { TASKS[CURRENT].shares_mm = false }
}

/// Take the semaphore a vfork parent is waiting on, leaving none behind: the
/// promise is kept once, whether by `execve` or by exit.
pub fn take_vfork_done() -> usize {
    unsafe {
        let v = TASKS[CURRENT].vfork_done;
        TASKS[CURRENT].vfork_done = 0;
        v
    }
}

pub fn clear_child_tid() -> u64 {
    unsafe { TASKS[CURRENT].clear_child_tid }
}

pub fn set_parent(child: usize, parent: usize) {
    unsafe { TASKS[child].parent = parent }
}

/// A child of `parent` that has finished, if there is one.
pub fn finished_child(parent: usize, want_pid: i64) -> Option<usize> {
    unsafe {
        (0..MAX_TASKS).find(|&i| {
            TASKS[i].parent == parent
                && TASKS[i].state == State::Finished
                && (want_pid <= 0 || TASKS[i].linux_pid == want_pid)
        })
    }
}

/// Whether `parent` still has a child that might yet finish. `wait4` blocks
/// only while this is true; with no children at all it has to return ECHILD
/// rather than wait for one that is never coming.
pub fn has_live_child(parent: usize, want_pid: i64) -> bool {
    unsafe {
        (0..MAX_TASKS).any(|i| {
            TASKS[i].parent == parent
                && TASKS[i].state != State::Unused
                && (want_pid <= 0 || TASKS[i].linux_pid == want_pid)
        })
    }
}

/// The task's own memory layout, for a child that inherits its parent's.
///
/// For `fork` this copies the layout to the child's new root; for a thread
/// (same root) there is nothing to do -- the layout is already shared, and
/// writing the creation-time copy back would rewind allocations siblings
/// made since. Takes the new root and whether the child shares memory.
pub fn set_user_memory_full(new_ttbr0: u64, shares_mm: bool) {
    if !shares_mm {
        mm_copy_to(new_ttbr0);
    }
}

/// Drop the calling task's address-space layout, for `execve`'s old root.
/// The new root gets its entry from `set_user_memory`.
pub fn drop_user_memory(ttbr0: u64) {
    mm_drop(ttbr0);
}

/// Shared mappings of the running task, copied out. `fork` hands them to the
/// child, which records them as pool references over its private copies.
pub fn shared_maps() -> alloc::vec::Vec<crate::user::SharedMap> {
    unsafe {
        TASKS[CURRENT].shared[..TASKS[CURRENT].nshared]
            .iter()
            .copied()
            .collect::<alloc::vec::Vec<_>>()
    }
}

/// Install the shared-mapping list for the running task. The child's entries
/// arrive in `Forked`; threads share the creator's list by copying it too --
/// same address space, same mappings.
pub fn set_shared_maps(maps: &[crate::user::SharedMap]) {
    unsafe {
        let n = maps.len().min(16);
        TASKS[CURRENT].shared[..n].copy_from_slice(&maps[..n]);
        TASKS[CURRENT].nshared = n;
    }
}

/// Record one shared mapping for the running task. False when the table is
/// full: the mapping is already in place, and the caller must undo it.
pub fn add_shared_map(m: crate::user::SharedMap) -> bool {
    unsafe {
        if TASKS[CURRENT].nshared >= 16 {
            return false;
        }
        let n = TASKS[CURRENT].nshared;
        TASKS[CURRENT].shared[n] = m;
        TASKS[CURRENT].nshared = n + 1;
        true
    }
}

/// Drop every shared-mapping record overlapping `start..start+len`, handing
/// back what was dropped so the caller can release the pool references.
pub fn remove_shared_maps(start: u64, len: u64) -> alloc::vec::Vec<crate::user::SharedMap> {
    use alloc::vec::Vec;
    unsafe {
        let mut out = Vec::new();
        let mut i = 0;
        while i < TASKS[CURRENT].nshared {
            let m = TASKS[CURRENT].shared[i];
            if m.start < start + len && start < m.start + m.len {
                out.push(m);
                TASKS[CURRENT].shared.copy_within(i + 1..TASKS[CURRENT].nshared, i);
                TASKS[CURRENT].nshared -= 1;
            } else {
                i += 1;
            }
        }
        out
    }
}

/// All shared mappings of the running task. `exit` releases every one.
pub fn take_shared_maps() -> alloc::vec::Vec<crate::user::SharedMap> {
    use alloc::vec::Vec;
    unsafe {
        let mut out = Vec::with_capacity(TASKS[CURRENT].nshared);
        for i in 0..TASKS[CURRENT].nshared {
            out.push(TASKS[CURRENT].shared[i]);
        }
        TASKS[CURRENT].nshared = 0;
        out
    }
}

/// Signal helpers: actions, masks and pending bits live in the task table
/// because they are per task (or per process, for actions), and `signal.rs`
/// owns the semantics while this owns the storage.
pub fn signal_action(id: usize, sig: u64) -> crate::signal::Action {
    unsafe { TASKS[id].sig_actions.get((sig - 1) as usize).copied().unwrap_or(crate::signal::Action::default()) }
}

pub fn set_signal_action(id: usize, sig: u64, act: crate::signal::Action) {
    unsafe {
        if sig >= 1 && sig <= 64 {
            // Threads share dispositions: writing one task's table writes
            // every task sharing its address space. The table is small and
            // sharing is by `ttbr0`, which threads share and processes do
            // not -- fork copies, threads alias.
            let root = TASKS[id].ttbr0;
            for t in TASKS.iter_mut() {
                if t.state != State::Unused && t.ttbr0 == root {
                    t.sig_actions[(sig - 1) as usize] = act;
                }
            }
        }
    }
}

pub fn signal_mask(id: usize) -> u64 {
    unsafe { TASKS[id].sig_mask }
}

pub fn set_signal_mask(id: usize, mask: u64) {
    unsafe { TASKS[id].sig_mask = mask }
}

pub fn signal_pending(id: usize) -> u64 {
    unsafe { TASKS[id].sig_pending }
}

pub fn add_signal_pending(id: usize, bits: u64) {
    unsafe { TASKS[id].sig_pending |= bits }
}

pub fn clear_signal_pending(id: usize, bits: u64) {
    unsafe { TASKS[id].sig_pending &= !bits }
}

/// Record where a signal came from, for the `siginfo` in the frame.
pub fn set_signal_source(id: usize, sig: u64, code: i32, sender: i32) {
    unsafe {
        if sig >= 1 && sig <= 64 {
            TASKS[id].sig_code[(sig - 1) as usize] = code;
            TASKS[id].sig_sender[(sig - 1) as usize] = sender;
        }
    }
}

pub fn signal_code(id: usize, sig: u64) -> i32 {
    unsafe {
        TASKS[id]
            .sig_code
            .get((sig.wrapping_sub(1)) as usize)
            .copied()
            .unwrap_or(0)
    }
}

pub fn signal_sender(id: usize, sig: u64) -> i32 {
    unsafe {
        TASKS[id]
            .sig_sender
            .get((sig.wrapping_sub(1)) as usize)
            .copied()
            .unwrap_or(0)
    }
}

/// The task with Linux pid `pid`, if it is still alive.
pub fn task_with_linux_pid(pid: i64) -> Option<usize> {
    unsafe {
        (0..MAX_TASKS).find(|&i| {
            TASKS[i].linux_pid == pid
                && (TASKS[i].state == State::Running
                    || TASKS[i].state == State::Ready
                    || TASKS[i].state == State::Blocked)
        })
    }
}

/// `(linux_pid, parent, live)` for `kill(0/-1)` group delivery.
pub fn task_identity(id: usize) -> (i64, usize, bool) {
    unsafe {
        let t = TASKS[id];
        let live = t.state == State::Running || t.state == State::Ready || t.state == State::Blocked;
        (t.linux_pid, t.parent, live)
    }
}

pub fn task_parent(id: usize) -> usize {
    unsafe { TASKS[id].parent }
}

pub fn signal_altstack(id: usize) -> (u64, u64, bool) {
    unsafe { (TASKS[id].sig_alt_base, TASKS[id].sig_alt_size, TASKS[id].sig_alt_in_use) }
}

pub fn set_signal_altstack(id: usize, base: u64, size: u64) {
    unsafe {
        TASKS[id].sig_alt_base = base;
        TASKS[id].sig_alt_size = size;
        TASKS[id].sig_alt_in_use = false;
    }
}

pub fn set_signal_altstack_in_use(id: usize, used: bool) {
    unsafe { TASKS[id].sig_alt_in_use = used }
}

pub fn set_signal_return(id: usize, mask: u64, used_alt: bool) {
    unsafe {
        TASKS[id].sig_return_mask = mask;
        TASKS[id].sig_return_alt = used_alt;
    }
}

pub fn signal_return(id: usize) -> (u64, bool) {
    unsafe { (TASKS[id].sig_return_mask, TASKS[id].sig_return_alt) }
}

/// Copy signal state to a forked child: dispositions, mask, altstack. The
/// child starts with nothing pending -- signals sent to the parent before
/// the fork are the parent's, not the child's.
pub fn inherit_signal_state(child: usize, parent: usize) {
    unsafe {
        TASKS[child].sig_actions = TASKS[parent].sig_actions;
        TASKS[child].sig_mask = TASKS[parent].sig_mask;
        TASKS[child].sig_alt_base = TASKS[parent].sig_alt_base;
        TASKS[child].sig_alt_size = TASKS[parent].sig_alt_size;
        TASKS[child].sig_alt_in_use = false;
        TASKS[child].sig_pending = 0;
        TASKS[child].sig_code = [0; 64];
        TASKS[child].sig_sender = [0; 64];
        TASKS[child].sig_return_mask = 0;
        TASKS[child].sig_return_alt = false;
    }
}

/// Install carried signal state on the running task. `fork` copies the
/// parent's dispositions into `Forked` before spawning; the child installs
/// them here, on itself, once it exists.
pub fn install_signal_state(
    actions: [crate::signal::Action; 64],
    mask: u64,
    alt_base: u64,
    alt_size: u64,
) {
    unsafe {
        TASKS[CURRENT].sig_actions = actions;
        TASKS[CURRENT].sig_mask = mask;
        TASKS[CURRENT].sig_alt_base = alt_base;
        TASKS[CURRENT].sig_alt_size = alt_size;
        TASKS[CURRENT].sig_alt_in_use = false;
        TASKS[CURRENT].sig_pending = 0;
        TASKS[CURRENT].sig_code = [0; 64];
        TASKS[CURRENT].sig_sender = [0; 64];
        TASKS[CURRENT].sig_return_mask = 0;
        TASKS[CURRENT].sig_return_alt = false;
    }
}

/// Dispositions of the running task, for carrying across `fork`.
pub fn signal_actions(id: usize) -> [crate::signal::Action; 64] {
    unsafe { TASKS[id].sig_actions }
}

/// The top of a task's kernel stack.
///
/// `execve` needs it: it never returns through the exception frame it was
/// called on, and every frame below that one is dead the moment the new
/// program starts. Without resetting the stack pointer those frames are
/// leaked for the life of the task, which a shell would notice.
pub fn kernel_stack_top() -> usize {
    kernel_stack_top_of(unsafe { CURRENT })
}

pub fn kernel_stack_top_of(id: usize) -> usize {
    unsafe { TASKS[id].stack + STACK_PAGES * PAGE }
}

pub fn bind_linux_pid(pid: i64) {
    unsafe {
        TASKS[CURRENT].linux_pid = pid;
    }
}
pub fn linux_pid(id: usize) -> i64 {
    unsafe { TASKS[id].linux_pid }
}
pub fn set_exit_status(status: i32) {
    unsafe {
        TASKS[CURRENT].exit_status = status;
    }
}
pub fn exit_status(id: usize) -> i32 {
    unsafe { TASKS[id].exit_status }
}

/// Give up the rest of this slice.
pub fn yield_now() {
    schedule();
}

/// Which task is running. The identity a semaphore or a join needs.
/// An identity for a task that is unique for the life of the machine.
///
/// The slot in the low bits, the generation above it. Linux is given this
/// rather than the slot: it keeps `cpu.owner` for as long as a thread lives
/// and compares it against `thread_self()`, so an identity that a later
/// thread can repeat makes it take a lock it never acquired or wait for a
/// thread that has gone.
pub fn uid(id: usize) -> usize {
    debug_assert!(id < MAX_TASKS);
    unsafe { ((TASKS[id].generation as usize) << 16) | id }
}

/// The slot a `uid` names, if that incarnation is still the one in it.
/// `None` once the task has exited, which is the whole point: a stale id
/// must resolve to nothing rather than to whoever took the slot next.
pub fn from_uid(uid: usize) -> Option<usize> {
    let id = uid & 0xffff;
    if id >= MAX_TASKS {
        return None;
    }
    let generation = (uid >> 16) as u64;
    unsafe { (TASKS[id].generation == generation).then_some(id) }
}

pub fn current_uid() -> usize {
    uid(current_id())
}

pub fn current_id() -> usize {
    unsafe { core::ptr::read(&raw const CURRENT) }
}

/// Stop running until somebody calls `wake`.
///
/// The caller must have arranged to be woken *before* calling this, and with
/// interrupts masked across both, or the wake can land in the gap between
/// deciding to sleep and sleeping -- the classic lost-wakeup, and the reason
/// this takes the saved interrupt state rather than masking it itself.
pub fn block(flags: u64) {
    block_on(flags, 0)
}

/// Block, recording what is being waited for so a deadlock names itself.
pub fn block_on(flags: u64, what: u32) {
    unsafe {
        (*(&raw mut TASKS))[CURRENT].waiting_on = what;
        (*(&raw mut TASKS))[CURRENT].state = State::Blocked;
        // Interrupts come back on before the switch: the task is already
        // marked blocked, so a wake arriving now sets it Ready again rather
        // than being lost.
        core::arch::asm!("msr daif, {}", in(reg) flags, options(nomem, nostack));
        schedule();
    }
}

/// A wake aimed at a task that was not blocked.
///
/// This is the shape of every lost-wakeup bug: somebody released a resource,
/// the release was recorded, and the waiter never learned of it. Counted
/// rather than assumed absent, because a lost wakeup does not fail where it
/// happens -- it fails later, as a machine where everything is waiting.
pub static mut LOST_WAKEUPS: u64 = 0;

/// Make a task runnable only if it is waiting on `what`.
///
/// `wake` does not look at what a task is blocked on, which is safe only
/// while every wait is nk's own. It is not: a task inside a system call is
/// often blocked in one of Linux's semaphores, and waking it there returns
/// it from `sem_down` without the semaphore having been given to it. LKL
/// then retries -- its `lkl_cpu_get` loops -- and increments `cpu.sleepers`
/// again on the way round, so the count of sleepers and the semaphore's own
/// count drift apart until a real `sem_up` is absorbed by a sleeper that was
/// never asleep. What comes out is a machine where everything is waiting,
/// with no single lost wakeup to point at.
///
/// The token is the rule: 0 is nk's own generic wait and may be interrupted
/// by anyone, and anything else names a primitive that only its owner may
/// wake a task out of.
pub fn wake_on(id: usize, what: u32) {
    unsafe {
        let tasks = &mut *(&raw mut TASKS);
        if id >= MAX_TASKS {
            return;
        }
        if tasks[id].state == State::Blocked && tasks[id].waiting_on != what {
            // Not ours to wake. Deliberately not counted as a lost wakeup:
            // nothing was lost, and the signal or event that prompted this
            // stays pending for whenever the task comes back on its own.
            return;
        }
        wake(id);
    }
}

/// Make a blocked task runnable, whatever it is waiting on.
///
/// Use `wake_on` unless you are the primitive the task is blocked in. See
/// the note there for what waking a task out of somebody else's semaphore
/// does.
pub fn wake(id: usize) {
    unsafe {
        let tasks = &mut *(&raw mut TASKS);
        if id >= MAX_TASKS {
            return;
        }
        match tasks[id].state {
            State::Blocked => tasks[id].state = State::Ready,
            // Already runnable: the wake is redundant, not lost.
            State::Ready | State::Running => {}
            _ => LOST_WAKEUPS += 1,
        }
    }
}

/// Wait for a task to finish.
pub fn join(id: usize) {
    loop {
        let done = unsafe {
            let t = &(*(&raw const TASKS))[id];
            t.state == State::Finished || t.state == State::Unused
        };
        if done {
            return;
        }
        yield_now();
    }
}

/// End the current task from inside it. What `task_exit` does when an entry
/// function returns, exposed for a caller that decides to stop early.
pub fn exit_current() -> ! {
    task_exit()
}

/// Called by task_start when a task's entry function returns.
#[no_mangle]
pub extern "C" fn task_exit() -> ! {
    crate::hostops::tls_cleanup();
    unsafe {
        let tasks = &mut *(&raw mut TASKS);
        tasks[CURRENT].state = State::Finished;
        // The stack is deliberately not freed here: it is the stack this code
        // is standing on. It is reclaimed by whoever reaps the slot, which
        // nothing does yet -- a real leak, bounded by MAX_TASKS, and noted
        // rather than hidden.
    }
    loop {
        schedule();
    }
}

/// Let the timer interrupt preempt. Off until the tasks exist, because a tick
/// arriving mid-`spawn` would switch to a task whose frame is half written.
pub fn enable() {
    ENABLED.store(true, Ordering::SeqCst);
}

pub fn preempt_enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

pub fn current() -> usize {
    unsafe { CURRENT }
}

pub fn report() {
    unsafe {
        let tasks = &*(&raw const TASKS);
        for (i, t) in tasks.iter().enumerate() {
            if t.state != State::Unused {
                let s = match t.state {
                    State::Running => "running",
                    State::Ready => "ready",
                    State::Blocked => "blocked",
                    State::Finished => "finished",
                    State::Unused => "",
                };
                if t.state == State::Blocked && t.waiting_on != 0 {
                    println!(
                        "          [{}] {:<10} {:<9} {} slices  waiting on {}",
                        i, t.name, s, t.slices, t.waiting_on
                    );
                } else {
                    println!(
                        "          [{}] {:<10} {:<9} {} slices",
                        i, t.name, s, t.slices
                    );
                }
            }
        }
    }
}

/// Reap only a joined process task. Its stack is now inactive and its TLS
/// destructors have already released the Linux task.
pub fn reap_process(id: usize) {
    let flags = crate::sync::irq_save();
    unsafe {
        assert_ne!(id, CURRENT);
        let task = TASKS[id];
        assert!(task.state == State::Finished && task.linux_pid > 1);
        // A thread's address space belongs to the threads it shared it with,
        // and they are still running in it.
        if !task.shares_mm {
            crate::paging::destroy_user_address_space(task.ttbr0);
            mm_drop(task.ttbr0);
        }
        for i in 0..STACK_PAGES {
            frames::free((task.stack + i * PAGE) as *mut u8);
        }
        frames::free(task.shadow as *mut u8);
        TASKS[id].state = State::Unused;
        crate::sync::irq_restore(flags);
    }
}

pub fn record_user_irq() {
    unsafe {
        TASKS[CURRENT].user_irqs += 1;
    }
}
pub fn user_irqs(id: usize) -> u64 {
    unsafe { TASKS[id].user_irqs }
}

/// Whether the init process the watchdog waits for is still running.
///
/// Set once, after `user::launch`, from the boot task; read by the watchdog.
/// The watchdog used to stop the machine after four one-second rounds no
/// matter what init was doing, which guillotined every test longer than four
/// seconds mid-syscall and read exactly like a kernel wedge: the process
/// READY-spinning, timers advancing, one ppoll in flight with its hrtimer
/// still queued in the future. Now it waits for init and only backstops a
/// boot that never finishes.
static mut INIT_TASK: usize = usize::MAX;

pub fn set_init_task(id: usize) {
    unsafe {
        INIT_TASK = id;
    }
}

/// True once init has exited (or was never launched): the watchdog may stop
/// the machine. A task that finished but has not been reaped yet still counts
/// as done -- reaping is the boot task's job, not the watchdog's.
pub fn init_done() -> bool {
    unsafe {
        let id = INIT_TASK;
        if id == usize::MAX {
            return false;
        }
        matches!(
            (*(&raw const TASKS))[id].state,
            State::Finished | State::Unused
        )
    }
}
