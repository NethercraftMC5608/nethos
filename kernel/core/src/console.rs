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
    fn lkl_trigger_irq(irq: i32) -> i32;
}

/// Which interrupt to raise, told to nk by the driver's initcall.
///
/// Pushed rather than pulled: a global function in the driver that nothing
/// inside Linux calls is dropped by the kernel's own `--gc-sections`, and the
/// link then fails on a symbol whose definition is plainly in the source.
static mut IRQ: i32 = -1;

#[no_mangle]
pub extern "C" fn nk_console_ready(irq: i32) {
    unsafe {
        let flags = crate::sync::irq_save();
        IRQ = irq;
        // Anything typed while Linux was still booting is in the ring and the
        // thread is asleep with nowhere to have sent it.
        if PUMP != 0 && PENDING {
            crate::sched::wake(PUMP);
        }
        crate::sync::irq_restore(flags);
    }
}

/// The thread that tells Linux input has arrived.
///
/// Raising LKL's interrupt takes LKL's CPU lock, and taking a lock in an
/// interrupt handler is the mistake nk already made once with timers: it
/// blocks the task that happened to be interrupted, on a half-finished
/// exception stack. So the handler only buffers and marks, and this does the
/// part that can wait.
static mut PENDING: bool = false;
static mut PUMP: usize = 0;

extern "C" fn pump(_: usize) {
    loop {
        let flags = crate::sync::irq_save();
        // Nothing to say, or nowhere yet to say it. A key typed before the
        // console driver has registered stays pending rather than being
        // consumed: the driver wakes this thread when it is ready, and the
        // bytes are already safe in the ring.
        if !unsafe { PENDING } || unsafe { IRQ } < 0 {
            crate::sched::block(flags);
            continue;
        }
        unsafe { PENDING = false };
        unsafe { crate::sync::irq_restore(flags) };

        unsafe { lkl_trigger_irq(IRQ) };
    }
}

pub fn start_input_thread() {
    let id = crate::sched::spawn("console", pump, 0);
    unsafe { PUMP = id };
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
    // Zero until the thread exists, and task zero is the boot thread: waking
    // it because somebody typed early would mark it ready while it waits on
    // something else entirely.
    unsafe {
        PENDING = true;
        if PUMP != 0 {
            crate::sched::wake(PUMP);
        }
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
