//! PL011, the serial port QEMU's `virt` machine puts at 0x0900_0000.
//!
//! Deliberately not initialised. QEMU's PL011 comes up enabled with the
//! transmitter running, and the baud rate is meaningless to a character
//! device on the other end of a pipe. Programming IBRD/FBRD/LCR_H here would
//! be code that cannot be wrong on QEMU and cannot be right on real hardware
//! without knowing UARTCLK -- which is in the device tree, which Stage 1
//! parses. Until then, write and go.

use core::fmt;

/// Fixed for QEMU `virt`. Stage 1 replaces this with the address the device
/// tree reports, at which point this constant becomes the fallback used
/// before the DTB has been parsed -- printing has to work before that.
const PL011_BASE: usize = 0x0900_0000;

const DR: usize = 0x00; // data
const FR: usize = 0x18; // flag; bit 5 is TXFF, transmit FIFO full

pub struct Uart {
    base: usize,
}

impl Uart {
    pub const fn new(base: usize) -> Self {
        Uart { base }
    }

    pub fn put(&self, byte: u8) {
        unsafe {
            // Wait for room, but bounded, and write anyway when the wait runs
            // out.
            //
            // An unbounded spin gives the console exactly one failure mode --
            // the machine stops mid-line with no message -- which is
            // indistinguishable from every other kind of hang, in the one
            // device that reports all the others. The bound is small on
            // purpose: under a hypervisor each of these reads is a trap out
            // to the emulator, so a large one turns a stuck FIFO into a
            // kernel that appears to hang anyway, just more slowly. That was
            // measured, at 100000 spins a character.
            let mut spins = 0;
            while crate::mmio::readl(self.base + FR) & (1 << 5) != 0 && spins < 1000 {
                spins += 1;
            }
            crate::mmio::writel(self.base + DR, byte as u32);
        }
    }
}

impl fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            // A bare \n leaves the cursor in column 0 on a real terminal.
            if b == b'\n' {
                self.put(b'\r');
            }
            self.put(b);
        }
        Ok(())
    }
}

/// The console used before anything is initialised, and the one the panic
/// handler uses. No lock: Stage 0 is single-threaded by construction (every
/// other CPU is parked in boot.s) and a lock here would be a lock the panic
/// path could deadlock on.
pub fn console() -> Uart {
    Uart::new(PL011_BASE)
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!($crate::uart::console(), $($arg)*);
    }};
}

#[macro_export]
macro_rules! println {
    ()                 => { $crate::print!("\n") };
    ($($arg:tt)*)      => { $crate::print!("{}\n", format_args!($($arg)*)) };
}
