//! The host end of the console Linux owns.
//!
//! Output goes out through `lkl_ops->print`, which is nk's UART and needs
//! nothing here. Input is the part that needs a shape: a key arrives when the
//! UART says so, and Linux cannot be called from wherever that happens --
//! LKL's kernel runs under a lock nk does not hold in an interrupt.
//!
//! So the two are decoupled the way every LKL device is. nk buffers the byte
//! and raises an interrupt; Linux's handler calls back to collect it. The
//! ring is nk's own memory and nothing in Linux touches it, which is what
//! makes `nk_console_read` safe to call from Linux's interrupt context.

const RING: usize = 512;

/// Masking interrupts is the whole of the mutual exclusion, because nk runs on
/// one CPU and the two ends of this ring are an interrupt handler and a
/// thread. A blocking lock would be wrong in the first and unnecessary in the
/// second.
static mut BUF: [u8; RING] = [0; RING];
static mut HEAD: usize = 0;
static mut TAIL: usize = 0;

extern "C" {
    /// Which interrupt to raise. The driver reserves it at init and nk asks
    /// once; a plain read of an integer, so it needs no lock.
    fn lkl_console_irq() -> i32;
    fn lkl_trigger_irq(irq: i32) -> i32;
}

/// Accept a byte from the UART. Dropped when the ring is full, which is what
/// a UART with no flow control does anyway.
pub fn input(byte: u8) {
    unsafe {
        let flags = crate::sync::irq_save();
        let next = (HEAD + 1) % RING;
        if next != TAIL {
            BUF[HEAD] = byte;
            HEAD = next;
        }
        crate::sync::irq_restore(flags);
    }
    let irq = unsafe { lkl_console_irq() };
    if irq >= 0 {
        unsafe { lkl_trigger_irq(irq) };
    }
}

/// Called by the console driver's interrupt handler.
#[no_mangle]
pub extern "C" fn nk_console_read(buf: *mut u8, max: i32) -> i32 {
    if buf.is_null() || max <= 0 {
        return 0;
    }
    unsafe {
        let flags = crate::sync::irq_save();
        let mut n = 0;
        while n < max as usize && TAIL != HEAD {
            *buf.add(n) = BUF[TAIL];
            TAIL = (TAIL + 1) % RING;
            n += 1;
        }
        crate::sync::irq_restore(flags);
        n as i32
    }
}
