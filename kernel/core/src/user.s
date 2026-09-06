// Entry from EL0, and the smallest possible program to enter from.
//
// Two things make this different from irq_entry. The stack: an exception from
// EL0 switches the CPU to SP_EL1 automatically, so the kernel stack is already
// in place and there is nothing to swap -- but SP_EL0 is the *user's* stack
// pointer and nk uses SP_EL0 for the current task pointer while in the kernel
// (that is where the stack-protector canary lives for every Linux file the
// shim compiles). So it is saved into the frame on the way in and put back on
// the way out, which is exactly what Linux does and for the same reason.

.section ".text", "ax"

// The frame is irq_entry's, with one more slot: 31 registers, ELR, SPSR, and
// the user stack pointer at 264.
.global el0_sync_entry
el0_sync_entry:
    sub     sp, sp, #288
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
    mrs     x2, sp_el0
    stp     x30, x0,  [sp, #16 * 15]
    stp     x1,  x2,  [sp, #16 * 16]

    // SP_EL0 now means "current task" again, not "user stack".
    bl      nk_enter_kernel

    mov     x0, sp
    bl      rust_el0_sync

    ldp     x1,  x2,  [sp, #16 * 16]
    msr     spsr_el1, x1
    msr     sp_el0, x2
    ldp     x30, x0,  [sp, #16 * 15]
    msr     elr_el1, x0
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
    add     sp, sp, #288
    eret


// enter_user(entry, stack, ttbr0)
//
// Switch to the process's address space and drop to EL0. Does not return:
// every way back is through a vector.
.global enter_user
enter_user:
    // The TLB holds translations made under the previous TTBR0, and nothing
    // distinguishes them -- nk uses ASID 0 throughout, so the hardware cannot
    // tell one address space from another and every switch has to flush.
    msr     ttbr0_el1, x2
    dsb     ishst
    tlbi    vmalle1
    dsb     ish
    isb

    msr     sp_el0, x1              // the user stack
    msr     elr_el1, x0             // where to start
    mov     x3, #0                  // EL0t, DAIF clear: interrupts on in user
    msr     spsr_el1, x3
    eret


// The program itself, in the kernel image, because nk has no filesystem to
// load one from yet. Position-independent -- it is copied to whatever address
// the user address space puts it at, so every reference is PC-relative.
//
// Linux's aarch64 syscall convention: number in x8, arguments in x0-x5,
// result in x0. These are the real numbers, not nk's own: the entire point of
// user space here is to be the ABI that existing binaries were compiled for.
.section ".rodata.user", "a"
// A real ELF file fixture. Linux's rootfs stores these bytes; nk reads the
// file through the VFS and maps PT_LOAD, rather than copying a raw program.
.balign 4096
.global __user_elf_start
__user_elf_start:
    .byte 0x7f, 0x45, 0x4c, 0x46, 2, 1, 1, 0
    .zero 8
    .short 2, 183
    .long 1
    .quad 0x8000000000
    .quad 64, 0
    .long 0
    .short 64, 56, 1, 0, 0, 0
    .long 1, 5
    .quad 4096, 0x8000000000, 0
    .quad __user_blob_end - __user_blob_start
    .quad 4096
    .quad 4096
    .balign 4096
.global __user_blob_start
__user_blob_start:
    // Ask who we are. With Linux linked in this is answered by Linux's own
    // sys_getpid, on nk. Without it, by nk's two-entry table, which does not
    // implement 172 and says so.
    mov     x8, #172                // __NR_getpid
    svc     #0
    mov     x19, x0

    // Then something the kernel must refuse: 0x40080000 is the kernel's own
    // image. Whoever answers, a user pointer into kernel memory has to come
    // back as an error rather than as the kernel's first instructions.
    mov     x0, #1
    movz    x1, #0x4008, lsl #16
    mov     x2, #8
    mov     x8, #64                 // __NR_write
    svc     #0
    neg     x20, x0
    cmp     x20, #14
    b.ne    9f

    // Pointer-bearing calls without a marshaller must never enter LKL.
    mov     x8, #56                 // openat
    svc     #0
    cmn     x0, #38
    b.ne    9f

    // The ELF file ends before this word; PT_LOAD's memory tail must be zero.
    adr     x1, __user_blob_start
    ldr     x0, [x1, #4000]
    cbnz    x0, 9f

    // And something it should allow.
    mov     x0, #1                  // fd 1
    adr     x1, 1f                  // buf, PC-relative
    mov     x2, 2f - 1f             // len
    mov     x8, #64                 // __NR_write
    svc     #0

    // Exit with the pid, so the answer is visible from outside.
    mov     x0, x19
    mov     x8, #93                 // __NR_exit
    svc     #0

    // Not reached. If exit ever returns, stopping here is better than
    // running into the string as instructions.
9:  mov     x0, #99                 // failed an isolation/BSS assertion
    mov     x8, #93
    svc     #0
0:  b       0b
1:  .ascii  "hello from EL0 -- this is user space, on nk.\n"
2:
.balign 4
.global __user_blob_end
__user_blob_end:
.global __user_elf_end
__user_elf_end:
.section ".text", "ax"


// nk_setjmp / nk_longjmp
//
// LKL asks the host for these by name: `jmp_buf_set` and `jmp_buf_longjmp`
// are two of its forty host operations, and it uses them where Linux would
// unwind -- to get out of a context that cannot return normally.
//
// Only the callee-saved registers, SP and the return address, which is all
// AAPCS makes the callee's problem: everything else the caller has either
// already saved or does not care about. No floating point, because the whole
// kernel is built -mgeneral-regs-only and there is none to save.
//
// nk_setjmp returns 0 when it is called and whatever nk_longjmp was given
// when it is returned to -- and the discipline that makes that safe is the
// same as C's: the function that called nk_setjmp must not have returned.

.global nk_setjmp
nk_setjmp:
    stp     x19, x20, [x0, #16 * 0]
    stp     x21, x22, [x0, #16 * 1]
    stp     x23, x24, [x0, #16 * 2]
    stp     x25, x26, [x0, #16 * 3]
    stp     x27, x28, [x0, #16 * 4]
    stp     x29, x30, [x0, #16 * 5]
    mov     x1, sp
    str     x1,       [x0, #16 * 6]
    mov     x0, #0
    ret

.global nk_longjmp
nk_longjmp:
    ldp     x19, x20, [x0, #16 * 0]
    ldp     x21, x22, [x0, #16 * 1]
    ldp     x23, x24, [x0, #16 * 2]
    ldp     x25, x26, [x0, #16 * 3]
    ldp     x27, x28, [x0, #16 * 4]
    ldp     x29, x30, [x0, #16 * 5]
    ldr     x2,       [x0, #16 * 6]
    mov     sp, x2
    // longjmp(buf, 0) must still look like a non-zero return from setjmp, or
    // the caller cannot tell the two paths apart. C requires this and it is
    // the one piece of the contract that is easy to leave out.
    cmp     w1, #0
    csinc   w0, w1, wzr, ne
    ret
