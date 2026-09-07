//! GICv3 — the interrupt controller.
//!
//! v3 rather than v2 on purpose. QEMU's `virt` still defaults to v2 and
//! run-kernel.sh asks for v3 explicitly, because v3 is what every ARM machine
//! made since roughly 2015 has, and learning the one that is being retired
//! would be work spent twice.
//!
//! Three pieces have to agree before a single interrupt arrives, and each is
//! silent when it is the one missing:
//!
//! - the **distributor**, one per machine, which routes shared interrupts;
//! - a **redistributor**, one per CPU, which owns that CPU's private ones --
//!   the timer is private, so almost everything here is redistributor work;
//! - the **CPU interface**, which in v3 is not memory-mapped at all but a set
//!   of system registers, and which has to be switched on before the
//!   registers even exist.

use crate::println;

// Distributor
const GICD_CTLR: usize = 0x0000;
const GICD_IGROUPR: usize = 0x0080;
const GICD_ISENABLER: usize = 0x0100;
const GICD_ICENABLER: usize = 0x0180;
const GICD_IPRIORITYR: usize = 0x0400;
const GICD_IROUTER: usize = 0x6000;

// Redistributor, RD_base frame
const GICR_WAKER: usize = 0x0014;
// ...and its SGI_base frame, the second 64KB page of the redistributor
const SGI_BASE: usize = 0x10000;
const GICR_IGROUPR0: usize = SGI_BASE + 0x0080;
const GICR_ISENABLER0: usize = SGI_BASE + 0x0100;
const GICR_IPRIORITYR: usize = SGI_BASE + 0x0400;

/// Everything nk enables runs at one priority. Not 0: ICC_PMR_EL1 masks
/// interrupts at a priority *numerically greater or equal* to it, and a
/// priority of 0 cannot be masked by anything, which removes the ability to
/// have a critical section later.
const PRIORITY: u8 = 0xa0;

pub struct Gic {
    /// Kept for Stage 3: shared interrupts (INTID >= 32) are enabled through
    /// the distributor's own registers, not the redistributor's, and every
    /// virtio device will have one. Nothing at Stage 1 does.
    #[allow(dead_code)]
    gicd: usize,
    gicr: usize,
}

static mut GIC: Gic = Gic { gicd: 0, gicr: 0 };

use crate::mmio::{readl as r32, writeb, writel as w32};

/// # Safety
/// Called once, with the MMU on and both frames mapped as Device memory.
pub unsafe fn init(gicd: usize, gicr: usize) {
    (*(&raw mut GIC)) = Gic { gicd, gicr };

    // The distributor. ARE_NS must go on before Group 1 is enabled: with
    // affinity routing off, v3's routing registers are not the ones in use,
    // and enabling the group first latches the wrong configuration.
    let ctlr = r32(gicd + GICD_CTLR);
    w32(gicd + GICD_CTLR, ctlr | (1 << 4)); // ARE_NS
    w32(gicd + GICD_CTLR, r32(gicd + GICD_CTLR) | (1 << 1)); // EnableGrp1A

    // Wake this CPU's redistributor. It comes out of reset asleep, and an
    // asleep redistributor forwards nothing while reporting no error at all.
    let waker = r32(gicr + GICR_WAKER);
    w32(gicr + GICR_WAKER, waker & !(1 << 1)); // clear ProcessorSleep
    while r32(gicr + GICR_WAKER) & (1 << 2) != 0 {} // wait for ChildrenAsleep

    // The CPU interface is system registers in v3, and ICC_SRE_EL1.SRE is what
    // makes them exist. Written and read back: on a CPU where the bit is
    // hard-wired off, every ICC_* access after this traps instead, and the
    // fault points at the access rather than at the cause.
    let mut sre: u64;
    core::arch::asm!("mrs {}, ICC_SRE_EL1", out(reg) sre, options(nomem, nostack));
    core::arch::asm!("msr ICC_SRE_EL1, {}", "isb", in(reg) sre | 1, options(nostack));
    core::arch::asm!("mrs {}, ICC_SRE_EL1", out(reg) sre, options(nomem, nostack));
    assert!(sre & 1 == 1, "ICC_SRE_EL1.SRE would not set: no system-register CPU interface");

    core::arch::asm!(
        "msr ICC_PMR_EL1, {pmr}",       // unmask every priority below 0xff
        "msr ICC_BPR1_EL1, {bpr}",      // no preemption grouping
        "msr ICC_IGRPEN1_EL1, {en}",    // Group 1 interrupts on
        "isb",
        pmr = in(reg) 0xffu64,
        bpr = in(reg) 0u64,
        en  = in(reg) 1u64,
        options(nostack)
    );

    println!("  gic:    v3 up, dist {:#x} redist {:#x}", gicd, gicr);
}

