//! nk -- the NETHOS kernel.
//!
//! Stage 0: reach Rust from the reset vector, own the exception table, and
//! say so on the serial port. Nothing more. See docs/KERNEL.md.

#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;

pub mod dt;
pub mod exceptions;
pub mod frames;
pub mod gic;
pub mod heap;
pub mod mmio;
pub mod paging;
pub mod sched;
pub mod selftest;
pub mod timer;
pub mod uart;

// The assembly lives in a real .s file rather than inline in a string, so it
// can be read and diffed as assembly. global_asm! means rustc's own assembler
// handles it and no separate binutils cross-toolchain is needed to build the
// kernel at all -- rustup and nothing else.
global_asm!(include_str!("boot.s"));

/// Where boot.s hands over. `dtb` is whatever x0 held at reset: on QEMU's
/// `virt` that is the physical address of the flattened device tree, which is
/// the whole of what this kernel is told about the machine.
#[no_mangle]
pub extern "C" fn rust_main(dtb: *const u8) -> ! {
    println!();
    println!("NETHOS kernel (nk) {} -- aarch64", env!("CARGO_PKG_VERSION"));

    println!("  running at EL{}", current_el());

    // Everything nk knows about the machine comes through here. A null or
    // unparseable pointer is fatal on purpose: guessing at hardware addresses
    // is how a kernel ends up working on exactly one emulator.
    let Some(fdt) = (unsafe { dt::Fdt::from_ptr(dtb) }) else {
        panic!("no usable device tree at {:#018x}", dtb as usize);
    };
    println!("  device tree at {:#018x}, {} bytes", fdt.base(), fdt.total_size());

    let (ac, sc) = fdt.root_cells();
    println!("  #address-cells {ac}  #size-cells {sc}");

    if let Some(root) = fdt.find_by_prefix("") {
        if let Some(m) = root.prop("model") {
            println!("  model: {}", core::str::from_utf8(&m[..m.len().saturating_sub(1)]).unwrap_or("?"));
        }
    }
    if let Some((base, size)) = fdt.find_by_prefix("memory@").and_then(|n| n.reg(0)) {
        println!("  memory: {:#x}..{:#x} ({} MiB)", base, base + size, size >> 20);
    }
    if let Some((base, _)) = fdt.find_compatible("arm,pl011").and_then(|n| n.reg(0)) {
        println!("  pl011:  {:#x}", base);
    }
    if let Some(gic) = fdt.find_compatible("arm,gic-v3") {
        let d = gic.reg(0).map(|r| r.0).unwrap_or(0);
        let r = gic.reg(1).map(|r| r.0).unwrap_or(0);
        println!("  gicv3:  dist {:#x}  redist {:#x}", d, r);
    }
    if let Some((_, num, _)) = fdt.find_compatible("arm,armv8-timer").and_then(|n| n.interrupt(2)) {
        // The four entries are secure physical, non-secure physical, virtual,
        // hypervisor -- in that order. Index 2, the virtual timer, is the one
        // nk uses; see the note at the top of timer.rs for why the physical
        // one at index 1 is the trap it looks like the right answer.
        println!("  timer:  virtual, PPI INTID {}", num + 16);
    }
    let mut virtio = 0;
    fdt.each_compatible("virtio,mmio", |_| virtio += 1);
    println!("  virtio-mmio transports: {}", virtio);

    let (ram_base, ram_size) = fdt
        .find_by_prefix("memory@")
        .and_then(|n| n.reg(0))
        .expect("device tree has no memory node");
    unsafe { paging::init(ram_base, ram_size) };

    unsafe { claim_memory(&fdt, ram_base as usize, (ram_base + ram_size) as usize) };
    frames::report();
    heap::init(16);
    selftest::run();

    let gic = fdt.find_compatible("arm,gic-v3").expect("no GICv3 in the device tree");
    let gicd = gic.reg(0).expect("GIC has no distributor reg").0 as usize;
    let gicr = gic.reg(1).expect("GIC has no redistributor reg").0 as usize;
    unsafe { gic::init(gicd, gicr) };

    let (_, ppi, _) = fdt
        .find_compatible("arm,armv8-timer")
        .and_then(|n| n.interrupt(2))
        .expect("no virtual timer in the device tree");
    unsafe { timer::init(ppi + 16) };

    sched::init();
    sched::spawn("ping", worker, 0);
    sched::spawn("pong", worker, 1);

    println!();
    println!("Stage 1 up. Unmasking interrupts; two threads should now alternate.");
    println!();

    sched::enable();
    unsafe { core::arch::asm!("msr daifclr, #0xf", options(nomem, nostack)) };

    // The boot thread becomes the idle task. wfi rather than a spin, so an
    // idle machine is genuinely idle: under HVF a busy loop here pins a whole
    // host core for as long as the kernel is running.
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

/// Two of these run, to show that preemption works and that each one keeps its
/// own stack and registers across a switch it never asked for.
extern "C" fn worker(id: usize) {
    let names = ["ping", "pong"];
    let mut n = 0u64;
    loop {
        // A local that must survive being preempted: if the context switch
        // loses a callee-saved register or lands on the wrong stack, this is
        // where it shows, as a counter that jumps or resets.
        n += 1;
        println!("[{:>5}ms] {} #{}", timer::ms(), names[id], n);
        if n == 5 {
            println!();
            sched::report();
            println!();
        }
        if n >= 8 {
            println!("{} done", names[id]);
            return;
        }
        // Spin out the slice rather than sleeping: there is no sleep yet, and
        // the point is to be interrupted involuntarily rather than to yield.
        let until = timer::ticks() + 20;
        while timer::ticks() < until {
            core::hint::spin_loop();
        }
    }
}

/// Hand every page of RAM to the frame allocator except the ones already
/// spoken for: the kernel image itself, and the device tree, which stays
/// mapped for as long as anything might want to re-read it.
///
/// The reserved ranges are sorted rather than assumed to be in any order.
/// QEMU happens to put the DTB above the kernel; U-Boot does not always, and
/// a boot that hands out the pages the kernel is executing from fails in a
/// way that has no useful symptom at all.
unsafe fn claim_memory(fdt: &dt::Fdt, ram_start: usize, ram_end: usize) {
    extern "C" {
        static __image_end: u8;
    }
    let image_start = 0x4008_0000usize; // where linker.ld links; see boot.s
    let image_end = &raw const __image_end as usize;

    let mut reserved = [
        (image_start, image_end),
        (fdt.base(), fdt.base() + fdt.total_size()),
    ];
    reserved.sort_unstable();

    let mut at = ram_start;
    for (rs, re) in reserved {
        if rs > at {
            frames::add(at, rs.min(ram_end));
        }
        at = at.max(re);
    }
    if at < ram_end {
        frames::add(at, ram_end);
    }
}

fn current_el() -> u64 {
    let el: u64;
    unsafe { core::arch::asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack)) };
    el >> 2
}

/// Mask interrupts and stop. `wfi` rather than a spin so an emulated CPU is
/// actually idle -- a busy loop here pins a host core for as long as the
/// window is open.
pub fn halt() -> ! {
    unsafe { core::arch::asm!("msr daifset, #0xf", options(nomem, nostack)) };
    loop {
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Deliberately uses the raw console rather than anything that could be
    // locked or allocated: the panic path has to work when the reason for the
    // panic is that one of those is broken.
    println!();
    println!("!! kernel panic");
    if let Some(loc) = info.location() {
        println!("   at {}:{}:{}", loc.file(), loc.line(), loc.column());
    }
    println!("   {}", info.message());
    halt();
}
