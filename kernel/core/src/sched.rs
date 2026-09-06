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
    tpidr: 0,
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
            tpidr: 0,
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

/// Give the current task a process's memory layout, just before it enters
/// EL0. It is set here rather than carried in the `Process` because `brk` and
/// `mmap` are answered from whatever thread is running, and that is this one.
pub fn set_user_memory(brk: u64, mmap_top: u64) {
    unsafe {
        TASKS[CURRENT].brk = brk;
        TASKS[CURRENT].brk_min = brk;
        TASKS[CURRENT].mmap_next = mmap_top;
    }
}

/// (brk, brk_min, mmap_next) for the running task.
pub fn user_memory() -> (u64, u64, u64) {
    unsafe { (TASKS[CURRENT].brk, TASKS[CURRENT].brk_min, TASKS[CURRENT].mmap_next) }
}

pub fn set_user_brk(v: u64) {
    unsafe { TASKS[CURRENT].brk = v }
}

pub fn set_user_mmap_next(v: u64) {
    unsafe { TASKS[CURRENT].mmap_next = v }
}

/// The top of the running task's kernel stack.
///
/// `execve` needs it: it never returns through the exception frame it was
/// called on, and every frame below that one is dead the moment the new
/// program starts. Without resetting the stack pointer those frames are
/// leaked for the life of the task, which a shell would notice.
pub fn kernel_stack_top() -> usize {
    unsafe { TASKS[CURRENT].stack + STACK_PAGES * PAGE }
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

/// Make a blocked task runnable. Safe from interrupt context.
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
        crate::paging::destroy_user_address_space(task.ttbr0);
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
