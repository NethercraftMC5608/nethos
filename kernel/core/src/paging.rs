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

/// What is added to a physical address to reach it through TTBR1.
///
/// The whole point of the high half, and it costs one register. With T1SZ 16
/// the top translation regime covers addresses whose top sixteen bits are all
/// ones, and it translates bits [47:0] of them -- which for `PA +
/// 0xFFFF_0000_0000_0000` are exactly the physical address. So TTBR1 pointed
/// at nk's *existing* identity tables produces a complete high alias of the
/// kernel, with no second set of page tables and no extra memory.
///
/// That alias is what makes a per-process TTBR0 possible. An exception from
/// EL0 does not change TTBR0, so today every address space has to carry a
/// copy of the kernel's mappings or the handler faults before it can report
/// why. A kernel running from the high half needs none of that, and the low
/// half becomes the process's alone -- which is where real binaries are
/// linked.
pub const KERNEL_VA_BASE: u64 = 0xFFFF_0000_0000_0000;

/// The high alias of a physical address.
pub const fn to_high(pa: u64) -> u64 {
    pa | KERNEL_VA_BASE
}

/// A page-table entry. Only the bits nk sets are named.
mod pte {
    pub const VALID: u64 = 1 << 0;
    pub const TABLE: u64 = 1 << 1; // at L0-L2: 1 = next-level table, 0 = block
    pub const AF: u64 = 1 << 10; // access flag; a miss on this faults
    pub const SH_INNER: u64 = 3 << 8; // inner shareable
    pub const UXN: u64 = 1 << 54; // never executable at EL0
    pub const PXN: u64 = 1 << 53; // never executable at EL1

    /// Not global: this translation belongs to one ASID only.
    ///
    /// Every mapping nk made was global, which is the default and is wrong
    /// for user space: a global entry is valid in every address space, so two
    /// processes with pages at the same address share whichever the TLB saw
    /// first. It was invisible because nk flushed the whole TLB on every
    /// switch -- which is both slower and less correct than saying what is
    /// actually private.
    pub const NG: u64 = 1 << 11;

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

/// What nk maps as Device memory, rounded outwards to 2MB blocks.
///
/// From the machine rather than from a guess would be better, and the device
/// tree has the answer -- but paging runs before anything has parsed it, and
/// the console has to work before that. On QEMU's `virt` the GIC starts at
/// 0x8000000 and the last virtio transport ends below 0xa200000. A device
/// outside this range reads as all-ones and writes nowhere, which presents as
/// hardware that is not there.
const DEVICE_START: u64 = 0x0800_0000;
const DEVICE_END: u64 = 0x0a20_0000;

#[repr(align(4096))]
struct Table([u64; 512]);

// In BSS, so boot.s has already zeroed them -- which matters, because a
// non-zero entry here is a valid mapping to somewhere arbitrary.
static mut L0: Table = Table([0; 512]);
static mut L1: Table = Table([0; 512]);

/// The first gigabyte, in 2MB blocks instead of one 1GB block.
///
/// It was one block, covering 0-1GB as Device memory, and that is why user
/// space lived at 512GiB: the addresses a real binary is linked for --
/// 0x400000 and up -- were inside a block the kernel had claimed for
/// peripherals that are not there. On this machine the devices occupy
/// 0x8000000 to about 0xa004000 and nothing else below 1GB exists.
///
/// So only those are mapped, and the 128MB below them is free for a process.
static mut L2_LOW: Table = Table([0; 512]);

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
        "tlbi vmalle1is",
        "dsb ish",
        "isb",
        options(nostack)
    );
}

