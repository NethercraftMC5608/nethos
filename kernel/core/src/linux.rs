//! The whole of what Linux code may call in nk.
//!
//! Deliberately tiny, and deliberately in terms of nothing but integers and
//! byte pointers. The shim in `kernel/linux/emul/` is compiled with Linux's
//! headers in scope and knows all of Linux's types; this side knows none of
//! them and must not start to. Everything that needs a `struct request` or a
//! `struct virtio_device` belongs on the other side of this boundary.
//!
//! It is also the licence boundary. `core/` is ours; `linux/` is GPL-2.0.

use crate::println;

/// Write bytes to the console. Not NUL-terminated -- Linux's `vscnprintf`
/// returns a length and passing it through avoids a second pass over the
/// string in the one place that runs on every single log line.
///
/// # Safety
/// `s` must point at `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn nk_console_write(s: *const u8, len: usize) {
    if s.is_null() {
        return;
    }
    let uart = crate::uart::console();
    for i in 0..len {
        let b = *s.add(i);
        if b == b'\n' {
            uart.put(b'\r');
        }
        uart.put(b);
    }
}

/// A disk reported its size. Recorded rather than acted on: nk has no
/// filesystem and nothing to mount, and the number is the clearest single
/// piece of evidence that the driver reached the device and read its
/// configuration space.
#[no_mangle]
pub extern "C" fn nk_set_capacity(sectors: u64) {
    unsafe { CAPACITY = sectors };
}

static mut CAPACITY: u64 = 0;

pub fn capacity() -> u64 {
    unsafe { core::ptr::read(&raw const CAPACITY) }
}

/// Read `buf.len()` bytes starting at `sector`, through the real driver.
pub fn read(sector: u64, buf: &mut [u8]) -> Result<(), i32> {
    let rc = unsafe { nk_blk_read(sector, buf.as_mut_ptr(), buf.len() as u32) };
    if rc == 0 {
        Ok(())
    } else {
        Err(rc)
    }
}

#[no_mangle]
pub extern "C" fn nk_halt() -> ! {
    crate::halt()
}

// --- what the shim may call in nk ---------------------------------------

/// `kmalloc`. Linux has no alignment argument, so the shim asks for the
/// largest alignment any kernel allocation is assumed to have.
#[no_mangle]
pub extern "C" fn nk_alloc(size: usize, align: usize) -> *mut u8 {
    unsafe { crate::heap::alloc_raw(size, align) }
}

/// # Safety
/// `p` came from `nk_alloc`.
#[no_mangle]
pub unsafe extern "C" fn nk_free(p: *mut u8) {
    crate::heap::free_raw(p)
}

/// Whole pages, for the ring buffers a virtqueue needs.
#[no_mangle]
pub extern "C" fn nk_alloc_pages(n: usize) -> *mut u8 {
    crate::frames::alloc_contiguous(n).unwrap_or(core::ptr::null_mut())
}

/// Mask interrupts and report the previous state, for a Linux spinlock.
#[no_mangle]
pub extern "C" fn nk_irq_save() -> u64 {
    let daif: u64;
    unsafe {
        core::arch::asm!("mrs {}, daif", "msr daifset, #0x2", out(reg) daif, options(nomem, nostack))
    };
    daif
}

/// # Safety
/// `flags` came from `nk_irq_save`.
#[no_mangle]
pub unsafe extern "C" fn nk_irq_restore(flags: u64) {
    core::arch::asm!("msr daif, {}", in(reg) flags, options(nomem, nostack));
}

#[no_mangle]
pub extern "C" fn nk_yield() {
    crate::sched::yield_now()
}

#[no_mangle]
pub extern "C" fn nk_ticks() -> u64 {
    crate::timer::ticks()
}

#[no_mangle]
pub extern "C" fn nk_hz() -> u64 {
    crate::timer::HZ
}

/// Route a device interrupt to a Linux handler.
///
/// The handler is stored rather than called from the GIC path directly: the
/// C side owns the `void *dev_id` cookie and the two-level threaded-IRQ
/// convention, and neither belongs on this side of the boundary.
///
/// # Safety
/// `intid` is a real, routable interrupt on this machine.
#[no_mangle]
pub unsafe extern "C" fn nk_request_irq(intid: u32) -> i32 {
    if intid < 32 {
        crate::gic::enable_ppi(intid);
    } else {
        crate::gic::enable_spi(intid);
    }
    0
}