/// Enable one private interrupt (an SGI or PPI, INTID < 32) on this CPU.
///
/// Only the private range: shared interrupts live in the distributor's own
/// registers, and nothing at Stage 1 has one. Stage 3's virtio devices will.
///
/// # Safety
/// `init` must have run.
pub unsafe fn enable_ppi(intid: u32) {
    assert!(intid < 32, "not a private interrupt: {intid}");
    let gicr = (*(&raw const GIC)).gicr;

    // Group 1. An interrupt left in Group 0 is a secure interrupt, and a
    // non-secure EL1 never sees it -- it simply does not arrive.
    w32(gicr + GICR_IGROUPR0, r32(gicr + GICR_IGROUPR0) | (1 << intid));

    // Priority is byte-addressed, one byte per INTID.
    writeb(gicr + GICR_IPRIORITYR + intid as usize, PRIORITY);

    w32(gicr + GICR_ISENABLER0, 1 << intid);
}

/// Enable a shared interrupt (INTID >= 32) and route it to this CPU.
///
/// Shared interrupts live in the *distributor*, not the redistributor: they
/// can be delivered to any CPU, so somebody has to say which. Every virtio
/// device has one; nothing at Stage 1 did, which is why this arrived late.
///
/// # Safety
/// `init` must have run.
pub unsafe fn enable_spi(intid: u32) {
    assert!((32..1020).contains(&intid), "not a shared interrupt: {intid}");
    let gicd = (*(&raw const GIC)).gicd;
    let i = intid as usize;

    // Group 1, or a non-secure EL1 never sees it.
    let reg = gicd + GICD_IGROUPR + (i / 32) * 4;
    w32(reg, r32(reg) | (1 << (i % 32)));

    writeb(gicd + GICD_IPRIORITYR + i, PRIORITY);

    // Route to this CPU by affinity. Mode bit 31 clear means "this specific
    // PE", not "any"; with one CPU running the distinction does not matter
    // yet, and getting it wrong later means an interrupt delivered to a core
    // that is parked in boot.s and will never acknowledge it.
    let mpidr: u64;
    core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack));
    let aff = mpidr & 0x00ff_ffff_ff00_ffff;
    core::ptr::write_volatile((gicd + GICD_IROUTER + i * 8) as *mut u64, aff);

    w32(gicd + GICD_ISENABLER + (i / 32) * 4, 1 << (i % 32));
}

/// Stop offering a shared interrupt, and start again.
///
/// A device interrupt is level-triggered: the line stays asserted until the
/// driver acknowledges it *at the device*, and only Linux's driver can do
/// that. nk cannot service it and must not simply return, because the GIC
/// will offer it again immediately and the machine does nothing else ever
/// again -- which is what happened, at several hundred thousand interrupts a
/// second, with the thread that would have told Linux starved by the very
/// interrupt it was trying to deliver.
///
/// So it is masked on arrival and unmasked once Linux has had it. This is
/// what Linux itself does for a threaded handler, for the same reason.
///
/// # Safety
/// `init` must have run.
pub unsafe fn mask_spi(intid: u32) {
    let gicd = (*(&raw const GIC)).gicd;
    let i = intid as usize;
    w32(gicd + GICD_ICENABLER + (i / 32) * 4, 1 << (i % 32));
}

/// # Safety
/// `init` must have run.
pub unsafe fn unmask_spi(intid: u32) {
    let gicd = (*(&raw const GIC)).gicd;
    let i = intid as usize;
    w32(gicd + GICD_ISENABLER + (i / 32) * 4, 1 << (i % 32));
}

/// Acknowledge the interrupt the CPU is being offered, and take ownership of
/// it. 1023 means spurious -- the interrupt went away before it was read.
pub fn ack() -> u32 {
    let iar: u64;
    unsafe { core::arch::asm!("mrs {}, ICC_IAR1_EL1", out(reg) iar, options(nomem, nostack)) };
    iar as u32 & 0xff_ffff
}

/// Signal completion. Until this happens the interrupt stays active and no
/// further one at the same or lower priority will be offered -- which presents
/// as the timer ticking exactly once.
pub fn eoi(intid: u32) {
    unsafe {
        core::arch::asm!("msr ICC_EOIR1_EL1, {}", in(reg) intid as u64, options(nomem, nostack))
    };
}
