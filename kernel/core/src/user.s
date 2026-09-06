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

.global el0_irq_entry
el0_irq_entry:
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

    bl      rust_el0_irq

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
    tlbi    vmalle1is
    dsb     ish
    isb

    msr     sp_el0, x1              // the user stack
    msr     elr_el1, x0             // where to start
    mov     x3, #0                  // EL0t, DAIF clear: interrupts on in user
    msr     spsr_el1, x3

    // Every register zero, and x0 above all.
    //
    // The aarch64 process-entry ABI says x0 holds `rtld_fini` -- a function
    // the dynamic loader wants run at exit -- and that it is zero when there
    // is none. Linux clears every register on execve for exactly this reason.
    // nk was leaving x0 holding the entry address, so glibc registered
    // `_start` as an atexit handler and called it on the way out: the program
    // ran main, printed, and then re-entered itself, dying on a write to
    // __libc_stack_end that RELRO had by then made read-only. Nothing in the
    // failure pointed at the entry path, and the same argument applies to the
    // other thirty registers -- whatever nk leaves in them is kernel state
    // handed to user space.
    mov     x0,  #0
    mov     x1,  #0
    mov     x2,  #0
    mov     x3,  #0
    mov     x4,  #0
    mov     x5,  #0
    mov     x6,  #0
    mov     x7,  #0
    mov     x8,  #0
    mov     x9,  #0
    mov     x10, #0
    mov     x11, #0
    mov     x12, #0
    mov     x13, #0
    mov     x14, #0
    mov     x15, #0
    mov     x16, #0
    mov     x17, #0
    mov     x18, #0
    mov     x19, #0
    mov     x20, #0
    mov     x21, #0
    mov     x22, #0
    mov     x23, #0
    mov     x24, #0
    mov     x25, #0
    mov     x26, #0
    mov     x27, #0
    mov     x28, #0
    mov     x29, #0
    mov     x30, #0
    eret


// enter_user_fresh(entry, stack, ttbr0, kernel_sp)
//
// enter_user, with the kernel stack wound back first. This is execve's entry:
// it is called from inside a syscall and never returns through it, so every
// frame beneath -- the exception frame, the handler, the loader -- is dead the
// moment the new program starts. Leaving them there leaks the stack for the
// life of the task, which one exec would not notice and a shell would.
.global enter_user_fresh
enter_user_fresh:
    mov     sp, x3
    b       enter_user

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
    .quad 0x400000                  // e_entry: 4MB, where aarch64 links
    .quad 64, 0
    .long 0
    .short 64, 56, 1, 0, 0, 0
    .long 1, 5
    .quad 4096, 0x400000, 0         // p_offset, p_vaddr, p_paddr
    .quad __user_blob_end - __user_blob_start
    .quad 4096
    .quad 4096
    .balign 4096
