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
#[cfg(nk_linux)]
pub mod linux;
pub mod mmio;
pub mod paging;
pub mod psci;
pub mod sched;
pub mod selftest;
pub mod stub;
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

    psci::init(&fdt);

    println!();
    println!("Stage 1 up. Unmasking interrupts.");
    sched::enable();
    unsafe { core::arch::asm!("msr daifclr, #0xf", options(nomem, nostack)) };

    // Linux code runs from here on. Interrupts are already on and the
    // scheduler is running, because a driver's probe may sleep, wait on a
    // completion, or take a timeout -- all of which need a tick and something
    // else to run.
    #[cfg(nk_linux)]
    {
        map_vmemmap(ram_base, ram_size);
        linux::init();
        linux::probe(&fdt);

        // Probing is not synchronous from here: virtio_blk's own probe path
        // defers part of itself to a workqueue, which runs on the kworker
        // thread and needs the CPU. Yielding until the capacity arrives is
        // what waiting for a device to appear looks like with no completion
        // to wait on.
        // Yield a bounded number of times rather than waiting a number of
        // seconds.
        //
        // Probing is not synchronous: virtio_blk defers part of its own to a
        // workqueue, which runs on the kworker thread and needs the CPU. But
        // a *time* limit is the wrong shape here -- under TCG the virtual
        // timer counts guest cycles rather than following the host clock, so
        // a three-second deadline is around seven hundred real ones, and the
        // wait is indistinguishable from a hang. Yields are the thing
        // actually being waited for, so count those.
        for _ in 0..2000 {
            if linux::capacity() != 0 {
                break;
            }
            sched::yield_now();
        }

        println!();
        if linux::capacity() != 0 {
            println!(
                "Stage 3: virtio-blk is up -- {} sectors, {} MiB",
                linux::capacity(),
                linux::capacity() * 512 / (1024 * 1024)
            );
            read_a_sector();
            stop();
        }
        if let Some(mac) = linux::net_up() {
            arp_exchange(mac);
            stop();
        }
        println!("No device appeared.");
        stop();
    }

    // Without a Linux port linked in there are no drivers to exercise, so the
    // two demo threads run instead -- still the only thing that proves the
    // context switch, and what tests/test_kernel_boot.py checks.
    #[cfg(not(nk_linux))]
    {
        sched::spawn("ping", worker, 0);
        sched::spawn("pong", worker, 1);
        println!("Two threads should now alternate.");
        println!();
        let deadline = timer::ticks() + timer::HZ * 4;
        while timer::ticks() < deadline {
            sched::yield_now();
        }
        stop();
    }
}

/// Finish: say so, and ask the machine to switch itself off.
///
/// Powering down rather than spinning matters for the tests. A kernel killed
/// by a watchdog looks exactly like one that hung -- both end with a signal
/// after N seconds -- and that ambiguity cost real time on one silent hang.
fn stop() -> ! {
    println!();
    println!("nk: done.");
    psci::poweroff();
    halt()
}

