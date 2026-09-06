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

    // Let EL1 read CNTPCT/CNTVCT and program the timers. Skipping this costs
    // nothing now and costs an afternoon at Stage 1, where the generic timer
    // traps to EL2 and the symptom is simply that no tick ever arrives.
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
    b       rust_exception
.endm

.global __vectors
.balign 2048
__vectors:
    VENTRY 0        // current EL, SP0:  synchronous
    VENTRY 1        // current EL, SP0:  IRQ
    VENTRY 2        // current EL, SP0:  FIQ
    VENTRY 3        // current EL, SP0:  SError
    VENTRY 4        // current EL, SPx:  synchronous
    VENTRY 5        // current EL, SPx:  IRQ
    VENTRY 6        // current EL, SPx:  FIQ
    VENTRY 7        // current EL, SPx:  SError
    VENTRY 8        // lower EL, AArch64: synchronous
    VENTRY 9        // lower EL, AArch64: IRQ
    VENTRY 10       // lower EL, AArch64: FIQ
    VENTRY 11       // lower EL, AArch64: SError
    VENTRY 12       // lower EL, AArch32: synchronous
    VENTRY 13       // lower EL, AArch32: IRQ
    VENTRY 14       // lower EL, AArch32: FIQ
    VENTRY 15       // lower EL, AArch32: SError
