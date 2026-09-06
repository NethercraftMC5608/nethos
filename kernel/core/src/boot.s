// Entry from QEMU. x0 holds the physical address of the flattened device
// tree; the arm64 boot protocol guarantees that, and it is the only thing
// this kernel knows about the machine, so it is preserved all the way into
// Rust rather than being clobbered by the setup below.

.section ".text.boot", "ax"
.global _start

// The arm64 Linux image header, 64 bytes, exactly as
// Documentation/arch/arm64/booting.rst defines it.
//
// nk is not Linux and does not want to be, but this header is the only way to
// be told what machine you are on. Handed an ELF, QEMU loads the segments,
// jumps to the entry point and stops there: no bootloader stub, no device
// tree, x0 zero. Measured -- a scan of the first 2MB of RAM found no FDT
// magic and nothing but zeroes at the bottom of memory. Handed an image with
// this header it takes the Linux path instead: it places a generated DTB in
// RAM and enters with x0 pointing at it, which is the entire boot protocol nk
// needs. The same header is also what GRUB, U-Boot and the EFI stub look for,
// so this is not a QEMU workaround.
//
// text_offset 0x80000 with flags bit 3 clear asks to be loaded 512KB above the
// start of RAM, which on `virt` is 0x40080000 -- the address linker.ld links
// for. nk is not position-independent, so that has to be an agreement rather
// than a hope.
_start:
    b       real_start              // code0: the branch is also the first instruction
    .long   0                       // code1: unused
    .quad   0x0000000000080000      // text_offset
    .quad   __image_end - _start    // image_size, BSS and boot stack included
    .quad   0                       // flags: LE, page size unspecified, fixed base
    .quad   0                       // res2
    .quad   0                       // res3
    .quad   0                       // res4
    .long   0x644d5241              // magic, "ARM\x64"
    .long   0                       // res5: PE/COFF header offset; nk is not an EFI image

real_start:
    mov     x19, x0                 // stash the DTB pointer; x0 is about to go

    // QEMU starts every CPU the -smp count asks for, all at _start. Only
    // affinity 0 goes on; the rest wait in park until Stage 1 has an SMP
    // bring-up path. Without this, several CPUs race to zero the same BSS.
    mrs     x1, mpidr_el1
    and     x1, x1, #0xff
    cbnz    x1, park

    // With `-M virt,virtualization=on` (or under some firmware) we arrive at
    // EL2 instead of EL1. Everything below assumes EL1, so drop if needed.
    mrs     x0, CurrentEL
    lsr     x0, x0, #2
    cmp     x0, #2
    b.ne    at_el1

    mov     x0, #(1 << 31)          // HCR_EL2.RW: EL1 runs AArch64, not AArch32
    msr     hcr_el2, x0

    // Let EL1 read the counters, and make its virtual counter equal the
    // physical one. nk uses the *virtual* timer (see timer.rs), so CNTVOFF is
    // the line that matters here -- left at whatever reset gave it, EL1's idea
    // of the time is offset from the machine's by an arbitrary amount.
    //
    // This path only runs when nk boots at EL2. Under a hypervisor it does
    // not, EL2 is not ours, and no amount of setup here can grant EL1 the
    // physical timer -- which is exactly why the virtual one is used.
    mrs     x0, cnthctl_el2
    orr     x0, x0, #3              // EL1PCTEN | EL1PCEN
    msr     cnthctl_el2, x0
    msr     cntvoff_el2, xzr        // EL1's virtual counter starts at 0

    mov     x0, #0x3c5              // DAIF all masked, return to EL1h (own SP)
    msr     spsr_el2, x0
    adr     x0, at_el1
    msr     elr_el2, x0
    eret

at_el1:
    ldr     x0, =__stack_top
    mov     sp, x0

    // Zero the BSS. Rust assumes it: every `static mut` and every zeroed
    // static lives here, and QEMU does not guarantee the memory is clean.
    ldr     x0, =__bss_start
    ldr     x1, =__bss_end
0:  cmp     x0, x1
    b.hs    1f
    str     xzr, [x0], #8
    b       0b
