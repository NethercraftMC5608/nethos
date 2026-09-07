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
use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering::SeqCst};

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

/// Every semaphore and mutex gets a number, and every blocked task records
/// which one it is waiting on.
///
/// Without this, a deadlock reports itself as "every task is blocked", which
/// is the fact rather than the cause. With it, the watchdog prints who waits
/// on what and what each of those holds -- a wait graph, which either names
/// the cycle or names the thing nobody raised.
static mut NEXT_ID: u32 = 1;

const MAX_TRACKED: usize = 64;
static mut REGISTRY: [*const Semaphore; MAX_TRACKED] =
    [core::ptr::null(); MAX_TRACKED];
static mut MUTEXES: [*const Mutex; MAX_TRACKED] = [core::ptr::null(); MAX_TRACKED];

fn next_id() -> u32 {
    unsafe {
        let flags = irq_save();
        let id = NEXT_ID;
        NEXT_ID += 1;
        irq_restore(flags);
        id
    }
}

/// Remember a semaphore so the watchdog can report its state. Only tracks the
/// first `MAX_TRACKED`; beyond that a semaphore still works, it is simply not
/// in the report.
pub fn track(s: &Semaphore) {
    unsafe {
        let flags = irq_save();
        let n = &mut *(&raw mut REGISTRY);
        for slot in n.iter_mut() {
            if slot.is_null() {
                *slot = s as *const Semaphore;
                break;
            }
        }
        irq_restore(flags);
    }
}

/// Remember a mutex. Ids come from the same counter as semaphores, so a
/// wait graph that reported only one of the two showed tasks waiting on
/// numbers that appeared nowhere -- which reads as corruption rather than as
/// the other kind of object.
pub fn track_mutex(m: &Mutex) {
    unsafe {
        let flags = irq_save();
        let n = &mut *(&raw mut MUTEXES);
        for slot in n.iter_mut() {
            if slot.is_null() {
                *slot = m as *const Mutex;
                break;
            }
        }
        irq_restore(flags);
    }
}

/// Print every tracked semaphore and mutex that anybody is waiting on, and
/// for a mutex, who holds it -- which is the edge that closes a deadlock.
pub fn report() {
    unsafe {
        for slot in (*(&raw const REGISTRY)).iter() {
            if slot.is_null() {
                continue;
            }
            let s = &**slot;
            let (count, waiters) = (s.count.load(SeqCst), s.waiters.load(SeqCst));
            if waiters != 0 || count != 0 {
                crate::println!(
                    "          sem   {:<3} count {:<4} waiters {:#018b} ups {} downs {}",
                    s.id,
                    count,
                    waiters,
                    s.ups.load(SeqCst),
                    s.downs.load(SeqCst)
                );
            }
        }
        for slot in (*(&raw const MUTEXES)).iter() {
            if slot.is_null() {
                continue;
            }
            let m = &**slot;
            let waiters = m.waiters.load(SeqCst);
            if waiters != 0 || m.owner().is_some() {
                match m.owner() {
                    Some(o) => crate::println!(
                        "          mutex {:<3} held by task {:<2} depth {}  waiters {:#018b}",
                        m.id, o, m.depth.load(SeqCst), waiters
                    ),
                    None => crate::println!(
                        "          mutex {:<3} free            waiters {:#018b}",
                        m.id, waiters
                    ),
                }
            }
        }
    }
}

/// Atomics, and `&self` rather than `&mut self`, and both are the fix for a
/// real bug rather than style.
///
/// `down` blocks in the middle of its own critical section: it holds a
/// reference to the semaphore across a context switch, while another task
/// mutates the same object through a reference of its own. With `&mut self`
/// that is aliasing undefined behaviour, and the compiler acts on it -- it may
/// keep `count` in a register across the switch and re-test the stale value
/// when the task resumes. The task then blocks again on a semaphore that was
/// raised while it slept, re-arming its own waiter bit each time.
///
/// It presents as a semaphore holding a token with somebody still waiting for
/// it, which is a state the code plainly cannot produce, and as a machine
/// where every thread is blocked. Nothing reports it.
pub struct Semaphore {
    pub id: u32,
    count: AtomicI32,
    waiters: AtomicU64,
    /// Lifetime totals. The watchdog prints them for any semaphore with a
    /// waiter: `ups` counted but the waiter still there means the wake went
    /// somewhere else (or nowhere); no `ups` at all means nobody left is in
    /// a position to wake it -- the scheduler is dead, not lossy.
    ups: AtomicU64,
    /// Times `down` found no token and parked. Fast-path takes leave no
    /// trace; every one of these is a sleep that needed a matching wake.
    downs: AtomicU64,
}