/// Where Linux's own virtual addresses live, when Linux is managing memory.
///
/// `arch/lkl` with `CONFIG_MMU` wants an address space to itself. Its
/// `__pa`/`__va` are the identity, its vmalloc arena is whatever
/// `VMALLOC_START..VMALLOC_END` says, and its linear map starts at
/// `CONFIG_LKL_MEMORY_START`. On every other LKL host that space is a Unix
/// process's and the low four gigabytes are free; on nk they are not, because
/// nk is *in* them -- its image, the devices and the identity map of RAM.
///
/// So Linux is given a window above all of that. Four gigabytes of virtual
/// address space starting at four, which is empty on this machine and well
/// under the 512GB a three-level page table can address -- `arch/lkl` uses
/// three levels and a virtual address that does not fit is one Linux indexes
/// its own tables with out of range.
pub const LINUX_VA_BASE: u64 = 4 << 30;
pub const LINUX_VA_SIZE: u64 = 4 << 30;

/// The physical memory Linux treats as its own, at a fixed address.
///
/// It has to be fixed, and it has to be the address `CONFIG_LKL_MEMORY_START`
/// names, because LKL's `__pa()` is the identity: a physical address *is* the
/// virtual address it was mapped at. On a host that is a Unix process nothing
/// notices, since nothing does real DMA. nk hands Linux real hardware, and a
/// device programmed with an address Linux invented reads memory that is not
/// there -- which presents as a driver probe that simply never returns.
///
/// So Linux's linear map is identity with real physical memory, the way a
/// linear map is on every real architecture. nk keeps this range out of the
/// frame allocator and hands it back from `shmem_init`; it is already mapped,
/// because nk identity maps RAM.
pub const LINUX_PHYS_BASE: u64 = 0x5000_0000;
pub const LINUX_PHYS_SIZE: u64 = 64 << 20;

/// Make the page tables for Linux's window exist, before any process does.
///
/// The tables below a level-1 entry are shared by pointer with every address
/// space nk creates, because `new_address_space` copies the entry and not the
/// subtree. So a mapping Linux makes later lands in every process's view --
/// but only if the level-1 entry was there to be copied. Creating them all up
/// front is what makes that true, and the alternative is a page that exists in
/// the kernel and in every process created after it and in none created
/// before.
///
/// # Safety
/// The frame allocator is up and no process exists yet.
pub unsafe fn reserve_linux_window() {
    let l0 = &raw mut L0 as *mut u64;
    let mut va = LINUX_VA_BASE;
    while va < LINUX_VA_BASE + LINUX_VA_SIZE {
        let l1 = next_table(l0.add(((va >> L0_SHIFT) & 511) as usize));
        let l1e = l1.add(((va >> L1_SHIFT) & 511) as usize);
        assert!(
            *l1e & pte::VALID == 0 || *l1e & pte::TABLE != 0,
            "Linux's window overlaps a block mapping at {va:#x}"
        );
        next_table(l1e);
        va += 1 << L1_SHIFT;
    }
    core::arch::asm!("dsb ishst", "isb", options(nostack));
}

/// Map physical pages into Linux's window.
///
/// EL1 only: this is Linux's memory, not a process's, and a process that
/// could read it could read the kernel. Executable because Linux maps its own
/// module text through the same call and has no way to say so -- `arch/lkl`'s
/// `mmap_pages_for_ptes` asks for read, write and execute together and leaves
/// a note saying it should not.
///
/// # Safety
/// `reserve_linux_window` has run and the range is inside it.
pub unsafe fn map_linux(va: u64, pa: u64, size: u64) -> bool {
    // The linear map needs no work: it is identity with physical memory and
    // nk already maps RAM that way. Saying so here rather than mapping it
    // again keeps one mapping of those pages rather than two that must agree.
    if va == pa && va >= LINUX_PHYS_BASE && va + size <= LINUX_PHYS_BASE + LINUX_PHYS_SIZE {
        return true;
    }
    if va < LINUX_VA_BASE || va + size > LINUX_VA_BASE + LINUX_VA_SIZE {
        return false;
    }
    let l0 = table_of(kernel_address_space()) as *mut u64;
    let mut off = 0;
    while off < size {
        let v = va + off;
        let l1 = next_table(l0.add(((v >> L0_SHIFT) & 511) as usize));
        let l2 = next_table(l1.add(((v >> L1_SHIFT) & 511) as usize));
        let l3 = next_table(l2.add(((v >> L2_SHIFT) & 511) as usize));
        *l3.add(((v >> L3_SHIFT) & 511) as usize) = (pa + off)
            | pte::VALID
            | pte::TABLE
            | pte::AF
            | pte::SH_INNER
            | pte::attr(ATTR_NORMAL)
            // No AP bits at all: AP[1] clear is EL1 only and AP[2] clear is
            // writable, which is exactly what Linux's own memory should be.
            | pte::UXN;
        off += 4096;
    }
    core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack));
    true
}