.global __user_blob_start
__user_blob_start:
    // Every register must arrive zero.
    //
    // The ABI says x0 at process entry is `rtld_fini`, a function the dynamic
    // loader wants run at exit, and that it is zero when there is none. nk was
    // leaving the entry address in it, so glibc registered `_start` with
    // atexit and called it on the way out -- a real binary ran main, printed,
    // and then re-entered itself. The rest are checked for the same reason
    // Linux clears them: whatever the kernel leaves behind is handed to EL0.
    orr     x0, x0, x1
    orr     x0, x0, x2
    orr     x0, x0, x3
    orr     x0, x0, x4
    orr     x0, x0, x5
    orr     x0, x0, x6
    orr     x0, x0, x7
    orr     x0, x0, x19
    orr     x0, x0, x20
    orr     x0, x0, x29
    orr     x0, x0, x30
    cbnz    x0, 9f

    // Then: is the stack the shape a libc expects?
    //
    // Nothing declares this interface. `_start` takes no arguments and reads
    // argc, argv, envp and the auxiliary vector off the stack at a layout the
    // kernel is simply expected to have built -- so a real binary would not
    // fail here at a syscall nk could name, it would dereference whatever
    // happened to be there. Checking it is the only way to know it is right.
    ldr     x0, [sp]                // argc
    cmp     x0, #1
    b.ne    9f
    ldr     x1, [sp, #8]            // argv[0], which is "/nk-init"
    cbz     x1, 9f
    ldrb    w0, [x1]
    cmp     w0, #0x2f               // '/'
    b.ne    9f
    ldr     x0, [sp, #16]           // argv must be NULL-terminated
    cbnz    x0, 9f

    add     x2, sp, #24             // envp
12: ldr     x0, [x2], #8
    cbnz    x0, 12b                 // walk to its NULL; auxv follows

    mov     x3, xzr                 // AT_PAGESZ
    mov     x4, xzr                 // AT_RANDOM
13: ldr     x0, [x2], #8
    ldr     x1, [x2], #8
    cbz     x0, 14f                 // AT_NULL
    cmp     x0, #6                  // AT_PAGESZ
    csel    x3, x1, x3, eq
    cmp     x0, #25                 // AT_RANDOM
    csel    x4, x1, x4, eq
    b       13b
14: mov     x0, #4096
    cmp     x3, x0
    b.ne    9f
    cbz     x4, 9f
    // The stack guard has to be sixteen bytes of something. A constant here
    // would give every process on the machine the same canary, so all-zero is
    // the one answer that is definitely wrong.
    ldp     x0, x1, [x4]
    orr     x0, x0, x1
    cbz     x0, 9f

    mov     x0, #1
    adr     x1, 15f
    mov     x2, 16f - 15f
    mov     x8, #64
    svc     #0

    // Ask who we are. With Linux linked in this is answered by Linux's own
    // sys_getpid, on nk. Without it, by nk's two-entry table, which does not
    // implement 172 and says so.
    mov     x8, #172                // __NR_getpid
    svc     #0
    mov     x19, x0
    // Distinct values at the same user VA expose a wrong TTBR0 restore.
    str     x19, [sp, #-16]!
    // Long enough to be preempted several times, not once.
    //
    // This was 0x0200ffff, and across both processes the timer fired exactly
    // once -- so whether a given process had been interrupted depended on
    // which side of a 10ms tick it happened to run. The test that asserts
    // address spaces survive preemption then passed or failed on timing
    // rather than on the thing it was testing. 0x10000000 is about four
    // ticks per process on this machine and several on a slower one, which
    // is the direction an assumption like this should fail in.
    movz    x21, #0x1000, lsl #16
8:  subs    x21, x21, #1
    b.ne    8b
    ldr     x20, [sp], #16
    cmp     x20, x19
    b.ne    9f

    // Open this very file through Linux's VFS, read its header back, and
    // print it. Every pointer here -- the path, the buffer -- is a user
    // address that nk copies across rather than handing to Linux.
    mov     x0, #-100               // AT_FDCWD
    adr     x1, 3f                  // "/nk-init"
    mov     x2, #0                  // O_RDONLY
    mov     x3, #0
    mov     x8, #56                 // __NR_openat
    svc     #0
    mov     x22, x0
    tbnz    x22, #63, 4f            // negative: openat failed, skip

    sub     sp, sp, #64
    mov     x0, x22
    mov     x1, sp
    mov     x2, #16
    mov     x8, #63                 // __NR_read
    svc     #0
    mov     x23, x0

    mov     x0, #1
    adr     x1, 5f
    mov     x2, 6f - 5f
    mov     x8, #64
    svc     #0

    add     x1, sp, #1              // skip the 0x7f, print "ELF"
    mov     x0, #1
    mov     x2, #3
    mov     x8, #64
    svc     #0

    mov     x0, #1
    adr     x1, 7f
    mov     x2, #1
    mov     x8, #64
    svc     #0
    add     sp, sp, #64

    // Scatter and gather, which is what a libc actually uses.
    //
    // readv into two *disjoint* buffers proves the marshalling layer
    // distributes the kernel's one flat buffer back across the user's ranges
    // rather than copying it all to the first one; writev to the console
    // proves the gather side. Both are the nested case: the argument is a
    // pointer to an array of pointers, and every one has to be walked.
    mov     x0, x22
    mov     x1, #0
    mov     x2, #0                  // SEEK_SET
    mov     x8, #62                 // __NR_lseek
    svc     #0

    sub     sp, sp, #128            // 0..47 iovecs, 48 one byte, 56 three
    add     x9, sp, #48
    str     x9, [sp, #0]
    mov     x9, #1
    str     x9, [sp, #8]
    add     x9, sp, #56
    str     x9, [sp, #16]
    mov     x9, #3
    str     x9, [sp, #24]
    mov     x0, x22
    mov     x1, sp
    mov     x2, #2
    mov     x8, #65                 // __NR_readv
    svc     #0
    cmp     x0, #4
    b.ne    9f
    ldrb    w0, [sp, #48]           // the first iovec got only the 0x7f
    cmp     w0, #0x7f
    b.ne    9f

    // ...and the second got "ELF", printed here in one gather call.
    adr     x9, 10f
    str     x9, [sp, #0]
    mov     x9, 11f - 10f
    str     x9, [sp, #8]
    add     x9, sp, #56
    str     x9, [sp, #16]
    mov     x9, #3
    str     x9, [sp, #24]
    adr     x9, 7f
    str     x9, [sp, #32]
    mov     x9, #1
    str     x9, [sp, #40]
    mov     x0, #1
    mov     x1, sp
    mov     x2, #3
    mov     x8, #66                 // __NR_writev
    svc     #0
    mov     x9, 11f - 10f
    add     x9, x9, #4
    cmp     x0, x9
    b.ne    9f
    add     sp, sp, #128

    mov     x0, x22
    mov     x8, #57                 // __NR_close
    svc     #0
4:

    // A heap, and an anonymous mapping. These are nk's own answers: LKL is one
    // flat region with no user half, so forwarding brk or mmap would move
    // Linux's break and hand back an address this process cannot reach.
    mov     x0, #0
    mov     x8, #214                // __NR_brk, asking
    svc     #0
    mov     x24, x0
    cbz     x24, 9f
    add     x0, x24, #4096
    mov     x8, #214                // __NR_brk, setting
    svc     #0
    cmp     x0, x24
    b.eq    9f                      // brk reports failure by not moving
    movz    x1, #0xbeef
    str     x1, [x24]               // the page has to be there and writable
    ldr     x2, [x24]
    cmp     x1, x2
    b.ne    9f

    mov     x0, #0
    mov     x1, #4096
    mov     x2, #3                  // PROT_READ|PROT_WRITE
    mov     x3, #0x22               // MAP_PRIVATE|MAP_ANONYMOUS
    mov     x4, #-1
    mov     x5, #0
    mov     x8, #222                // __NR_mmap
    svc     #0
    tbnz    x0, #63, 9f
    mov     x26, x0
    movz    x1, #0xcafe
    str     x1, [x26]
    ldr     x2, [x26]
    cmp     x1, x2
    b.ne    9f
    ldr     x2, [x26, #8]           // and the rest of it must be zero
    cbnz    x2, 9f
    mov     x0, x26
    mov     x1, #4096
    mov     x8, #215                // __NR_munmap
    svc     #0
    cbnz    x0, 9f

    mov     x0, #1
    adr     x1, 17f
    mov     x2, 18f - 17f
    mov     x8, #64
    svc     #0

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

    // A call nk has no descriptor for must not reach LKL.
    //
    // This asserted that of `openat` until openat grew a descriptor, at which
    // point the program was asserting the absence of the feature that had
    // just been added. `mount` stands in now: it takes four pointers, nk does
    // not describe it, and forwarding it unmarshalled would hand Linux user
    // addresses it cannot safely hold. When mount is described, this line
    // moves to whatever is still undescribed -- the check is the rule, not
    // the number.
    mov     x8, #40                 // __NR_mount
    svc     #0
    cmn     x0, #38                 // -ENOSYS
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
3:  .asciz  "/nk-init"
5:  .ascii  "  /nk-init, opened and read from EL0 through Linux, begins: "
6:
7:  .ascii  "\n"
10: .ascii  "  and again by readv, gathered back out with writev: "
11:
15: .ascii  "  stack: zeroed registers, argc, argv, envp and a seeded auxv\n"
16:
17: .ascii  "  memory: brk grew and holds a value, mmap gave a zeroed page\n"
18:
.balign 4
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
