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
const FR: usize = 0x18; // flag; bit 4 is RXFE, bit 5 TXFF
const IMSC: usize = 0x38; // interrupt mask set/clear
const MIS: usize = 0x40; // masked interrupt status
const ICR: usize = 0x44; // interrupt clear

const RXFE: u32 = 1 << 4; // receive FIFO empty
const RXIM: u32 = 1 << 4; // receive interrupt
const RTIM: u32 = 1 << 6; // receive timeout

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

    /// One byte, or None when the receive FIFO is empty.
    pub fn get(&self) -> Option<u8> {
        unsafe {
            if crate::mmio::readl(self.base + FR) & RXFE != 0 {
                return None;
            }
            // The low eight bits are the character; the rest are the framing,
            // parity and overrun flags, which say the byte arrived badly
            // rather than that it is a different byte.
            Some(crate::mmio::readl(self.base + DR) as u8)
        }
    }

    /// Ask to be interrupted when input arrives.
    ///
    /// Both the receive interrupt and the receive *timeout*. The first fires
    /// when the FIFO reaches its trigger level -- a quarter full, so eight
    /// characters -- and on its own it means a person typing one character
    /// gets no interrupt until they have typed eight. The timeout fires when
    /// the FIFO is non-empty and has been idle for the length of 32 bits,
    /// which is what makes a single keystroke arrive.
    ///
    /// # Safety
    /// The GIC must be up, or the interrupt has nowhere to go.
    pub unsafe fn enable_receive(&self) {
        // Nothing is cleared here, and that is the point. A character typed
        // before the kernel got this far is already in the receive register
        // with its interrupt raised, and the raw status is what remembers
        // that -- the PL011 raises it once, when the character arrives, not
        // for as long as the character is there. Clearing "anything stale"
        // threw exactly that away, and the input then waited for a keystroke
        // that had already happened. With both sources masked until now, a
        // stale bit cannot have interrupted anything anyway.
        crate::mmio::writel(self.base + IMSC, RXIM | RTIM);
    }

    /// Acknowledge and drain. Returns how many bytes it took.
    pub fn drain_receive(&self, mut sink: impl FnMut(u8)) -> usize {
        unsafe {
            crate::mmio::writel(self.base + ICR, crate::mmio::readl(self.base + MIS));
        }
        let mut n = 0;
        while let Some(b) = self.get() {
            sink(b);
            n += 1;
        }
        n
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

/// Which interrupt the PL011 raises, from the device tree. SPI 1 on QEMU's
/// `virt`, which is INTID 33; the number is read rather than assumed because
/// assuming it is how a kernel ends up working on exactly one machine.
static mut RX_INTID: u32 = 0;

pub fn rx_intid() -> u32 {
    unsafe { RX_INTID }
}

/// # Safety
/// The GIC must be initialised.
pub unsafe fn init_receive(intid: u32) {
    RX_INTID = intid;
    console().enable_receive();
    crate::gic::enable_spi(intid);
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
