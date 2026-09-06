//! Semaphores and mutexes that actually sleep.
//!
//! nk's first locks masked interrupts and its first waits spun on `yield_now`.
//! Both are correct on one CPU and neither can carry a kernel: a spinning
//! waiter is indistinguishable from a busy one, so the machine can never go
//! idle, and a lock held across a device operation burns every remaining
//! slice of every other task. Linux's core assumes real blocking everywhere --
//! it is what `struct semaphore`, `struct mutex` and every wait queue are.
//!
//! The waiter set is a bitmask of task slots rather than a list. Sixteen task
//! slots fit in a `u32`, wakeups need no allocation, and a waiter cannot be
//! recorded twice -- which a list has to be careful about and a bitmask
//! cannot get wrong.
//!
//! **The ordering matters and is the classic bug.** Between deciding to sleep
//! and sleeping there is a window in which the wakeup can arrive and be lost,
//! and the task then waits for an event that has already happened. So the
//! waiter bit is set with interrupts masked, and `sched::block` is handed the
//! saved interrupt state to restore *after* the task is marked blocked --
//! never before.

use crate::sched;

/// Mask interrupts and report the previous state. Public because every
/// primitive that has to be safe against an interrupt handler needs it, and
/// duplicating two instructions per user is how they drift.
pub fn irq_save() -> u64 {
    let daif: u64;
    unsafe {
        core::arch::asm!("mrs {}, daif", "msr daifset, #0x2", out(reg) daif, options(nomem, nostack))
    };
    daif
}

/// # Safety
/// `flags` came from `irq_save`.
pub unsafe fn irq_restore(flags: u64) {
    core::arch::asm!("msr daif, {}", in(reg) flags, options(nomem, nostack));
}

pub struct Semaphore {
    count: i32,
    waiters: u32,
}

impl Semaphore {
    pub const fn new(count: i32) -> Self {
        Semaphore { count, waiters: 0 }
    }

    pub fn down(&mut self) {
        loop {
            let flags = irq_save();
            if self.count > 0 {
                self.count -= 1;
                unsafe { irq_restore(flags) };
                return;
            }
            self.waiters |= 1 << sched::current_id();
            // Marks this task blocked, restores `flags`, and switches away.
            // An `up` arriving after the bit is set but before the switch
            // finds the task already blocked and makes it ready again.
            sched::block(flags);
        }
    }

    pub fn up(&mut self) {
        let flags = irq_save();
        self.count += 1;
        // Wake every waiter rather than choosing one. With a bitmask that is
        // one pass and no bookkeeping; the cost is that n-1 of them re-check
        // and block again, which on one CPU is a handful of switches and is
        // simpler than a queue that has to stay in order.
        let waiting = core::mem::take(&mut self.waiters);
        unsafe { irq_restore(flags) };
        for id in 0..32 {
            if waiting & (1 << id) != 0 {
                sched::wake(id);
            }
        }
    }
}

/// A sleeping mutex, optionally recursive.
///
/// Recursion is not an indulgence: Linux's host interface asks for it by
/// name, because some of its locks are taken again from inside a callback the
/// lock holder made.
pub struct Mutex {
    owner: Option<usize>,
    depth: u32,
    recursive: bool,
    waiters: u32,
}

impl Mutex {
    pub const fn new(recursive: bool) -> Self {
        Mutex { owner: None, depth: 0, recursive, waiters: 0 }
    }

    pub fn lock(&mut self) {
        let me = sched::current_id();
        loop {
            let flags = irq_save();
            match self.owner {
                None => {
                    self.owner = Some(me);
                    self.depth = 1;
                    unsafe { irq_restore(flags) };
                    return;
                }
                Some(o) if o == me && self.recursive => {
                    self.depth += 1;
                    unsafe { irq_restore(flags) };
                    return;
                }
                Some(o) if o == me => {
                    unsafe { irq_restore(flags) };
                    panic!("deadlock: task {} took a non-recursive mutex twice", me);
                }
                _ => {
                    self.waiters |= 1 << me;
                    sched::block(flags);
                }
            }
        }
    }

    pub fn unlock(&mut self) {
        let flags = irq_save();
        self.depth -= 1;
        if self.depth > 0 {
            unsafe { irq_restore(flags) };
            return;
        }
        self.owner = None;
        let waiting = core::mem::take(&mut self.waiters);
        unsafe { irq_restore(flags) };
        for id in 0..32 {
            if waiting & (1 << id) != 0 {
                sched::wake(id);
            }
        }
    }
}
