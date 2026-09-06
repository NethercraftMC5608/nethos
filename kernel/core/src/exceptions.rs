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
pub extern "C" fn rust_exception(id: u64, esr: u64, far: u64, elr: u64) -> ! {
    let ec = (esr >> 26) & 0x3f;
    println!();
    println!("!! exception: {}", NAMES[(id & 15) as usize]);
    println!("   esr {:#018x}  ec {:#04b}_{:04b} ({})", esr, ec >> 4, ec & 15, ec_name(ec));
    println!("   far {:#018x}   elr {:#018x}", far, elr);
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

    println!("!! unexpected interrupt {}", intid);
    crate::gic::eoi(intid);
}