/// Take a range of Linux's window out of the tables. The pages themselves
/// belong to whoever allocated them; only the mapping goes.
///
/// # Safety
/// `reserve_linux_window` has run.
pub unsafe fn unmap_linux(va: u64, size: u64) {
    let l0 = table_of(kernel_address_space()) as *mut u64;
    let mut off = 0;
    while off < size {
        let v = va + off;
        off += 4096;
        let e0 = *l0.add(((v >> L0_SHIFT) & 511) as usize);
        if !is_table(e0) {
            continue;
        }
        let l1 = (e0 & ADDR) as *mut u64;
        let e1 = *l1.add(((v >> L1_SHIFT) & 511) as usize);
        if !is_table(e1) {
            continue;
        }
        let l2 = (e1 & ADDR) as *mut u64;
        let e2 = *l2.add(((v >> L2_SHIFT) & 511) as usize);
        if !is_table(e2) {
            continue;
        }
        let l3 = (e2 & ADDR) as *mut u64;
        *l3.add(((v >> L3_SHIFT) & 511) as usize) = 0;
    }
    core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack));
}

const L3_SHIFT: u64 = 12;

/// Mask for the output address in a descriptor or a TTBR value.
const ADDR: u64 = 0x0000_ffff_ffff_f000;

/// The table a TTBR value points at, without its ASID.
///
/// TTBR0_EL1 carries the ASID in bits [63:48], so the register value and the
/// table pointer are no longer the same number. Every walk goes through this
/// -- dereferencing a TTBR value directly reads memory at `asid << 48 |
/// table`, which is unmapped, and the fault names the address rather than the
/// mistake.
pub const fn table_of(ttbr: u64) -> u64 {
    ttbr & ADDR
}

/// A descriptor is a table when both low bits are set. A *block* has only
/// bit 0, and telling them apart matters wherever tables are walked or freed:
/// a block's output address is memory, and following it as a table reads a
/// gigabyte of RAM as page-table entries.
const fn is_table(entry: u64) -> bool {
    entry & 3 == 3
}

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
/// Address-space identifiers, so the hardware can tell one process's
/// translations from another's.
///
/// nk used ASID 0 everywhere and flushed the entire TLB on every switch. That
/// is correct and expensive, and it hides mistakes: with distinct ASIDs a
/// stale entry from another address space cannot be used at all, rather than
/// being avoided by a flush somebody has to remember.
static mut NEXT_ASID: u64 = 1;

fn next_asid() -> u64 {
    unsafe {
        // 8-bit ASIDs are the minimum the architecture guarantees. Wrapping
        // reuses one, which is why the switch still flushes -- a real
        // implementation tracks generations and flushes only on rollover.
        let a = NEXT_ASID;
        NEXT_ASID = if NEXT_ASID >= 255 { 1 } else { NEXT_ASID + 1 };
        a
    }
}

pub fn new_address_space() -> u64 {
    unsafe {
        let l0 = crate::frames::alloc().expect("no memory for an address space") as *mut u64;
        let l1 = crate::frames::alloc().expect("no memory for an address space") as *mut u64;
        let l2 = crate::frames::alloc().expect("no memory for an address space") as *mut u64;

        // Three levels copied, not one, and the reason is that user space now
        // lives in the *same* gigabyte as the devices. A shallow copy of L0
        // shares the kernel's L1 and its low L2, so mapping a process at
        // 0x400000 would map it into every address space at once.
        //
        // The kernel's own mappings are still present in every process,
        // because an exception from EL0 does not change TTBR0 and the handler
        // is fetched through whatever is installed. A kernel in TTBR1's half
        // would need none of this; TTBR1 is enabled now and the move is the
        // next structural change.
        core::ptr::copy_nonoverlapping(&raw const L0 as *const Table as *const u64, l0, 512);
        core::ptr::copy_nonoverlapping(&raw const L1 as *const Table as *const u64, l1, 512);
        core::ptr::copy_nonoverlapping(&raw const L2_LOW as *const Table as *const u64, l2, 512);
        *l0 = (l1 as u64) | pte::VALID | pte::TABLE;
        *l1 = (l2 as u64) | pte::VALID | pte::TABLE;
        // TTBR0_EL1[63:48] is the ASID; TCR_EL1.A1 is 0, so TTBR0 is what
        // defines it.
        (l0 as u64) | (next_asid() << 48)
    }
}

