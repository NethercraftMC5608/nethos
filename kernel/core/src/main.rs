//! nk -- the NETHOS kernel.
//!
//! Stage 0: reach Rust from the reset vector, own the exception table, and
//! say so on the serial port. Nothing more. See docs/KERNEL.md.

#![no_std]
#![no_main]

use core::arch::global_asm;

pub mod exceptions;
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

    // Not parsed yet -- Stage 1 does that. Reported now because a null or
    // obviously wrong pointer here means the boot protocol was not honoured,
    // and finding that out at Stage 1 would mean debugging the DTB parser for
    // a fault that happened before it ran.
    println!("  device tree at {:#018x}", dtb as usize);
    println!("  running at EL{}", current_el());
    println!("  console: PL011");
    println!();
    println!("Stage 0 reached. No MMU, no allocator, no scheduler yet.");

    halt();
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