1:

    ldr     x0, =__vectors
    msr     vbar_el1, x0
    isb

    mov     x0, x19                 // rust_main(dtb: *const u8)
    bl      rust_main
    // rust_main is -> !, so this is only reached if it ever stops being that.

park:
    wfe
    b       park


// The vector table. Sixteen entries, each exactly 0x80 bytes, in the fixed
// order the architecture defines: four exception kinds (sync, IRQ, FIQ,
// SError) for each of four sources (current EL on SP0, current EL on SPx,
// lower EL in AArch64, lower EL in AArch32).
//
// Every entry currently reports and halts. x0 is clobbered without being
// saved, which is fine only because nothing here returns -- the moment an
// entry needs to `eret`, it needs a full register save first.

.section ".text.vectors", "ax"

.macro VENTRY id
.balign 0x80
    mov     x0, #\id
    mrs     x1, esr_el1
    mrs     x2, far_el1
    mrs     x3, elr_el1
    mov     x4, x30                 // the link register: who called the code that faulted
    mov     x5, sp
    b       rust_exception
.endm

// An IRQ, unlike every other entry here, returns -- so it saves state first.
//
// x0-x30 plus ELR_EL1 and SPSR_EL1: everything the interrupted code could be
// holding. x19-x28 are callee-saved and rust_irq will preserve them, but they
// are saved anyway because the scheduler switches stacks inside this handler,
// and the frame is then also what the *other* task returns through.
//
// No stack switch. The handler runs on the interrupted task's own kernel
// stack, which is what makes a context switch from here work at all: the
// frame stays behind on the outgoing stack and is still there, untouched,
// whenever that task is next chosen.
.macro VENTRY_IRQ
.balign 0x80
    b       irq_entry
.endm

// A system call, or a fault in user space. Its own entry rather than
// irq_entry's because it must also save SP_EL0 -- which holds the user's
// stack pointer on the way in and the current task pointer on the way out.
.macro VENTRY_EL0_SYNC
.balign 0x80
    b       el0_sync_entry
.endm

.global __vectors
.balign 2048
__vectors:
    VENTRY 0        // current EL, SP0:  synchronous
    VENTRY_IRQ      // current EL, SP0:  IRQ  (nk runs on SPx; cannot happen)
    VENTRY 2        // current EL, SP0:  FIQ
    VENTRY 3        // current EL, SP0:  SError
    VENTRY 4        // current EL, SPx:  synchronous
    VENTRY_IRQ      // current EL, SPx:  IRQ  <- the only one that fires
    VENTRY 6        // current EL, SPx:  FIQ
    VENTRY 7        // current EL, SPx:  SError
    VENTRY_EL0_SYNC // lower EL, AArch64: synchronous  <- system calls
    // Deliberately not irq_entry. An interrupt from a lower EL arrives with
    // SP still pointing at the user stack, and irq_entry pushes its frame
    // wherever SP happens to be. There is no EL0 yet, so this cannot fire --
    // and when there is one, this has to become a handler that swaps stacks
    // first rather than a line someone changes without noticing.
    VENTRY 9        // lower EL, AArch64: IRQ
    VENTRY 10       // lower EL, AArch64: FIQ
    VENTRY 11       // lower EL, AArch64: SError
    VENTRY 12       // lower EL, AArch32: synchronous
    VENTRY 13       // lower EL, AArch32: IRQ
    VENTRY 14       // lower EL, AArch32: FIQ
    VENTRY 15       // lower EL, AArch32: SError


