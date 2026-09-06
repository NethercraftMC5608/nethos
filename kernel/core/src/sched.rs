//! Threads, and a round robin over them.
//!
//! Kernel threads only: one address space, no user mode, no priorities. That
//! is not a placeholder for something better -- it is what the drivers need.
//! Linux's `kthread`, workqueues and the softirq machinery all sit on exactly
//! this, and Stage 3's shim will map onto it directly.
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
const STACK_PAGES: usize = 4;
const MAX_TASKS: usize = 16;

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    Unused,
    Ready,
    Running,
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
}

static mut TASKS: [Task; MAX_TASKS] = [Task {
    sp: 0,
    stack: 0,
    shadow: 0,
    state: State::Unused,
    name: "",
    slices: 0,
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
        tasks[slot] =
            Task { sp, stack: stack as usize, shadow, state: State::Ready, name, slices: 0 };
        slot
    }
}

/// Pick the next ready task and go to it. Safe to call with nothing else
/// runnable: it returns immediately, and the caller carries on.
pub fn schedule() {
    unsafe {
        let tasks = &mut *(&raw mut TASKS);
        let cur = CURRENT;

        // Round robin: start looking after the current slot, so a task cannot
        // starve the ones behind it by being ready every time.
        let mut next = None;
        for i in 1..=MAX_TASKS {
            let c = (cur + i) % MAX_TASKS;
            if tasks[c].state == State::Ready {
                next = Some(c);
                break;
            }
        }
        let Some(next) = next else { return };

        if tasks[cur].state == State::Running {
            tasks[cur].state = State::Ready;
        }
        tasks[next].state = State::Running;
        tasks[next].slices += 1;
        CURRENT = next;

        // Before the switch, not after: cpu_switch does not return here, it
        // returns into the incoming task, which may be Linux code that reads
        // its stack canary through SP_EL0 in its very first instruction.
        set_shadow(tasks[next].shadow);

        let prev_sp: *mut usize = &raw mut tasks[cur].sp;
        cpu_switch(prev_sp, tasks[next].sp);
    }
}

/// Give up the rest of this slice.
pub fn yield_now() {
    schedule();
}

/// Called by task_start when a task's entry function returns.
#[no_mangle]
pub extern "C" fn task_exit() -> ! {
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
                    State::Finished => "finished",
                    State::Unused => "",
                };
                println!("          [{}] {:<10} {:<9} {} slices", i, t.name, s, t.slices);
            }
        }
    }
}