/// Map user-accessible pages into `ttbr0`, in 4KB pages.
///
/// Executable pages arrive dirty: the bytes were just copied (ELF load) or
/// are about to be written (JIT, handler trampolines delivered by copy).
/// Caches are not coherent on this machine, so every executable mapping
/// cleans the data cache to the point of unification and invalidates the
/// instruction cache before the TLB flush makes it visible. `publish_code`
/// in user.rs does the same for single pages; this covers the mapping path
/// itself, which is where an EL0 handler's first instruction fetch faults
/// as SIGILL when the icache still holds whatever the frame held before.
pub unsafe fn map_user(ttbr0: u64, va: u64, pa: u64, size: u64, exec: bool) {
    if exec {
        clean_icache_range(pa, size);
    }
    map_user_permissions(ttbr0, va, pa, size, exec, !exec);
}

/// Clean one range to the point of unification and invalidate the icache.
///
/// The caller guarantees `pa..pa+size` is normal cacheable memory it owns.
/// Device memory must never come here: `dc cvau` on Device-nGnRnE faults.
pub unsafe fn clean_icache_range(pa: u64, size: u64) {
    let ctr: u64;
    core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nostack, nomem));
    let line = 4usize << ((ctr >> 16) & 15);
    let start = (pa as usize) & !(line - 1);
    let end = pa as usize + size as usize;
    let mut addr = start;
    while addr < end {
        core::arch::asm!("dc cvau, {}", in(reg) addr, options(nostack));
        addr += line;
    }
    core::arch::asm!("dsb ish", "ic iallu", "dsb ish", "isb", options(nostack));
}

/// Map an ELF segment with independent write and execute permissions.
///
/// Executable segments are cleaned to the point of unification here, for
/// the same reason as `map_user`: the loader just copied the bytes, and
/// the icache does not know that.
/// # Safety
/// Same requirements as `map_user`; writable executable pages are forbidden.
pub unsafe fn map_user_permissions(ttbr0: u64, va: u64, pa: u64, size: u64, exec: bool, writable: bool) {
    assert!(!(exec && writable));
    if exec {
        clean_icache_range(pa, size);
    }
    let l0 = table_of(ttbr0) as *mut u64;
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
            | pte::PXN
            | pte::NG
            | if exec { 0 } else { pte::UXN };
        *l3.add(((v >> L3_SHIFT) & 511) as usize) = (pa + off)
            | pte::VALID
            | pte::TABLE
            | pte::AF
            | pte::SH_INNER
            | pte::attr(ATTR_NORMAL)
            | perms;
        off += 4096;
    }
    core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack));
}

