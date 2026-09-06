//! The MMU: identity mapping, so that memory behaves like memory.
//!
//! This is the reason Stage 1 exists at all. With the MMU off, every access on
//! aarch64 is Device-nGnRnE: no cache, no write buffering, no speculation, and
//! an alignment fault on any unaligned load or store. Linux drivers need
//! ordinary cacheable memory for their data structures and genuine Device
//! memory for their registers, and neither exists until this runs.
//!
//! **Identity mapped, deliberately.** Virtual equals physical everywhere.
//! A higher-half kernel is the eventual right answer -- it is what lets user
//! space have the low addresses -- but nk is linked at a fixed physical
//! address and moving it costs a linker script, a PIE-or-relocation decision
//! and a switch of PC mid-flight. None of that buys anything at Stage 1, and
//! all of it can go wrong silently. So: identity, and say so.
//!
//! 1GB blocks at level 1. The whole map is two 4KB tables, and both are static
//! because the frame allocator does not exist yet -- and could not, since it
//! wants normal cacheable memory to be useful, which is what this provides.

use crate::println;

/// A page-table entry. Only the bits nk sets are named.
mod pte {
    pub const VALID: u64 = 1 << 0;
    pub const TABLE: u64 = 1 << 1; // at L0-L2: 1 = next-level table, 0 = block
    pub const AF: u64 = 1 << 10; // access flag; a miss on this faults
    pub const SH_INNER: u64 = 3 << 8; // inner shareable
    pub const UXN: u64 = 1 << 54; // never executable at EL0
    pub const PXN: u64 = 1 << 53; // never executable at EL1

    /// AttrIndx, an index into MAIR_EL1 rather than a description of the
    /// memory -- the attributes themselves live in that register.
    pub const fn attr(idx: u64) -> u64 {
        idx << 2
    }
}

/// MAIR_EL1 slot 0: Device-nGnRnE. Registers. No caching, no reordering, no
/// merging, no early write acknowledgement.
const MAIR_DEVICE: u64 = 0x00;
/// MAIR_EL1 slot 1: Normal memory, write-back read/write-allocate, inner and
/// outer. Ordinary RAM.
const MAIR_NORMAL: u64 = 0xff;

const ATTR_DEVICE: u64 = 0;
const ATTR_NORMAL: u64 = 1;

const GB: u64 = 1 << 30;

#[repr(align(4096))]
struct Table([u64; 512]);

// In BSS, so boot.s has already zeroed them -- which matters, because a
// non-zero entry here is a valid mapping to somewhere arbitrary.
static mut L0: Table = Table([0; 512]);
static mut L1: Table = Table([0; 512]);

