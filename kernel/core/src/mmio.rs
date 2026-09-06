//! Register accessors, in assembly, for a reason worth the whole file.
//!
//! `read_volatile`/`write_volatile` guarantee that an access happens, that it
//! happens once, and that it is not reordered with other volatile accesses.
//! They do **not** guarantee which instruction LLVM reaches for, and on
//! aarch64 that turns out to matter enormously.
//!
//! nk's GIC setup was three volatile 32-bit accesses to nearby registers.
//! LLVM folded the address arithmetic into an addressing mode and emitted:
//!
//! ```text
//!     ldr w13, [x10, #0x80]!
//! ```
//!
//! a load with **pre-index writeback**. The architecture defines ESR_EL1.ISV
//! as 0 for a data abort on any load or store with writeback: the syndrome
//! register has no way to describe "and also update the base register", so the
//! fault carries no instruction decode at all. A hypervisor trapping that
//! access is handed a fault it cannot emulate -- QEMU's HVF backend asserts
//! outright, and KVM is no better placed. On real hardware it works, which is
//! the worst possible failure mode: correct until the machine is virtualised.
//!
//! Linux has always used `asm volatile` for its `__raw_readl`/`__raw_writel`
//! rather than a volatile pointer, and this is why. So does nk.
//!
//! Deliberately no `nomem`: an MMIO read is not pure and an MMIO write is not
//! dead code, and telling LLVM otherwise invites it to remove either.

/// # Safety
/// `addr` must be a mapped Device-memory register of this width.
#[inline]
pub unsafe fn readl(addr: usize) -> u32 {
    let v: u32;
    core::arch::asm!("ldr {v:w}, [{a}]", v = out(reg) v, a = in(reg) addr, options(nostack, preserves_flags));
    v
}

/// # Safety
/// As `readl`.
#[inline]
pub unsafe fn writel(addr: usize, v: u32) {
    core::arch::asm!("str {v:w}, [{a}]", v = in(reg) v, a = in(reg) addr, options(nostack, preserves_flags));
}

/// # Safety
/// As `readl`. Byte access is legal on some registers and not others -- the
/// GIC's priority registers are byte-addressed by design, most are not.
#[inline]
pub unsafe fn writeb(addr: usize, v: u8) {
    core::arch::asm!("strb {v:w}, [{a}]", v = in(reg) v, a = in(reg) addr, options(nostack, preserves_flags));
}

/// # Safety
/// As `readl`.
#[inline]
pub unsafe fn readb(addr: usize) -> u8 {
    let v: u32;
    core::arch::asm!("ldrb {v:w}, [{a}]", v = out(reg) v, a = in(reg) addr, options(nostack, preserves_flags));
    v as u8
}