/// Remove a user mapping and free the pages behind it.
///
/// Returns how many pages it actually freed. Unmapping something that was
/// never mapped is not an error -- `munmap` over a hole is legal and common,
/// and a process that frees its own memory twice should not take the kernel
/// with it.
///
/// The page tables themselves are left in place. They are freed wholesale
/// when the address space is destroyed, and keeping an empty level-3 table
/// costs one page against the alternative of reference-counting three levels
/// on every unmap.
///
/// # Safety
/// `ttbr0` must be a user address space, and `va..va+size` must lie inside
/// the part of it the process owns -- never over the kernel's copied tables.
pub unsafe fn unmap_user(ttbr0: u64, va: u64, size: u64) -> usize {
    let l0 = table_of(ttbr0) as *mut u64;
    let mut freed = 0;
    let mut off = 0;
    while off < size {
        let v = va + off;
        off += 4096;
        // Walk without creating: a hole stays a hole.
        let e0 = *l0.add(((v >> L0_SHIFT) & 511) as usize);
        if !is_table(e0) {
            continue;
        }
        let l1 = (e0 & ADDR) as *mut u64;
        let e1 = *l1.add(((v >> L1_SHIFT) & 511) as usize);
        if !is_table(e1) {
            continue;
        }
        let l2 = (e1 & ADDR) as *mut u64;
        let e2 = *l2.add(((v >> L2_SHIFT) & 511) as usize);
        if !is_table(e2) {
            continue;
        }
        let l3 = (e2 & ADDR) as *mut u64;
        let slot = l3.add(((v >> L3_SHIFT) & 511) as usize);
        let e3 = *slot;
        if e3 & pte::VALID == 0 {
            continue;
        }
        *slot = 0;
        crate::frames::free((e3 & ADDR) as *mut u8);
        freed += 1;
    }
    core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack));
    freed
}

/// Remove a user mapping without freeing the pages behind it.
///
/// The shared pool's other half: `unmap_user` frees, which would hand pool
/// pages back to the allocator while other holders still map them. This
/// clears the entries and flushes the TLB, and the pool frees when its
/// reference count reaches zero. Unmapping a hole is still legal.
///
/// # Safety
/// Same requirements as `unmap_user`.
pub unsafe fn unmap_user_nofree(ttbr0: u64, va: u64, size: u64) {
    let l0 = table_of(ttbr0) as *mut u64;
    let mut off = 0;
    while off < size {
        let v = va + off;
        off += 4096;
        let e0 = *l0.add(((v >> L0_SHIFT) & 511) as usize);
        if !is_table(e0) {
            continue;
        }
        let l1 = (e0 & ADDR) as *mut u64;
        let e1 = *l1.add(((v >> L1_SHIFT) & 511) as usize);
        if !is_table(e1) {
            continue;
        }
        let l2 = (e1 & ADDR) as *mut u64;
        let e2 = *l2.add(((v >> L2_SHIFT) & 511) as usize);
        if !is_table(e2) {
            continue;
        }
        let l3 = (e2 & ADDR) as *mut u64;
        let slot = l3.add(((v >> L3_SHIFT) & 511) as usize);
        if *slot & pte::VALID == 0 {
            continue;
        }
        *slot = 0;
    }
    core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack));
}

/// Copy an address space: every page the process owns, with the permissions
/// it had.
///
/// `fork` is the only caller and it wants the plainest possible thing: real
/// memory, copied. Copy-on-write is the obvious improvement and it needs a
/// fault handler that can tell a write to a shared page from a wild pointer,
/// which nk does not have yet -- and getting that wrong turns a bug in one
/// process into silent corruption in another.
///
/// Only the process's own pages are copied. The first gigabyte is shared with
/// the devices, and those level-2 entries are *blocks*, not tables; following
/// one as though it were a table is what turns a fork into freeing the
/// kernel's memory a page-table entry at a time.
///
/// # Safety
/// `parent` must be a user address space built by `new_address_space`.
pub unsafe fn copy_user_address_space(parent: u64) -> Option<u64> {
    let child = new_address_space();
    let pl0 = table_of(parent) as *const u64;
    let e0 = *pl0;
    if !is_table(e0) {
        return Some(child);
    }
    let pl1 = (e0 & ADDR) as *const u64;
    let e1 = *pl1;
    if !is_table(e1) {
        return Some(child);
    }
    let pl2 = (e1 & ADDR) as *const u64;
    for i in 0..512 {
        let e2 = *pl2.add(i);
        if !is_table(e2) {
            continue; // a block: the shared device mapping
        }
        let pl3 = (e2 & ADDR) as *const u64;
        for j in 0..512 {
            let e3 = *pl3.add(j);
            if e3 & pte::VALID == 0 {
                continue;
            }
            let va = ((i as u64) << L2_SHIFT) | ((j as u64) << L3_SHIFT);
            let Some(page) = crate::frames::alloc() else {
                destroy_user_address_space(child);
                return None;
            };
            core::ptr::copy_nonoverlapping((e3 & ADDR) as *const u8, page, 4096);
            // The permissions the parent had, not a guess: an executable page
            // stays executable and a read-only one stays read-only, which is
            // what makes the child's RELRO and its text the same as its
            // parent's.
            let exec = e3 & pte::UXN == 0;
            // AP_RO_ANY is *both* AP bits, and AP_RW_ANY is one of them, so
            // "read-only" is the pair being present rather than the field
            // being non-zero -- a writable page has bit 6 set too.
            let writable = e3 & (1 << 7) == 0;
            map_user_permissions(child, va, page as u64, 4096, exec, writable);
            if e3 & (1 << 6) == 0 { protect_user_none(child, va, 4096); }
        }
    }
    Some(child)
}