/// Stage 4: send an Ethernet frame through the unmodified driver and read the
/// answer that comes back.
///
/// An ARP request, sent *by* nk rather than waiting for one. QEMU's user-mode
/// network only ARPs a guest when it has traffic for it, so waiting is a test
/// that depends on the host deciding to speak first. Asking for the gateway's
/// address exercises the same two paths -- transmit and receive -- and does
/// so on nk's own initiative.
#[cfg(nk_linux)]
fn arp_exchange(mac: [u8; 6]) {
    const OURS: [u8; 4] = [10, 0, 2, 15]; // what QEMU's DHCP would hand out
    const GATEWAY: [u8; 4] = [10, 0, 2, 2];

    println!(
        "Stage 4: virtio-net is up -- {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    let mut f = [0u8; 42];
    f[0..6].fill(0xff); // broadcast
    f[6..12].copy_from_slice(&mac);
    f[12..14].copy_from_slice(&0x0806u16.to_be_bytes()); // ARP
    f[14..16].copy_from_slice(&1u16.to_be_bytes()); // over Ethernet
    f[16..18].copy_from_slice(&0x0800u16.to_be_bytes()); // resolving IPv4
    f[18] = 6;
    f[19] = 4;
    f[20..22].copy_from_slice(&1u16.to_be_bytes()); // request
    f[22..28].copy_from_slice(&mac);
    f[28..32].copy_from_slice(&OURS);
    // target hardware address left zero: that is the question
    f[38..42].copy_from_slice(&GATEWAY);

    println!();
    println!("  who has 10.0.2.2? asking as 10.0.2.15");
    if let Err(e) = linux::net_xmit(&f) {
        println!("  transmit failed: {}", e);
        return;
    }

    let mut buf = [0u8; 1600];
    let deadline = timer::ticks() + timer::HZ * 3;
    while timer::ticks() < deadline {
        let n = linux::net_recv(&mut buf);
        if n == 0 {
            sched::yield_now();
            continue;
        }
        // The driver hands frames up with the Ethernet header still on.
        if n >= 42 && buf[12] == 0x08 && buf[13] == 0x06 && buf[21] == 2 {
            println!(
                "  reply: 10.0.2.2 is at {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                buf[22], buf[23], buf[24], buf[25], buf[26], buf[27]
            );
            println!();
            println!("  the frame, as the driver delivered it:");
            hexdump(&buf[..n.min(64)], 0);
            return;
        }
        println!("  {} bytes, not the ARP reply -- still waiting", n);
    }
    println!("  no reply in 3 seconds");
}

/// The whole point of Stage 3: ask the unmodified Linux driver for a sector
/// and look at what comes back.
#[cfg(nk_linux)]
fn read_a_sector() {
    // Prefilled rather than zeroed. A read that never reaches the buffer
    // leaves it exactly as it was, and zeroes would be indistinguishable from
    // a disk full of zeroes -- which is precisely the case that looked like a
    // working read for a while.
    let mut buf = alloc::vec![0xAAu8; 512];
    println!();
    println!("  reading sector 0 through the Linux driver...");
    match linux::read(0, &mut buf) {
        Ok(()) => {
            hexdump(&buf[..96], 0);
            // Printed as text as well: the disk built by the test carries a
            // readable marker, so a correct read is legible rather than
            // something to verify byte by byte.
            crate::print!("  as text: \"");
            for &b in &buf[..32] {
                crate::print!("{}", if (0x20..0x7f).contains(&b) { b as char } else { '.' });
            }
            println!("\"");
        }
        Err(e) => println!("  read failed: {}", e),
    }
}

/// A hex dump written without `core::fmt`.
///
/// Deliberately raw, and this is not premature caution. The formatted version
/// of this function -- `print!("{:08x}", ...)` in a loop -- printed its first
/// lines correctly and then stopped the kernel dead: no fault, no panic, the
/// CPU idle in wfi, every later print lost, in the Linux-linked build only.
/// The same `print!` calls work everywhere else in the kernel, including
/// immediately after this function. It is not the UART -- removing flow
/// control entirely changed nothing -- and it is not a fault, because the
/// exception path now writes a raw marker before it formats anything.
///
/// **That is an open bug, not a solved one**, and it is recorded in
/// docs/KERNEL.md rather than papered over. This version avoids it, and is
/// the better thing for a kernel to have regardless: the one routine used to
/// inspect memory when something is wrong should not itself depend on the
/// largest piece of machinery in the binary.
#[cfg(nk_linux)]
fn hexdump(bytes: &[u8], base: usize) {
    let u = uart::console();
    let _ = base;
    let hex = |n: u8| if n < 10 { b'0' + n } else { b'a' + n - 10 };
    for (i, b) in bytes.iter().enumerate() {
        if i % 16 == 0 {
            u.put(b'\r');
            u.put(b'\n');
            u.put(b' ');
            u.put(b' ');
        }
        u.put(hex(b >> 4));
        u.put(hex(b & 15));
        u.put(b' ');
    }
    u.put(b'\r');
    u.put(b'\n');
}

/// Two of these run when no Linux port is linked, to show that preemption
/// works and that each thread keeps its own stack and registers across a
/// switch it never asked for.
#[cfg(not(nk_linux))]
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

/// Give Linux the `struct page` array it believes already exists.
///
/// Everything in Linux that handles memory eventually holds a `struct page`.
/// nk got a long way without one: `virt_to_page` and `page_to_phys` are pure
/// arithmetic, and virtio-blk only ever converted an address into a page and
/// straight back again, so the pages it named never had to exist. The first
/// driver that *reads* one -- virtio-net's receive path, through
/// `virt_to_head_page` -- faults on an address nothing ever mapped.
///
/// So it gets mapped. Eight megabytes for a 512MB guest, at an address Linux's
/// own arithmetic chooses; nk allocates the memory, zeroes it and maps it
/// where the formula says it is. See emul/mm.c for why the formula lands where
/// it does.
#[cfg(nk_linux)]
fn map_vmemmap(ram_base: u64, ram_size: u64) {
    const BLOCK: u64 = 2 * 1024 * 1024;
    let (va, size) = linux::vmemmap_range(ram_base, ram_size);
    // Rounded outwards to whole 2MB blocks: the mapping granule is larger
    // than the array, and a partial block at either end would leave a page
    // Linux thinks exists unmapped.
    let start = va & !(BLOCK - 1);
    let bytes = ((va + size + BLOCK - 1) & !(BLOCK - 1)) - start;

    let pa = frames::alloc_contiguous_aligned(
        (bytes / frames::PAGE as u64) as usize,
        BLOCK as usize,
    )
    .expect("not enough memory for the struct page array");
    unsafe { paging::map_normal(start, pa as u64, bytes) };
    println!(
        "  vmemmap: {} MiB of struct page at {:#x} -> {:#x}",
        bytes / (1024 * 1024),
        start,
        pa as usize
    );
    // Proof it is really there, before anything relies on it. A write that
    // faults here is a mapping bug; one that faults later is a driver bug,
    // and telling those apart afterwards is expensive.
    unsafe {
        let p = start as *mut u64;
        p.write_volatile(0);
        p.add((bytes / 8 - 1) as usize).write_volatile(0);
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
