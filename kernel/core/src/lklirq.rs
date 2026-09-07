//! Raising Linux's interrupts, from a thread rather than from a handler.
//!
//! nk is the machine: a device interrupt arrives at nk's GIC, and Linux has to
//! be told. `lkl_trigger_irq` is how, and it takes LKL's CPU lock -- so it
//! cannot be called from an interrupt handler, which would block whichever
//! task was interrupted on a half-finished exception stack. That is the same
//! mistake nk made once with timers and once with the console, and this exists
//! so the next device does not make it a third time.
//!
//! So a handler marks and this raises. The mark is a bitmask because a handler
//! must not allocate and because two devices can be pending at once.

use crate::sched;
use crate::sync::{irq_restore, irq_save};

extern "C" {
    fn lkl_trigger_irq(irq: i32) -> i32;
    /// Reserve an interrupt number from LKL. Host-callable by design: this is
    /// how LKL's own disk and network backends get one.
    fn lkl_get_free_irq(user: *const u8) -> i32;
}

/// Interrupts 0..127. LKL hands them out from the bottom and nk will have a
/// handful; a bitmask keeps `raise` to a few instructions with interrupts
/// masked, which is what makes it safe in a handler.
static mut PENDING: u128 = 0;
static mut PUMP: usize = 0;

/// Which LKL interrupt a GIC interrupt stands for.
///
/// nk's GIC is the real one and LKL's is a bitmask of its own, so a device
/// interrupt has two numbers and something has to hold the pair. Small and
/// linear because there are a handful: the console, and one per virtio
/// transport that has anything behind it.
static mut MAP: [(u32, i32); 16] = [(0, -1); 16];

pub fn map_gic(intid: u32, lkl: i32) {
    unsafe {
        let flags = irq_save();
        if let Some(slot) = (*(&raw mut MAP)).iter_mut().find(|(_, l)| *l < 0) {
            *slot = (intid, lkl);
        }
        irq_restore(flags);
    }
}

/// The GIC interrupt an LKL one came from, for putting the mask back.
pub fn gic_for(lkl: i32) -> Option<u32> {
    unsafe {
        (*(&raw const MAP))
            .iter()
            .find(|(_, l)| *l == lkl)
            .map(|(g, _)| *g)
    }
}

/// The LKL interrupt for a GIC one, if nk gave that device to Linux.
pub fn for_gic(intid: u32) -> Option<i32> {
    unsafe {
        (*(&raw const MAP))
            .iter()
            .find(|(g, l)| *l >= 0 && *g == intid)
            .map(|(_, l)| *l)
    }
}

/// Reserve an LKL interrupt number.
///
/// # Safety
/// Linux must be up.
pub unsafe fn reserve(name: &core::ffi::CStr) -> Option<i32> {
    let irq = lkl_get_free_irq(name.as_ptr() as *const u8);
    (irq >= 0 && irq < 128).then_some(irq)
}

/// Mark an interrupt to be raised. Safe from a handler: it masks, sets a bit,
/// and wakes a thread.
pub fn raise(irq: i32) {
    if !(0..128).contains(&irq) {
        return;
    }
    unsafe {
        let flags = irq_save();
        PENDING |= 1u128 << irq;
        // Zero until the thread exists, and task zero is the boot thread:
        // waking it because a device spoke early would mark it ready while it
        // waits on something else entirely.
        if PUMP != 0 {
            sched::wake(PUMP);
        }
        irq_restore(flags);
    }
}

extern "C" fn pump(_: usize) {
    loop {
        let flags = irq_save();
        let due = unsafe { core::mem::take(&mut *(&raw mut PENDING)) };
        if due == 0 {
            sched::block(flags);
            continue;
        }
        unsafe { irq_restore(flags) };

        for irq in 0..128 {
            if due & (1u128 << irq) != 0 {
                unsafe { lkl_trigger_irq(irq) };
                // Linux has had it; the device's line can be listened to
                // again. If its driver has not run yet the line is still
                // asserted and this fires once more, which is a retry rather
                // than a storm.
                if let Some(gic) = gic_for(irq) {
                    unsafe { crate::gic::unmask_spi(gic) };
                }
            }
        }
    }
}

pub fn start() {
    let id = sched::spawn("lkl-irq", pump, 0);
    unsafe { PUMP = id };
    // Anything a device said while this was being set up.
    let flags = irq_save();
    if unsafe { PENDING } != 0 {
        sched::wake(id);
    }
    unsafe { irq_restore(flags) };
}
