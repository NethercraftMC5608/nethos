//! PSCI: asking the firmware to turn the machine off.
//!
//! nk cannot power down a machine itself -- that is below EL1, in whatever
//! runs the platform. PSCI is the standard way to ask, and QEMU's `virt`
//! implements it, so `nk_poweroff` ends a run cleanly instead of leaving a
//! kernel spinning until something outside kills it.
//!
//! That matters more than it sounds for the tests. Killing QEMU on a watchdog
//! makes "the kernel finished" and "the kernel hung" look identical from the
//! outside -- both end with a signal after N seconds -- and that ambiguity is
//! exactly what made one silent hang take far longer to pin down than it
//! should have. A clean exit is a signal in itself.
//!
//! The call method comes from the device tree's `psci` node, which says `hvc`
//! or `smc` depending on whether firmware sits at EL2 or EL3. QEMU's virt
//! under HVF says `hvc`. Assuming one would work on this machine and fail on
//! the next, in a way that presents as the machine simply not switching off.

use crate::dt::Fdt;

const SYSTEM_OFF: u32 = 0x8400_0008;

#[derive(Clone, Copy, PartialEq)]
enum Method {
    None,
    Hvc,
    Smc,
}

static mut METHOD: Method = Method::None;

pub fn init(fdt: &Fdt) {
    let m = fdt
        .find_compatible("arm,psci-0.2")
        .or_else(|| fdt.find_compatible("arm,psci"))
        .and_then(|n| n.prop("method"))
        .map(|v| if v.starts_with(b"smc") { Method::Smc } else { Method::Hvc })
        .unwrap_or(Method::None);
    unsafe { METHOD = m };
}

/// Turn the machine off. Returns only if the firmware refuses.
pub fn poweroff() {
    unsafe {
        match core::ptr::read(&raw const METHOD) {
            Method::Hvc => core::arch::asm!("hvc #0", in("x0") SYSTEM_OFF as u64, options(nostack)),
            Method::Smc => core::arch::asm!("smc #0", in("x0") SYSTEM_OFF as u64, options(nostack)),
            Method::None => {}
        }
    }
}