extern "C" {
    fn nk_linux_init() -> i32;
    fn nk_add_virtio_mmio(base: u64, size: u64, irq: u32) -> i32;
    fn nk_work_drain();
    /// Advances Linux's `jiffies`. Not optional: drivers read the variable
    /// directly and loop until it changes, so a jiffies that never moves
    /// turns every timeout in every driver into an infinite one -- which
    /// presents as a device that never answers.
    pub fn nk_tick();
    fn nk_blk_read(sector: u64, buf: *mut u8, len: u32) -> i32;
    fn nk_net_up(mac: *mut u8) -> i32;
    fn nk_net_xmit(frame: *const u8, len: u32) -> i32;
    fn nk_net_poll();
    fn nk_net_recv(out: *mut u8, max: u32) -> u32;
    /// Called from the IRQ path for anything that is not nk's own timer.
    pub fn nk_linux_irq(intid: u32) -> i32;
}

/// Hand every virtio-mmio transport the device tree describes to the shim,
/// which turns each into a `platform_device` for virtio_mmio.c to probe.
///
/// All of them, including the empty ones. QEMU's `virt` always advertises 32
/// transports and populates only the ones with a `-device` behind them; the
/// rest answer with a device ID of zero and virtio_mmio.c rejects them
/// itself. Filtering them out here would mean reimplementing that check
/// against the driver, which is the sort of duplication that eventually
/// disagrees.
pub fn add_virtio_devices(fdt: &crate::dt::Fdt) -> usize {
    let mut n = 0;
    fdt.each_compatible("virtio,mmio", |node| {
        let Some((base, size)) = node.reg(0) else { return };
        // An SPI in the device tree is numbered from the start of the shared
        // range, not as an absolute INTID: type 0, number N, means INTID
        // N + 32. Getting this wrong gives a device that probes perfectly and
        // never delivers a completion.
        let Some((ty, num, _)) = node.interrupt(0) else { return };
        let intid = if ty == 0 { num + 32 } else { num + 16 };
        if unsafe { nk_add_virtio_mmio(base, size, intid) } == 0 {
            n += 1;
        }
    });
    n
}

/// Run every linked-in driver's `module_init`.
///
/// Nothing is called directly by name. The drivers registered themselves into
/// `.initcallN.init` sections at compile time and linker.ld gathered them in
/// level order; the walk itself is in `emul/glue.c`, because on arm64 an
/// initcall entry is a 32-bit relative offset rather than a pointer and
/// Linux's own `initcall_from_entry` is the only version of that which cannot
/// be subtly wrong.
pub fn init() {
    println!();
    println!("  linux:  running initcalls");
    let n = unsafe { nk_linux_init() };
    println!("  linux:  {} initcalls ran", n);
}

/// Bring the network interface up and report its MAC address.
///
/// `register_netdevice` does not open a device -- in Linux that is `ip link
/// set up`, from userspace, and nk is the userspace.
pub fn net_up() -> Option<[u8; 6]> {
    let mut mac = [0u8; 6];
    (unsafe { nk_net_up(mac.as_mut_ptr()) } == 0).then_some(mac)
}

pub fn net_xmit(frame: &[u8]) -> Result<(), i32> {
    let rc = unsafe { nk_net_xmit(frame.as_ptr(), frame.len() as u32) };
    if rc == 0 { Ok(()) } else { Err(rc) }
}

/// Run whatever NAPI polling the driver has asked for, then take a frame if
/// one arrived. Polling here rather than on a thread keeps the whole receive
/// path on one stack while it is being brought up.
pub fn net_recv(out: &mut [u8]) -> usize {
    unsafe {
        nk_net_poll();
        nk_net_recv(out.as_mut_ptr(), out.len() as u32) as usize
    }
}

/// The thread Linux's workqueues run on.
///
/// A real thread rather than running work inline at queue time. Work is
/// deferred precisely because the caller is somewhere it must not run it --
/// inside an interrupt handler, or holding a lock the work will take -- and
/// running it there deadlocks at a point a long way from the queue_work that
/// caused it.
extern "C" fn work_thread(_: usize) {
    loop {
        unsafe { nk_work_drain() };
        crate::sched::yield_now();
    }
}

/// Present the machine's devices, after the drivers have registered.
pub fn probe(fdt: &crate::dt::Fdt) {
    crate::sched::spawn("kworker", work_thread, 0);
    println!("  linux:  presenting virtio-mmio transports");
    let n = add_virtio_devices(fdt);
    println!("  linux:  {} transports offered", n);
}
