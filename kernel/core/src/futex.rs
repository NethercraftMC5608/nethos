//! The futex, which is nk's for the same reason `mmap` and `fork` are.
//!
//! A futex is an address two threads agree on: one sleeps until the value
//! there changes, another wakes it. Everything about that is a fact about the
//! *address space*, and the address space is nk's -- Linux has no mapping for
//! it and could not hash it into the same bucket for two threads even if it
//! did. Forwarding `futex` to Linux would give each thread a private queue
//! and a `pthread_join` that never returns.
//!
//! So the key is `(TTBR0, address)`. Two threads share an address space, so
//! they share a key; two processes do not, so the same numeric address in
//! each is a different futex, which is what `FUTEX_PRIVATE_FLAG` means.
//!
//! Small and linear on purpose. A hash table is the right shape at scale and
//! this is not at scale: a handful of threads, each waiting on one address.
//! Making it a table is a change to this file and nothing else.

use crate::sched;
use crate::sync::{irq_restore, irq_save};
use crate::uaccess;

const WAIT: u64 = 0;
const WAKE: u64 = 1;
const WAIT_BITSET: u64 = 9;
const WAKE_BITSET: u64 = 10;
/// Everything glibc asks for is private, and the private flag is not part of
/// the operation; it says only that the key need not be global.
const PRIVATE: u64 = 128;
const CLOCK_REALTIME: u64 = 256;

const MAX_WAITERS: usize = 64;

#[derive(Clone, Copy)]
struct Waiter {
    space: u64,
    addr: u64,
    task: usize,
    /// Which bits of a `*_BITSET` wake this waiter answers to. Plain WAIT is
    /// the same thing with every bit set, which is why there is one path.
    bits: u32,
    live: bool,
    /// Set by a waker before it makes the task ready, so a wake that arrives
    /// between the value check and the block is not lost.
    woken: bool,
}

static mut WAITERS: [Waiter; MAX_WAITERS] = [Waiter {
    space: 0,
    addr: 0,
    task: 0,
    bits: 0,
    live: false,
    woken: false,
}; MAX_WAITERS];

/// The address space a futex belongs to.
fn space() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {}, ttbr0_el1", out(reg) v, options(nomem, nostack)) };
    crate::paging::table_of(v)
}

/// # futex(uaddr, op, val, timeout, uaddr2, val3)
pub fn futex(uaddr: u64, op: u64, val: u32, _timeout: u64, _uaddr2: u64, val3: u32) -> i64 {
    match op & !(PRIVATE | CLOCK_REALTIME) {
        WAIT => wait(uaddr, val, !0),
        WAIT_BITSET => wait(uaddr, val, val3),
        WAKE => wake(uaddr, val, !0),
        WAKE_BITSET => wake(uaddr, val, val3),
        other => {
            crate::println!("  futex: operation {} is not implemented", other);
            -38 // -ENOSYS
        }
    }
}

/// Sleep while the value at `uaddr` is still `val`.
///
/// The comparison and the decision to sleep have to be one step as far as any
/// waker is concerned, or a wake can land in between and be lost -- the
/// classic way to build a `pthread_join` that hangs once a week. Interrupts
/// are masked across both, and `block_on` puts them back only after the task
/// is marked blocked.
fn wait(uaddr: u64, val: u32, bits: u32) -> i64 {
    if bits == 0 {
        return -22; // -EINVAL: a wait no wake could ever match
    }
    let flags = irq_save();

    let mut buf = [0u8; 4];
    if uaccess::copy_from_user(&mut buf, uaddr).is_err() {
        unsafe { irq_restore(flags) };
        return uaccess::EFAULT;
    }
    if u32::from_le_bytes(buf) != val {
        // Somebody changed it before we got here. That is not a failure: it
        // is the whole reason the value is passed in.
        unsafe { irq_restore(flags) };
        return -11; // -EAGAIN
    }

    let me = sched::current_id();
    let slot = unsafe {
        let w = &mut *(&raw mut WAITERS);
        let Some(slot) = w.iter().position(|s| !s.live) else {
            irq_restore(flags);
            return -12; // -ENOMEM: nk is out of waiters, and says so
        };
        w[slot] = Waiter {
            space: space(),
            addr: uaddr,
            task: me,
            bits,
            live: true,
            woken: false,
        };
        slot
    };

    // Not blocked if a wake already happened: `woken` is set with interrupts
    // masked, so this test cannot miss one.
    if !unsafe { (*(&raw const WAITERS))[slot].woken } {
        sched::block_on(flags, 0);
    } else {
        unsafe { irq_restore(flags) };
    }

    let flags = irq_save();
    unsafe { (*(&raw mut WAITERS))[slot].live = false };
    unsafe { irq_restore(flags) };
    0
}

/// Wake at most `n` waiters on `uaddr` whose bits overlap `bits`.
fn wake(uaddr: u64, n: u32, bits: u32) -> i64 {
    let flags = irq_save();
    let here = space();
    let mut woken = 0;
    unsafe {
        let w = &mut *(&raw mut WAITERS);
        for s in w.iter_mut() {
            if woken >= n {
                break;
            }
            if s.live && !s.woken && s.space == here && s.addr == uaddr && s.bits & bits != 0 {
                s.woken = true;
                sched::wake(s.task);
                woken += 1;
            }
        }
        irq_restore(flags);
    }
    woken as i64
}

/// Every waiter still asleep. Printed by the watchdog, which runs only once
/// something has stopped making progress, so it costs nothing on the path it
/// is diagnosing -- which is the point: a futex bug is a hang, and a hang is
/// exactly the state in which no other diagnostic is running.
pub fn report() {
    let flags = irq_save();
    unsafe {
        for (i, s) in (*(&raw const WAITERS)).iter().enumerate() {
            if s.live {
                crate::println!(
                    // Not the value at the address: this runs on the
                    // watchdog, whose address space is the kernel's, so
                    // reading a user address here fails and says nothing.
                    "          futex[{}] task {} on {:#x} space {:#x}{}",
                    i,
                    s.task,
                    s.addr,
                    s.space,
                    if s.woken { " (woken)" } else { "" }
                );
            }
        }
        irq_restore(flags);
    }
}

/// Forget a task's waiters. A thread killed while waiting would otherwise
/// leave a slot claiming it, and a later wake would make a dead task ready.
pub fn forget(task: usize) {
    let flags = irq_save();
    unsafe {
        for s in (*(&raw mut WAITERS)).iter_mut() {
            if s.live && s.task == task {
                s.live = false;
            }
        }
        irq_restore(flags);
    }
}