// 272 bytes: 31 general registers, then ELR_EL1 and SPSR_EL1. Sixteen-byte
// aligned throughout, which the architecture requires of SP at every point an
// exception could be taken -- including inside this handler.
.section ".text", "ax"
irq_entry:
    sub     sp, sp, #272
    stp     x0,  x1,  [sp, #16 * 0]
    stp     x2,  x3,  [sp, #16 * 1]
    stp     x4,  x5,  [sp, #16 * 2]
    stp     x6,  x7,  [sp, #16 * 3]
    stp     x8,  x9,  [sp, #16 * 4]
    stp     x10, x11, [sp, #16 * 5]
    stp     x12, x13, [sp, #16 * 6]
    stp     x14, x15, [sp, #16 * 7]
    stp     x16, x17, [sp, #16 * 8]
    stp     x18, x19, [sp, #16 * 9]
    stp     x20, x21, [sp, #16 * 10]
    stp     x22, x23, [sp, #16 * 11]
    stp     x24, x25, [sp, #16 * 12]
    stp     x26, x27, [sp, #16 * 13]
    stp     x28, x29, [sp, #16 * 14]
    mrs     x0, elr_el1
    mrs     x1, spsr_el1
    stp     x30, x0,  [sp, #16 * 15]
    str     x1,       [sp, #16 * 16]

    bl      rust_irq

    ldr     x1,       [sp, #16 * 16]
    ldp     x30, x0,  [sp, #16 * 15]
    msr     elr_el1, x0
    msr     spsr_el1, x1
    ldp     x0,  x1,  [sp, #16 * 0]
    ldp     x2,  x3,  [sp, #16 * 1]
    ldp     x4,  x5,  [sp, #16 * 2]
    ldp     x6,  x7,  [sp, #16 * 3]
    ldp     x8,  x9,  [sp, #16 * 4]
    ldp     x10, x11, [sp, #16 * 5]
    ldp     x12, x13, [sp, #16 * 6]
    ldp     x14, x15, [sp, #16 * 7]
    ldp     x16, x17, [sp, #16 * 8]
    ldp     x18, x19, [sp, #16 * 9]
    ldp     x20, x21, [sp, #16 * 10]
    ldp     x22, x23, [sp, #16 * 11]
    ldp     x24, x25, [sp, #16 * 12]
    ldp     x26, x27, [sp, #16 * 13]
    ldp     x28, x29, [sp, #16 * 14]
    add     sp, sp, #272
    eret


// cpu_switch(prev_sp: *mut usize, next_sp: usize)
//
// Only the callee-saved registers, because this is reached by an ordinary
// function call: AAPCS already says x0-x18 are the caller's problem, and the
// caller has already dealt with them. What makes it a context switch rather
// than a function call is the two instructions in the middle that put SP
// somewhere else.
.global cpu_switch
cpu_switch:
    sub     sp, sp, #96
    stp     x19, x20, [sp, #16 * 0]
    stp     x21, x22, [sp, #16 * 1]
    stp     x23, x24, [sp, #16 * 2]
    stp     x25, x26, [sp, #16 * 3]
    stp     x27, x28, [sp, #16 * 4]
    stp     x29, x30, [sp, #16 * 5]
    mov     x2, sp
    str     x2, [x0]                // remember where the outgoing task stopped
    mov     sp, x1                  // and take up where the incoming one did
    ldp     x19, x20, [sp, #16 * 0]
    ldp     x21, x22, [sp, #16 * 1]
    ldp     x23, x24, [sp, #16 * 2]
    ldp     x25, x26, [sp, #16 * 3]
    ldp     x27, x28, [sp, #16 * 4]
    ldp     x29, x30, [sp, #16 * 5]
    add     sp, sp, #96
    ret                             // to wherever x30 came from: the other task


// Where a task begins. sched.rs builds a frame whose x30 is this label and
// whose x19/x20 hold the entry point and its argument, so the `ret` above
// lands here with everything already in place. A task that returns falls into
// task_exit rather than off the end of its stack.
.global task_start
task_start:
    // Unmask interrupts before the task's first instruction.
    //
    // Every other task resumes by returning through irq_entry's epilogue,
    // whose `eret` restores SPSR_EL1 and with it the interrupt mask. A brand
    // new task has no such frame -- cpu_switch simply `ret`s here -- so it
    // inherits DAIF exactly as the timer handler left it, which is masked,
    // because the CPU masks interrupts on exception entry.
    //
    // The symptom is precise and misleading: the first task starts, runs, and
    // then the whole machine stops. Nothing has crashed. It is spinning with
    // the only thing that could ever preempt it switched off.
    msr     daifclr, #0xf
    mov     x0, x20
    blr     x19
    b       task_exit
