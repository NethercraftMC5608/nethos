//! nk, as a machine for Linux to run on.
//!
//! LKL -- `arch/lkl` in the Linux tree -- is a real architecture port whose
//! "hardware" is a struct of function pointers the host fills in. It is 5,168
//! lines, against 26,636 for `arch/um` and 179,127 for `arch/arm64`, because
//! it delegates instead of implementing. Built for aarch64 it produces one
//! relocatable object, `lkl.o`, containing the whole Linux kernel -- VFS,
//! ext4, the network stack, every system call -- and that object has
//! **five** undefined symbols.
//!
//! This file is the nk half of that interface: threads, semaphores, mutexes,
//! thread-local storage, memory, time and one-shot timers, exported with C
//! linkage. `kernel/lkl/nk-host.c` assembles them into the struct Linux
//! expects.
//!
//! It is the same bet the rest of the project makes, one level up. The driver
//! shim reuses Linux's leaf code and writes the kernel underneath it; this
//! reuses *all* of Linux and writes only the machine underneath that. The
//! shim grows by a few hand-written functions per driver ported. This does
//! not grow at all.

use crate::sched;
use crate::sync::{irq_restore, irq_save, Mutex, Semaphore};
use alloc::boxed::Box;

// --- the services nk provides to any Linux, shim or LKL -----------------
//
// These were in linux.rs, which only exists when the driver shim is linked.
// Both routes need them, so they live here: nk_alloc is the same nk_alloc
// whether the caller is a hand-written shim function or the real slab
// allocator inside lkl.o.

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

// --- printing and stopping ----------------------------------------------

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

#[no_mangle]
pub extern "C" fn nk_halt() -> ! {
    crate::halt()
}


/// # Safety
/// `s` points at `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn nk_host_print(s: *const u8, len: i32) {
    let uart = crate::uart::console();
    for i in 0..len.max(0) as usize {
        let b = *s.add(i);
        if b == b'\n' {
            uart.put(b'\r');
        }
        uart.put(b);
    }
}

#[no_mangle]
pub extern "C" fn nk_host_panic() -> ! {
    crate::println!();
    crate::println!("!! Linux panicked");
    crate::halt()
}

// --- semaphores and mutexes ---------------------------------------------
//
// Boxed and leaked into a raw pointer, which is what a C interface handing
// back an opaque handle means. They are freed through the matching free
// operation and nowhere else.

#[no_mangle]
pub extern "C" fn nk_sem_alloc(count: i32) -> *mut Semaphore {
    Box::into_raw(Box::new(Semaphore::new(count)))
}

/// # Safety
/// `s` came from `nk_sem_alloc` and is not used again.
#[no_mangle]
pub unsafe extern "C" fn nk_sem_free(s: *mut Semaphore) {
    drop(Box::from_raw(s));
}

/// # Safety
/// `s` came from `nk_sem_alloc`.
#[no_mangle]
pub unsafe extern "C" fn nk_sem_up(s: *mut Semaphore) {
    (*s).up();
}

/// # Safety
/// `s` came from `nk_sem_alloc`.
#[no_mangle]
pub unsafe extern "C" fn nk_sem_down(s: *mut Semaphore) {
    (*s).down();
}

#[no_mangle]
pub extern "C" fn nk_mutex_alloc(recursive: i32) -> *mut Mutex {
    Box::into_raw(Box::new(Mutex::new(recursive != 0)))
}

/// # Safety
/// `m` came from `nk_mutex_alloc` and is not used again.
#[no_mangle]
pub unsafe extern "C" fn nk_mutex_free(m: *mut Mutex) {
    drop(Box::from_raw(m));
}

/// # Safety
/// `m` came from `nk_mutex_alloc`.
#[no_mangle]
pub unsafe extern "C" fn nk_mutex_lock(m: *mut Mutex) {
    (*m).lock();
}

/// # Safety
/// `m` came from `nk_mutex_alloc`.
#[no_mangle]
pub unsafe extern "C" fn nk_mutex_unlock(m: *mut Mutex) {
    (*m).unlock();
}