impl Semaphore {
    pub fn new(count: i32) -> Self {
        Semaphore {
            id: next_id(),
            count: AtomicI32::new(count),
            waiters: AtomicU64::new(0),
            ups: AtomicU64::new(0),
            downs: AtomicU64::new(0),
        }
    }

    pub fn down(&self) {
        loop {
            let flags = irq_save();
            let count = self.count.load(SeqCst);
            if count > 0 {
                self.count.store(count - 1, SeqCst);
                unsafe { irq_restore(flags) };
                return;
            }
            self.waiters.fetch_or(1u64 << sched::current_id(), SeqCst);
            self.downs.fetch_add(1, SeqCst);
            // Marks this task blocked, restores `flags`, and switches away.
            // An `up` arriving after the bit is set but before the switch
            // finds the task already blocked and makes it ready again.
            sched::block_on(flags, self.id);
        }
    }

    pub fn up(&self) {
        let flags = irq_save();
        self.count.fetch_add(1, SeqCst);
        self.ups.fetch_add(1, SeqCst);
        // Exactly one waiter, not all of them.
        //
        // Waking all of them and letting the losers re-check looks harmless
        // and is not. A caller that counts its own sleepers -- one `up` per
        // sleeper, which is how LKL's CPU lock is written -- sees each
        // spurious wakeup re-enter its wait loop and increment that count
        // again. The bookkeeping inflates, the ups and downs stop matching,
        // and the result is a set of threads that are each certain somebody
        // else holds the thing they are waiting for.
        let waiting = self.waiters.load(SeqCst);
        let woken = if waiting != 0 {
            let id = waiting.trailing_zeros();
            self.waiters.fetch_and(!(1u64 << id), SeqCst);
            Some(id as usize)
        } else {
            None
        };
        unsafe { irq_restore(flags) };
        if let Some(id) = woken {
            sched::wake(id);
        }
    }
}

/// A sleeping mutex, optionally recursive.
///
/// Recursion is not an indulgence: Linux's host interface asks for it by
/// name, because some of its locks are taken again from inside a callback the
/// lock holder made.
/// Atomic for the same reason as `Semaphore`; see the note there.
const NO_OWNER: usize = usize::MAX;

pub struct Mutex {
    pub id: u32,
    owner: AtomicUsize,
    depth: AtomicU32,
    recursive: bool,
    waiters: AtomicU64,
}

impl Mutex {
    pub fn new(recursive: bool) -> Self {
        Mutex {
            id: next_id(),
            owner: AtomicUsize::new(NO_OWNER),
            depth: AtomicU32::new(0),
            recursive,
            waiters: AtomicU64::new(0),
        }
    }

    pub fn owner(&self) -> Option<usize> {
        match self.owner.load(SeqCst) {
            NO_OWNER => None,
            o => Some(o),
        }
    }

    pub fn lock(&self) {
        let me = sched::current_id();
        loop {
            let flags = irq_save();
            let owner = self.owner.load(SeqCst);
            if owner == NO_OWNER {
                self.owner.store(me, SeqCst);
                self.depth.store(1, SeqCst);
                unsafe { irq_restore(flags) };
                return;
            }
            if owner == me {
                if !self.recursive {
                    unsafe { irq_restore(flags) };
                    panic!("deadlock: task {} took a non-recursive mutex twice", me);
                }
                self.depth.fetch_add(1, SeqCst);
                unsafe { irq_restore(flags) };
                return;
            }
            self.waiters.fetch_or(1u64 << me, SeqCst);
            sched::block_on(flags, self.id);
        }
    }

    pub fn unlock(&self) {
        let flags = irq_save();
        if self.depth.fetch_sub(1, SeqCst) > 1 {
            unsafe { irq_restore(flags) };
            return;
        }
        self.owner.store(NO_OWNER, SeqCst);
        // One waiter here too: only one of them can take the mutex, and
        // waking the rest to discover that is churn a single-CPU scheduler
        // pays for in full.
        let waiting = self.waiters.load(SeqCst);
        let woken = if waiting != 0 {
            let id = waiting.trailing_zeros();
            self.waiters.fetch_and(!(1u64 << id), SeqCst);
            Some(id as usize)
        } else {
            None
        };
        unsafe { irq_restore(flags) };
        if let Some(id) = woken {
            sched::wake(id);
        }
    }
}
