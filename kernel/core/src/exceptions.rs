//! What every vector entry lands in, until Stage 1 gives IRQs somewhere to go.
//!
//! Nothing here returns. An unexpected exception this early is always a bug in
//! the kernel rather than something to recover from, and the useful thing is
//! to print enough to identify it: which vector, and the three registers that
//! say why.

use crate::println;

/// Matches the order of the VENTRY macro invocations in boot.s.
const NAMES: [&str; 16] = [
    "EL1/SP0 sync", "EL1/SP0 irq", "EL1/SP0 fiq", "EL1/SP0 serror",
    "EL1/SPx sync", "EL1/SPx irq", "EL1/SPx fiq", "EL1/SPx serror",
    "EL0/a64 sync", "EL0/a64 irq", "EL0/a64 fiq", "EL0/a64 serror",
    "EL0/a32 sync", "EL0/a32 irq", "EL0/a32 fiq", "EL0/a32 serror",
];

/// ESR_EL1.EC, the exception class. Only the ones worth recognising by name
/// this early -- everything else prints as a number, which is enough to look
/// up in the ARM ARM.
fn ec_name(ec: u64) -> &'static str {
    match ec {
        0b000000 => "unknown",
        0b000111 => "SIMD/FP access trapped",
        0b001110 => "illegal execution state",
        0b010101 => "SVC",
        0b100000 => "instruction abort, lower EL",
        0b100001 => "instruction abort, same EL",
        0b100010 => "PC alignment fault",
        0b100100 => "data abort, lower EL",
        0b100101 => "data abort, same EL",
        0b100110 => "SP alignment fault",
        0b111100 => "BRK",
        _ => "?",
    }
}

#[no_mangle]
pub extern "C" fn rust_exception(id: u64, esr: u64, far: u64, elr: u64, lr: u64, sp: u64) -> ! {
    // Raw bytes first, before any formatting.
    //
    // core::fmt is a large amount of machinery to require of a kernel that
    // has just faulted, and if it is what faulted, the report never appears
    // and the machine looks like it simply stopped. Five characters through
    // the smallest path there is cost nothing and remove that failure mode.
    let u = crate::uart::console();
    for b in b"\r\n!!EXC " {
        u.put(*b);
    }
    for shift in (0..64).step_by(4).rev() {
        let n = ((esr >> shift) & 0xf) as u8;
        u.put(if n < 10 { b'0' + n } else { b'a' + n - 10 });
    }
    u.put(b'\r');
    u.put(b'\n');

    let ec = (esr >> 26) & 0x3f;
    println!();
    println!("!! exception: {}", NAMES[(id & 15) as usize]);
    println!("   esr {:#018x}  ec {:#04b}_{:04b} ({})", esr, ec >> 4, ec & 15, ec_name(ec));
    println!("   far {:#018x}   elr {:#018x}", far, elr);
    // The link register is usually the only useful thing here. When a call
    // goes through a bad function pointer, ELR is the garbage that was
    // jumped to and says nothing; LR still points just after the call that
    // did it, which names the caller exactly.
    println!("   lr  {:#018x}   sp  {:#018x}", lr, sp);
    if elr < 0x4000_0000 || elr > 0x6000_0000 {
        println!("   (elr is not in RAM: this is a jump through a bad pointer,");
        println!("    so look at lr, not elr)");
    }
    // Power off rather than spin. A kernel that halts after a fault and a
    // kernel that hung look identical from outside -- both end with the
    // watchdog -- and telling them apart mattered more than once.
    crate::psci::poweroff();
    crate::halt();
}

/// Every IRQ, from `irq_entry` in boot.s. Runs on the interrupted task's own
/// kernel stack, with its full register frame sitting just below.
#[no_mangle]
pub extern "C" fn rust_irq() {
    let intid = crate::gic::ack();

    // 1023 is the GIC's way of saying the interrupt withdrew itself between
    // being signalled and being read. It must not be EOI'd.
    if intid == 1023 {
        return;
    }

    if intid == crate::timer::intid() {
        unsafe { crate::timer::rearm() };
        crate::timer::on_tick();
        #[cfg(nk_linux)]
        unsafe {
            crate::linux::nk_tick()
        };
        // One-shot timers Linux asked for. Checked on the periodic tick,
        // which caps their resolution at one tick -- see hostops.rs.
        crate::hostops::tick_timers();
        // Completion before the switch, not after: cpu_switch does not return
        // here, it returns into some other task, and an un-EOI'd interrupt
        // stays active forever. The symptom is a timer that ticks exactly
        // once and a machine that then does nothing at all.
        crate::gic::eoi(intid);
        if crate::sched::preempt_enabled() {
            crate::sched::schedule();
        }
        return;
    }

    // Anything else belongs to a Linux driver. EOI after the handler, not
    // before: a level-triggered device interrupt stays asserted until the
    // driver quiets the device, and acknowledging it first means the GIC
    // immediately offers it again.
    #[cfg(nk_linux)]
    let handled = unsafe { crate::linux::nk_linux_irq(intid) };
    #[cfg(not(nk_linux))]
    let handled = 0;

    crate::gic::eoi(intid);
    if handled == 0 {
        println!("!! unexpected interrupt {}", intid);
    }
}