/// Map the machine and switch the MMU on.
///
/// `ram_base`/`ram_size` come from the device tree. Everything below 1GB is
/// mapped as Device: on `virt` that covers the UART, the GIC, and all 32
/// virtio-mmio transports, and on any machine it is where the low peripherals
/// live. Mapping it as Normal instead is the classic way to get a driver that
/// works until the compiler decides to reorder two register writes.
///
/// # Safety
/// Called once, on the boot CPU, with the MMU off.
pub unsafe fn init(ram_base: u64, ram_size: u64) {
    let l1_ptr = &raw mut L1 as *mut Table as u64;

    // The low 512GB of the address space, which is all nk maps.
    (*(&raw mut L0)).0[0] = l1_ptr | pte::VALID | pte::TABLE;

    // [0, 1GB): peripherals. Never executable -- nothing should ever branch
    // into a register window, and if it does, the fault should say so.
    (*(&raw mut L1)).0[0] = pte::VALID
        | pte::AF
        | pte::attr(ATTR_DEVICE)
        | pte::UXN
        | pte::PXN;

    // RAM, in 1GB blocks, from the device tree rather than assumed. Rounded
    // up: a machine with 512MB still needs its whole block mapped, and the
    // address space above the RAM inside that block is simply never touched.
    let first = ram_base / GB;
    let last = (ram_base + ram_size).div_ceil(GB);
    for gb in first..last {
        (*(&raw mut L1)).0[gb as usize] =
            (gb * GB) | pte::VALID | pte::AF | pte::SH_INNER | pte::attr(ATTR_NORMAL);
    }
    println!(
        "  mmu:    identity, {} device GB + {} normal GB, 1GB blocks",
        1,
        last - first
    );

    let l0_ptr = &raw mut L0 as *mut Table as u64;

    // Physical address size. Hard-coding 48 bits would fault on a CPU that
    // implements fewer, and TCR_EL1.IPS is one of the fields where a value the
    // hardware does not support is simply ignored in a way that shows up much
    // later as a translation fault.
    let mmfr0: u64;
    core::arch::asm!("mrs {}, id_aa64mmfr0_el1", out(reg) mmfr0, options(nomem, nostack));
    let ips = mmfr0 & 0xf;

    let mair = MAIR_DEVICE | (MAIR_NORMAL << 8);

    // T0SZ 16 -> a 48-bit VA space in TTBR0. TG0 0 -> 4KB granule.
    // SH0 inner shareable, IRGN0/ORGN0 write-back write-allocate: these
    // describe how the *page-table walk itself* accesses memory, and leaving
    // them non-cacheable is a large, invisible cost on every miss.
    let tcr: u64 = 16          // T0SZ
        | (1 << 8)             // IRGN0 = WBWA
        | (1 << 10)            // ORGN0 = WBWA
        | (3 << 12)            // SH0   = inner shareable
        | (0 << 14)            // TG0   = 4KB
        | (ips << 32)          // IPS
        // TTBR1 is not set up, so disable walks through it outright. Left
        // enabled, any stray high address becomes a walk through a zero table
        // rather than an immediate, obvious fault.
        | (1 << 23); // EPD1

    core::arch::asm!(
        "msr mair_el1, {mair}",
        "msr tcr_el1,  {tcr}",
        "msr ttbr0_el1,{ttbr}",
        // The tables were just written with the MMU off, so they went straight
        // to memory; the TLB and the instruction cache, however, may hold
        // stale entries from before. Clear both before anything can use them.
        "tlbi vmalle1",
        "ic  iallu",
        "dsb nsh",
        "isb",
        mair = in(reg) mair,
        tcr  = in(reg) tcr,
        ttbr = in(reg) l0_ptr,
        options(nostack)
    );

    // M: translation on. C: data cacheable. I: instruction cacheable.
    // Read-modify-write rather than a constant, because SCTLR_EL1 has
    // reserved bits that must keep whatever the reset value gave them.
    let mut sctlr: u64;
    core::arch::asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack));
    sctlr |= (1 << 0) | (1 << 2) | (1 << 12);
    // A: strict alignment checking. Left off on purpose now that the MMU is
    // on -- normal memory permits unaligned access, and the Rust side is
    // still built with +strict-align, so this only removes a restriction.
    sctlr &= !(1 << 1);
    core::arch::asm!(
        "msr sctlr_el1, {}",
        // The instruction after this must be fetched through the MMU. Without
        // the barrier the CPU may still be running on the old translation
        // regime, and the failure is not a fault -- it is the next few
        // instructions quietly executing under the wrong rules.
        "isb",
        in(reg) sctlr,
        options(nostack)
    );

    // Read back rather than trust the write. A kernel that failed to enable
    // the MMU keeps running and printing exactly as though it had -- the
    // identity map means nothing observably changes until something needs a
    // cache or an unaligned access, and by then the cause is a long way back.
    let mut check: u64;
    core::arch::asm!("mrs {}, sctlr_el1", out(reg) check, options(nomem, nostack));
    println!(
        "          sctlr_el1 {:#x}  M={} C={} I={}",
        check,
        check & 1,
        (check >> 2) & 1,
        (check >> 12) & 1
    );
    assert!(check & 1 == 1, "the MMU did not come on");
}