/// Change the permissions of an existing user mapping.
///
/// Returns false if any page in the range is not mapped -- `mprotect` over a
/// hole is an error, unlike `munmap`, and a partial change would leave the
/// process with a range whose permissions differ half way through.
///
/// # Safety
/// `ttbr0` must be a user address space and the range must lie in the part of
/// it the process owns.
pub unsafe fn protect_user(ttbr0: u64, va: u64, size: u64, exec: bool, writable: bool) -> bool {
    if exec && writable {
        return false;
    }
    let l0 = table_of(ttbr0) as *mut u64;
    // Two passes: nothing is changed until every page is known to be there.
    for pass in 0..2 {
        let mut off = 0;
        while off < size {
            let v = va + off;
            off += 4096;
            let e0 = *l0.add(((v >> L0_SHIFT) & 511) as usize);
            if !is_table(e0) {
                return false;
            }
            let l1 = (e0 & ADDR) as *mut u64;
            let e1 = *l1.add(((v >> L1_SHIFT) & 511) as usize);
            if !is_table(e1) {
                return false;
            }
            let l2 = (e1 & ADDR) as *mut u64;
            let e2 = *l2.add(((v >> L2_SHIFT) & 511) as usize);
            if !is_table(e2) {
                return false;
            }
            let l3 = (e2 & ADDR) as *mut u64;
            let slot = l3.add(((v >> L3_SHIFT) & 511) as usize);
            if *slot & pte::VALID == 0 {
                return false;
            }
            if pass == 1 {
                let keep = *slot & !(pte::AP_RW_ANY | pte::AP_RO_ANY | pte::UXN);
                *slot = keep
                    | if writable { pte::AP_RW_ANY } else { pte::AP_RO_ANY }
                    | if exec { 0 } else { pte::UXN };
            }
        }
    }
    core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack));
    true
}

