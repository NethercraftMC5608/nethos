//! The virtual timer, and the tick everything else is paced by.
//!
//! **CNTV_*_EL0, not CNTP_*.** The device tree lists four timer interrupts --
//! secure physical, non-secure physical, virtual, hypervisor -- and the
//! physical one at index 1 looks like the obvious choice for a kernel running
//! at EL1. It is not, and this cost a real debugging session:
//!
//! Under a hypervisor, EL2 belongs to the hypervisor and the physical timer
//! belongs with it. Apple's Hypervisor.framework does not set
//! CNTHCTL_EL2.EL1PCEN for its guests, so `msr CNTP_TVAL_EL0` from EL1 traps
//! -- and it does not trap as a recognisable system-register access. It
//! arrives as a synchronous exception with EC 0 ("unknown reason"), which says
//! nothing at all about what happened. It was identified by disassembling
//! around ELR, not by reading the syndrome.
//!
//! The virtual timer works in both worlds: bare metal at EL1 with CNTVOFF
//! zero, and under any hypervisor, which is precisely what it exists for.
//! Linux picks it for the same reason whenever it does not own EL2. boot.s
//! still zeroes CNTVOFF_EL2 on the way down from EL2, which is what keeps the
//! virtual counter equal to the physical one when nk *is* the hypervisor's
//! level.
//!
//! A count-down, not a deadline. CNTV_TVAL_EL0 is written with an interval and
//! the timer fires when it reaches zero, which is one register per tick and no
//! arithmetic that can drift into the past -- the failure mode of writing an
//! absolute CNTP_CVAL that has already been passed is a timer that never
//! fires again.

use crate::println;
use core::sync::atomic::{AtomicU64, Ordering};

/// Ticks per second. 100 is a compromise nobody argues with: fine enough that
/// a round-robin slice is imperceptible, coarse enough that the handler is
/// not a measurable fraction of the machine.
pub const HZ: u64 = 100;

static TICKS: AtomicU64 = AtomicU64::new(0);
static mut INTERVAL: u64 = 0;
static mut INTID: u32 = 27;

fn interval() -> u64 {
    // Through a raw pointer rather than a reference: a shared reference to a
    // mutable static is undefined behaviour the moment anything writes it, and
    // rearm() does, from interrupt context.
    unsafe { core::ptr::read(&raw const INTERVAL) }
}

pub fn frequency() -> u64 {
    let f: u64;
    unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack)) };
    f
}

/// # Safety
/// The GIC must be up; `intid` is the PPI the device tree named.
pub unsafe fn init(intid: u32) {
    let freq = frequency();
    assert!(freq != 0, "CNTFRQ_EL0 is zero -- firmware never set the timer frequency");
    INTID = intid;
    INTERVAL = freq / HZ;
    crate::gic::enable_ppi(intid);
    rearm();
    // ENABLE, with IMASK clear. Both matter: an enabled timer with IMASK set
    // counts down and fires nothing.
    core::arch::asm!("msr cntv_ctl_el0, {}", in(reg) 1u64, options(nomem, nostack));
    println!(
        "  timer:  {} Hz on PPI {} ({} MHz counter, {} ticks/interval)",
        HZ,
        intid,
        freq / 1_000_000,
        interval()
    );
}

/// # Safety
/// Called from the interrupt handler, or from init.
pub unsafe fn rearm() {
    core::arch::asm!("msr cntv_tval_el0, {}", in(reg) INTERVAL, options(nomem, nostack));
}

pub fn intid() -> u32 {
    unsafe { INTID }
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn on_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Milliseconds since the timer started. Derived from the tick count rather
/// than read from the counter, so it moves only when interrupts are actually
/// being delivered -- which makes it useful evidence that they are.
pub fn ms() -> u64 {
    ticks() * 1000 / HZ
}