// --- threads -------------------------------------------------------------

/// Linux hands a `void (*)(void *)` and an argument; nk's `spawn` takes an
/// `extern "C" fn(usize)`. The trampoline exists because the two differ only
/// in how the argument is typed, and casting a function pointer to a
/// different signature is undefined behaviour rather than a formality.
static mut ENTRY: [Option<(unsafe extern "C" fn(*mut u8), *mut u8)>; 16] = [None; 16];

extern "C" fn trampoline(slot: usize) {
    let entry = unsafe { (*(&raw const ENTRY))[slot] };
    if let Some((f, arg)) = entry {
        unsafe { f(arg) };
    }
}

/// # Safety
/// `f` is a valid function and `arg` outlives the thread.
#[no_mangle]
pub unsafe extern "C" fn nk_thread_create(
    f: unsafe extern "C" fn(*mut u8),
    arg: *mut u8,
) -> usize {
    // The entry goes in *before* the task exists, and the slot index is what
    // is passed as the task's argument. Recording it afterwards would leave a
    // window in which a tick could run the trampoline against an empty slot.
    let flags = irq_save();
    let entries = &mut *(&raw mut ENTRY);
    let slot = entries
        .iter()
        .position(|e| e.is_none())
        .expect("out of Linux thread slots");
    entries[slot] = Some((f, arg));
    let task = sched::spawn("linux", trampoline, slot);
    irq_restore(flags);
    task
}

#[no_mangle]
pub extern "C" fn nk_thread_self() -> usize {
    sched::current_id()
}

#[no_mangle]
pub extern "C" fn nk_thread_exit() -> ! {
    sched::exit_current()
}

#[no_mangle]
pub extern "C" fn nk_thread_join(id: usize) -> i32 {
    sched::join(id);
    0
}

// --- thread-local storage ------------------------------------------------

/// One slot per key per task. Linux uses a handful of keys and nk has sixteen
/// tasks, so a fixed table costs a kilobyte and removes an allocator from a
/// path the scheduler calls into.
const MAX_KEYS: usize = 8;
static mut TLS: [[*mut u8; MAX_KEYS]; 16] = [[core::ptr::null_mut(); MAX_KEYS]; 16];
static mut KEYS_USED: usize = 0;

#[no_mangle]
pub extern "C" fn nk_tls_alloc() -> usize {
    unsafe {
        let k = KEYS_USED;
        assert!(k < MAX_KEYS, "out of TLS keys");
        KEYS_USED += 1;
        k
    }
}

#[no_mangle]
pub extern "C" fn nk_tls_free(_key: usize) {}

#[no_mangle]
pub extern "C" fn nk_tls_set(key: usize, value: *mut u8) -> i32 {
    unsafe {
        (*(&raw mut TLS))[sched::current_id()][key] = value;
    }
    0
}

#[no_mangle]
pub extern "C" fn nk_tls_get(key: usize) -> *mut u8 {
    unsafe { (*(&raw const TLS))[sched::current_id()][key] }
}

// --- time and one-shot timers -------------------------------------------

#[no_mangle]
pub extern "C" fn nk_time_ns() -> u64 {
    crate::timer::ticks() * (1_000_000_000 / crate::timer::HZ)
}

/// A one-shot timer, checked on the periodic tick.
///
/// Not a real one-shot: nk's timer runs at a fixed 100Hz and these are
/// deadlines compared against it. So the resolution is 10ms and a timer set
/// for less than that fires at the next tick. Linux will not like that under
/// load, and the fix is a proper deadline timer -- programming CNTV_CVAL for
/// the nearest deadline rather than a fixed interval -- which is a change to
/// `timer.rs` and not to this interface.
struct OneShot {
    deadline: u64,
    armed: bool,
    fire: extern "C" fn(),
}

static mut TIMERS: [Option<OneShot>; 16] = [const { None }; 16];