/// PROBE: walk a table by hand and print every descriptor.
pub fn dump_walk(ttbr0: u64, va: u64) {
    unsafe {
        let l0 = table_of(ttbr0) as *const u64;
        let e0 = *l0.add(((va >> L0_SHIFT) & 511) as usize);
        crate::println!("    L0[{}] = {:#018x}", (va >> L0_SHIFT) & 511, e0);
        if e0 & pte::VALID == 0 { return; }
        let l1 = (e0 & 0x0000_ffff_ffff_f000) as *const u64;
        let e1 = *l1.add(((va >> L1_SHIFT) & 511) as usize);
        crate::println!("    L1[{}] = {:#018x}", (va >> L1_SHIFT) & 511, e1);
        if e1 & pte::VALID == 0 || e1 & pte::TABLE == 0 { return; }
        let l2 = (e1 & 0x0000_ffff_ffff_f000) as *const u64;
        let e2 = *l2.add(((va >> L2_SHIFT) & 511) as usize);
        crate::println!("    L2[{}] = {:#018x}", (va >> L2_SHIFT) & 511, e2);
        if e2 & pte::VALID == 0 || e2 & pte::TABLE == 0 { return; }
        let l3 = (e2 & 0x0000_ffff_ffff_f000) as *const u64;
        let e3 = *l3.add(((va >> L3_SHIFT) & 511) as usize);
        crate::println!("    L3[{}] = {:#018x}", (va >> L3_SHIFT) & 511, e3);
    }
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
/// As `user_to_phys`, but asks whether EL0 may *write* there.
///
/// A separate instruction, not a flag: `AT S1E0R` and `AT S1E0W` ask
/// different questions, and a read-only user page answers yes to the first
/// and no to the second. Copying a syscall's results back through the read
/// check would let a process ask the kernel to write into its own text.
pub fn user_to_phys_write(va: u64) -> Option<u64> {
    let par: u64;
    unsafe {
        core::arch::asm!(
            "at s1e0w, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack)
        );
    }
    if par & 1 != 0 {
        return None;
    }
    Some((par & 0x0000_ffff_ffff_f000) | (va & 0xfff))
}

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

    // The first gigabyte, through a table of 2MB blocks rather than one
    // block, so that only the addresses devices actually occupy are taken.
    // Never executable: nothing should branch into a register window, and if
    // it does the fault should say so.
    let l2_low = &raw mut L2_LOW as *mut Table as u64;
    (*(&raw mut L1)).0[0] = l2_low | pte::VALID | pte::TABLE;
    let mut dev = DEVICE_START;
    while dev < DEVICE_END {
        (*(&raw mut L2_LOW)).0[(dev >> L2_SHIFT) as usize & 511] =
            dev | pte::VALID | pte::AF | pte::attr(ATTR_DEVICE) | pte::UXN | pte::PXN;
        dev += BLOCK_2MB;
    }

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
        "  mmu:    identity, devices {:#x}..{:#x}, {} GB of RAM, {} MiB free below them",
        DEVICE_START,
        DEVICE_END,
        last - first,
        DEVICE_START / (1024 * 1024)
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
        // TTBR1, over the same tables. See KERNEL_VA_BASE.
        | (16 << 16)           // T1SZ  = 48-bit
        | (1 << 24)            // IRGN1 = WBWA
        | (1 << 26)            // ORGN1 = WBWA
        | (3 << 28)            // SH1   = inner shareable
        // TG1 is not TG0: 4KB is 0b10 here and 0b00 there. Encoding one as
        // the other gives a granule the tables were not built for, and the
        // fault is a translation fault on an address that is plainly mapped.
        | (2 << 30); // TG1 = 4KB

    core::arch::asm!(
        "msr mair_el1, {mair}",
        "msr tcr_el1,  {tcr}",
        "msr ttbr0_el1,{ttbr}",
        "msr ttbr1_el1,{ttbr}",
        // The tables were just written with the MMU off, so they went straight
        // to memory; the TLB and the instruction cache, however, may hold
        // stale entries from before. Clear both before anything can use them.
        "tlbi vmalle1is",
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
    // UCT and UCI: let EL0 read CTR_EL0 and run the cache maintenance
    // instructions. Both are trapped by default, and the trap looks nothing
    // like what it is -- an ESR with EC 0x18 and a FAR of zero, which reads
    // as a null dereference rather than a system register nobody may touch.
    //
    // They are not optional for real programs. A libc reads CTR_EL0 to learn
    // its cache line size, and anything that generates code -- LLVM's JIT
    // inside Mesa, most obviously -- must clean it to the point of unification
    // with DC CVAU and IC IVAU before it can jump to it. Linux sets both for
    // exactly these reasons.
    sctlr |= (1 << 15) | (1 << 26);

    // CNTKCTL_EL1: let EL0 read the counters.
    //
    // The same shape of trap as UCT above and it arrives just as
    // misleadingly: an ESR with EC 0x18 and a FAR of zero, which reads like a
    // null dereference until the ISS is decoded and names a register. WebKit
    // was the first thing to hit it -- `MRS x2, CNTVCT_EL0`, trapped at its
    // reset value because nk had never written this register at all.
    //
    // EL0VCTEN (bit 1) is the one that matters: the virtual counter is what a
    // libc's clock_gettime reads without a syscall, and on Linux the vDSO
    // makes that the normal path, so anything with a fast clock finds it.
    // EL0PCTEN (bit 0) comes along because CNTFRQ_EL0 -- how a program learns
    // the counter's frequency, and useless without it -- is readable at EL0
    // only when one of the two is set.
    //
    // The timer controls (bits 8 and 9) stay trapped. Reading a counter is
    // harmless; programming a timer from EL0 is not something a process
    // should be doing behind the kernel's back, and leaving it trapped is the
    // honest default rather than an oversight.
    let cntkctl: u64 = (1 << 0) | (1 << 1);
    core::arch::asm!("msr cntkctl_el1, {}", in(reg) cntkctl, options(nomem, nostack));

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

    // Prove the high alias rather than assume it. The kernel image starts
    // with the arm64 boot header, whose magic is at offset 56; reading it
    // through TTBR1 has to give the same word as reading it directly.
    let low = 0x4008_0000u64 + 56;
    let a = core::ptr::read_volatile(low as *const u32);
    let b = core::ptr::read_volatile(to_high(low) as *const u32);
    assert_eq!(a, b, "TTBR1 does not alias the kernel");
    println!("          ttbr1 on: {:#x} aliases {:#x} (magic {:#x})", to_high(low), low, b);
}

/// Kernel threads always use the kernel root, never a process's mappings.
pub fn kernel_address_space() -> u64 { &raw const L0 as u64 }

/// Reclaim the private user subtree, then the copied root. The remaining
/// root entries belong to the kernel and must not be freed.
/// # Safety
/// This is a finished, single-threaded process's inactive address space,
/// created by new_address_space/map_user in USER_BASE's top-level slot only.
pub unsafe fn destroy_user_address_space(root: u64) {
    // Only what this address space owns.
    //
    // Its L0, L1 and low L2 are its own copies, and the L3s hanging off that
    // L2 are its own. Everything else in L0 and L1 is a copy of the kernel's
    // and is shared with every other process -- including a one-gigabyte RAM
    // *block* at L1[1], which the previous version of this function would
    // have followed as though it were a table and freed a gigabyte of the
    // kernel's memory one page-table entry at a time.
    //
    // That was safe only because user space used to live under its own
    // top-level entry, where nothing was shared. It stopped being safe the
    // moment processes moved into the low half.
    let l0 = table_of(root) as *mut u64;
    assert_ne!(table_of(root), table_of(kernel_address_space()));

    let e0 = *l0;
    if is_table(e0) {
        let l1 = (e0 & ADDR) as *mut u64;
        let e1 = *l1;
        if is_table(e1) {
            let l2 = (e1 & ADDR) as *mut u64;
            for i in 0..512 {
                let e2 = *l2.add(i);
                // Tables are this process's leaf levels. Blocks are the
                // copied device mappings and belong to everyone.
                if !is_table(e2) {
                    continue;
                }
                let l3 = (e2 & ADDR) as *mut u64;
                for j in 0..512 {
                    let e3 = *l3.add(j);
                    if e3 & 1 != 0 {
                        crate::frames::free((e3 & ADDR) as *mut u8);
                    }
                }
                crate::frames::free(l3 as *mut u8);
            }
            crate::frames::free(l2 as *mut u8);
        }
        crate::frames::free(l1 as *mut u8);
    }
    crate::frames::free(l0 as *mut u8);
}

/// Retain backing pages while removing all EL0 access (PROT_NONE).
/// # Safety
/// Same address-space ownership requirements as protect_user.
pub unsafe fn protect_user_none(root: u64, va: u64, size: u64) -> bool {
    if !protect_user(root,va,size,false,false) { return false; }
    for v in (va..va+size).step_by(4096) {
        let mut table = table_of(root) as *mut u64;
        for shift in [39,30,21] { table = (*table.add(((v>>shift)&511) as usize) & ADDR) as *mut u64; }
        *table.add(((v>>12)&511) as usize) &= !(1<<6);
    }
    core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack));
    true
}
