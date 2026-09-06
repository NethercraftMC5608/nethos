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

/// Bits per level of a 4KB-granule, 48-bit walk.
const L0_SHIFT: u64 = 39;
const L1_SHIFT: u64 = 30;
const L2_SHIFT: u64 = 21;
const BLOCK_2MB: u64 = 1 << L2_SHIFT;

/// A page-table entry. Only the bits nk sets are named.
mod pte {
    pub const VALID: u64 = 1 << 0;
    pub const TABLE: u64 = 1 << 1; // at L0-L2: 1 = next-level table, 0 = block
    pub const AF: u64 = 1 << 10; // access flag; a miss on this faults
    pub const SH_INNER: u64 = 3 << 8; // inner shareable
    pub const UXN: u64 = 1 << 54; // never executable at EL0
    pub const PXN: u64 = 1 << 53; // never executable at EL1

    /// AP[2:1] at bits 7:6. EL0 can only reach a page that says so; there is
    /// no separate user page table, only this bit.
    pub const AP_RW_ANY: u64 = 1 << 6; // read/write at EL1 and EL0
    pub const AP_RO_ANY: u64 = 3 << 6; // read-only at EL1 and EL0

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

/// Follow a table entry, creating the next-level table if it is not there.
///
/// Tables come from the frame allocator, which returns zeroed pages -- which
/// matters more here than anywhere else in the kernel: a non-zero entry in a
/// fresh page table is a valid mapping to an arbitrary address, and the fault
/// it eventually produces has no connection to the code that caused it.
///
/// # Safety
/// The MMU is on and the frame allocator is up.
unsafe fn next_table(entry: *mut u64) -> *mut u64 {
    if *entry & pte::VALID == 0 {
        let page = crate::frames::alloc().expect("out of memory for a page table");
        *entry = (page as u64) | pte::VALID | pte::TABLE;
        // The walk is done by hardware reading memory this CPU just wrote.
        // Without the barrier the walker may not see it.
        core::arch::asm!("dsb ishst", options(nostack));
    }
    (*entry & 0x0000_ffff_ffff_f000) as *mut u64
}

/// Map `size` bytes of normal memory at `va` onto `pa`, in 2MB blocks.
///
/// Called after boot, unlike `init`, so it allocates the tables it needs
/// rather than using the static ones -- and it is the first thing in nk to
/// map an address that is not simply itself. Linux's vmemmap is the reason:
/// `struct page` lives at an address computed from a formula, not one nk gets
/// to choose.
///
/// # Safety
/// `va`, `pa` and `size` are 2MB-aligned; the range is not already mapped.
pub unsafe fn map_normal(va: u64, pa: u64, size: u64) {
    assert!(va % BLOCK_2MB == 0 && pa % BLOCK_2MB == 0 && size % BLOCK_2MB == 0);
    assert!(va >> 48 == 0, "only TTBR0 addresses: {va:#x}");

    let l0 = &raw mut L0 as *mut u64;
    let mut off = 0;
    while off < size {
        let v = va + off;
        let l0e = l0.add(((v >> L0_SHIFT) & 511) as usize);
        let l1 = next_table(l0e);
        let l1e = l1.add(((v >> L1_SHIFT) & 511) as usize);
        // The static L1 uses 1GB blocks; a block entry here would be
        // overwritten by next_table into a table pointer, silently unmapping
        // a gigabyte. Nothing currently overlaps, and this says so out loud
        // rather than discovering it as a fault somewhere else entirely.
        assert!(
            *l1e & pte::VALID == 0 || *l1e & pte::TABLE != 0,
            "{v:#x} lands inside an existing 1GB block"
        );
        let l2 = next_table(l1e);
        let l2e = l2.add(((v >> L2_SHIFT) & 511) as usize);
        *l2e = (pa + off) | pte::VALID | pte::AF | pte::SH_INNER | pte::attr(ATTR_NORMAL)
            | pte::UXN
            | pte::PXN;
        off += BLOCK_2MB;
    }

    core::arch::asm!(
        "dsb ishst",
        "tlbi vmalle1",
        "dsb ish",
        "isb",
        options(nostack)
    );
}

const L3_SHIFT: u64 = 12;

/// A fresh address space for a process.
///
/// It starts as a copy of the kernel's top-level table, so that kernel code
/// and data stay mapped while the process runs -- and, more to the point,
/// while the kernel handles the process's exceptions. An exception from EL0
/// does not change TTBR0, so the very first instruction of the handler is
/// fetched through the *process's* tables; a table without the kernel in it
/// faults before anything can report why.
///
/// A kernel in TTBR1's half needs none of this, and that is the reason to
/// move there. Until then, every process carries a copy of the kernel's
/// mappings and `USER_BASE` sits in a top-level slot the kernel does not use,
/// so that adding to one address space cannot alter another.
pub fn new_address_space() -> u64 {
    let table = crate::frames::alloc().expect("no memory for an address space") as *mut u64;
    unsafe {
        core::ptr::copy_nonoverlapping(&raw const L0 as *const Table as *const u64, table, 512);
    }
    table as u64
}

/// Map user-accessible pages into `ttbr0`, in 4KB pages.
///
/// # Safety
/// `ttbr0` is a table from `new_address_space`; the range is not already
/// mapped and does not overlap the kernel's own top-level entries.
pub unsafe fn map_user(ttbr0: u64, va: u64, pa: u64, size: u64, exec: bool) {
    map_user_permissions(ttbr0, va, pa, size, exec, !exec);
}

/// Map an ELF segment with independent write and execute permissions.
/// # Safety
/// Same requirements as `map_user`; writable executable pages are forbidden.
pub unsafe fn map_user_permissions(ttbr0: u64, va: u64, pa: u64, size: u64, exec: bool, writable: bool) {
    assert!(!(exec && writable));
    let l0 = ttbr0 as *mut u64;
    let mut off = 0;
    while off < size {
        let v = va + off;
        let l1 = next_table(l0.add(((v >> L0_SHIFT) & 511) as usize));
        let l2 = next_table(l1.add(((v >> L1_SHIFT) & 511) as usize));
        let l3 = next_table(l2.add(((v >> L2_SHIFT) & 511) as usize));
        // At level 3 the TABLE bit does not mean "table" -- it is what makes
        // the descriptor a page rather than a reserved encoding. A level-3
        // entry without it is simply invalid, and the fault says nothing
        // about why.
        let perms = (if writable { pte::AP_RW_ANY } else { pte::AP_RO_ANY })
            | pte::PXN | if exec { 0 } else { pte::UXN };
        *l3.add(((v >> L3_SHIFT) & 511) as usize) = (pa + off)
            | pte::VALID
            | pte::TABLE
            | pte::AF
            | pte::SH_INNER
            | pte::attr(ATTR_NORMAL)
            | perms;
        off += 4096;
    }
    core::arch::asm!("dsb ishst", "tlbi vmalle1", "dsb ish", "isb", options(nostack));
}

/// Translate a user virtual address, exactly as EL0 would see it.
///
/// Through the hardware rather than by walking the tables in software. `AT
/// S1E0R` asks the MMU to perform the translation with EL0's permissions and
/// leaves the answer in PAR_EL1 -- so a page the kernel can reach but the
/// process cannot correctly fails here, which is the entire point of checking
/// a user pointer rather than dereferencing it. A software walk would have to
/// reimplement the permission rules to get that right.
///
/// This is the smallest honest `copy_from_user`. It is also slow: one
/// translation per byte in the caller above. Batching by page is the obvious
/// next step and needs no new mechanism.
pub fn user_to_phys(va: u64) -> Option<u64> {
    let par: u64;
    unsafe {
        core::arch::asm!(
            "at s1e0r, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack)
        );
    }
    // PAR_EL1.F: set means the translation faulted, and the rest of the
    // register is then a fault status rather than an address.
    if par & 1 != 0 {
        return None;
    }
    Some((par & 0x0000_ffff_ffff_f000) | (va & 0xfff))
}

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