#[no_mangle]
pub extern "C" fn nk_timer_alloc(fire: extern "C" fn()) -> usize {
    unsafe {
        let timers = &mut *(&raw mut TIMERS);
        for (i, slot) in timers.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(OneShot { deadline: 0, armed: false, fire });
                return i;
            }
        }
        panic!("out of one-shot timers");
    }
}

#[no_mangle]
pub extern "C" fn nk_timer_set_oneshot(id: usize, delta_ns: u64) -> i32 {
    unsafe {
        // Divide first, never multiply. Linux arms this with whatever its
        // next event is, and a long timeout is a very large number of
        // nanoseconds -- `delta_ns * HZ` overflows a u64 well before the
        // deadline is unreasonable, and the wrapped result lands in the past
        // or the far future. Neither fires; the clock simply stops, and a
        // Linux whose clock has stopped does not report anything, it just
        // stops making progress.
        let ns_per_tick = 1_000_000_000 / crate::timer::HZ;
        let ticks = (delta_ns / ns_per_tick).max(1);
        if let Some(t) = (*(&raw mut TIMERS))[id].as_mut() {
            t.deadline = crate::timer::ticks() + ticks;
            t.armed = true;
            N_ARMED += 1;
            LAST_DEADLINE = t.deadline;
        }
    }
    0
}

#[no_mangle]
pub extern "C" fn nk_timer_free(id: usize) {
    unsafe { (*(&raw mut TIMERS))[id] = None };
}

static mut DUE: u32 = 0;
static mut TIMER_TASK: usize = usize::MAX;

// PROBE
pub static mut N_ARMED: u64 = 0;
pub static mut N_DUE: u64 = 0;
pub static mut N_FIRED: u64 = 0;
pub static mut LAST_DEADLINE: u64 = 0;

/// Called from the timer interrupt. Marks what is due and wakes the thread
/// that will run it -- it does not run anything itself.
///
/// That distinction is the whole of this function and it is not fussiness.
/// Linux's timer callback goes back into Linux, and Linux takes mutexes; a
/// mutex nk cannot grant blocks the caller, and blocking inside an interrupt
/// handler marks the *interrupted* task blocked and switches away from a
/// stack that is halfway through an exception. Nothing reports it. The
/// machine simply stops making progress, which is exactly how this presented.
pub fn tick_timers() {
    let now = crate::timer::ticks();
    unsafe {
        let timers = &*(&raw const TIMERS);
        let mut due = 0u32;
        for (i, slot) in timers.iter().enumerate() {
            if let Some(t) = slot {
                if t.armed && now >= t.deadline {
                    due |= 1 << i;
                }
            }
        }
        if due != 0 {
            N_DUE += 1;
            DUE |= due;
            let task = core::ptr::read(&raw const TIMER_TASK);
            if task != usize::MAX {
                sched::wake(task);
            }
        }
    }
}

/// The thread that runs timer callbacks, in thread context where they may
/// block. Started by `start_timer_thread`.
extern "C" fn timer_thread(_: usize) {
    loop {
        let flags = irq_save();
        let due = unsafe { core::mem::take(&mut *(&raw mut DUE)) };
        if due == 0 {
            // Nothing to do. Block with interrupts masked until the tick
            // marks something due -- the same ordering every other wait in
            // nk uses, for the same reason.
            sched::block(flags);
            continue;
        }
        unsafe { irq_restore(flags) };

        for i in 0..32 {
            if due & (1 << i) == 0 {
                continue;
            }
            let fire = unsafe {
                let timers = &mut *(&raw mut TIMERS);
                timers[i].as_mut().filter(|t| t.armed).map(|t| {
                    t.armed = false;
                    t.fire
                })
            };
            if let Some(f) = fire {
                unsafe { N_FIRED += 1 };
                f();
            }
        }
    }
}

pub fn start_timer_thread() {
    let id = sched::spawn("timers", timer_thread, 0);
    unsafe { TIMER_TASK = id };
}
