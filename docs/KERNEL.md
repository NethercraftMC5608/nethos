# nk — the NETHOS kernel

A kernel of our own that runs **unmodified Linux drivers**. It is a sibling of
NETHOS, not a replacement for anything in it: `scripts/build-x86.sh` and
`scripts/build-arm.sh` still produce images that boot Debian's kernel, and
nothing in `payload/`, `pkg/` or the image build knows this directory exists.

Read this before touching `kernel/`.

## Why not simply write drivers

Because nobody can. Modern hardware support is thousands of person-years of
other people's work, and a distribution that writes its own drivers per device
supports three machines forever.

So the kernel has to run driver code that already exists — and the way to do
that is not what people first assume.

## The one fact the whole design rests on

**There is no way to translate a Linux driver into another kernel's driver
model, and there cannot be a good one.** Linux has no stable in-kernel API, by
policy and on purpose. A driver is not written against a published interface;
it is welded to whatever the kernel's internals looked like that week. A
mid-sized driver reaches thousands of kernel-internal symbols, hundreds of
header macros, and several subsystem lifecycles that only exist inside Linux.

Every system that has actually succeeded at reusing Linux drivers did the
opposite of translating them:

| project | what it does |
| --- | --- |
| Genode `dde_linux` | USB, NIC, WiFi and framebuffer drivers on a microkernel |
| LKL (Linux Kernel Library) | the entire kernel linked in as a library |
| rump kernels (NetBSD) | BSD drivers as portable components — the non-Linux answer |

They keep the driver source **byte-for-byte unmodified**, compile it against
**Linux's own headers**, and reimplement the Linux internal API underneath it.
That reimplementation is the shim, and it is the real work of this project.

## The shim is discovered, not designed

This is the property that makes it possible for one person, and it is worth
stating plainly because the instinct is to do the other thing:

> You never sit down to implement "the Linux kernel API". You compile a
> driver, get a list of a few hundred undefined symbols, generate a stub for
> every one of them that panics when called, boot it, and implement only what
> it actually reaches at runtime.

For a first driver that is typically 60–120 functions, most of them three
lines. Every driver after the first reuses the great majority of what the
previous one forced you to write. `ldk report` is meant to make that number
visible, so the decision to attempt a new class of device is made against a
measurement rather than a feeling.

Compiling against Linux's real headers is what makes this cheap: every macro,
every `static inline`, every `container_of` and list-head operation comes from
the kernel tree for free. Only **out-of-line** symbols need implementing, and
those are exactly the ones a linker will name for you.

## Why aarch64 first

The Mac this is developed on is arm64 with Apple's hypervisor, so
`qemu-system-aarch64 -M virt,accel=hvf` runs nk **natively** — a build-and-boot
cycle is seconds. The x86 equivalent on this machine is TCG emulation and
roughly thirty minutes (see `nethos-kernel build --cross`, and the same reason
the image build is slow).

`virt` is also a small and completely documented machine: one flattened device
tree describes all of it, and GICv3 + the generic timer + PSCI + 32 virtio-mmio
transports is the whole platform. x86 means ACPI and legacy PCI interrupt
routing, which is a separate project rather than a port.

## Why Rust for the core and C at the boundary

The shim has to export plain C symbols to unmodified driver objects. Rust does
that natively with `extern "C"`, so the boundary costs nothing. And an MMU, a
frame allocator and a scheduler written solo in C is a year spent on memory
bugs rather than on a kernel.

`#![no_std]`, stable toolchain, no `build-std`, no nightly. The bare-metal
target `aarch64-unknown-none-softfloat` ships precompiled `core`, so the only
prerequisite is rustup.

## Layout

```
kernel/
  core/            the kernel proper — Rust, no_std. Ours, and separately licensed.
    src/boot.s     the arm64 image header, EL2->EL1, stack, BSS, vector table
    src/main.rs    rust_main, panic handler, halt
    src/uart.rs    PL011 and the print!/println! macros
    src/exceptions.rs  where every vector lands until IRQs have somewhere to go
    linker.ld      links at 0x40080000, which the image header agrees to
    src/dt.rs      the flattened device tree, parsed from the specification
    src/paging.rs  MMU: identity map, 1GB blocks, device vs normal memory
    src/frames.rs  4KB physical frames: bump, then a free list in the frames
    src/heap.rs    first fit, splitting and coalescing -- what kmalloc will use
    src/mmio.rs    register access in assembly. Read its header before using it.
    src/gic.rs     GICv3: distributor, redistributor, system-register CPU interface
    src/timer.rs   the virtual timer, and the tick
    src/sched.rs   kernel threads, preemptive round robin
    src/selftest.rs what the kernel checks about itself at boot
  ldk/             the Linux Driver Kit: fetch, compile, list undefined symbols,
                   generate stubs, report coverage           (Stage 2)
  linux/           the shim. GPL-2.0, kept in its own directory on purpose.
    emul/          implementations, grouped the way Linux groups its headers
    stubs/         generated panicking stubs, committed so diffs are visible
    drivers/       a manifest only — driver source is fetched, never vendored
```

The `core/` vs `linux/` split is the licence boundary as well as an
architectural one. See **Licensing** below.

## Build and run

```bash
brew install rustup && rustup default stable      # once
rustup target add aarch64-unknown-none-softfloat  # once
rustup component add llvm-tools                   # once, for llvm-objcopy

scripts/run-kernel.sh              # build and boot, serial on stdio
scripts/run-kernel.sh --debug      # no LTO, and panics you can read
scripts/run-kernel.sh --gdb        # wait for gdb on :1234
make -C kernel test                # the boot test
```

Quit QEMU with **Ctrl-A X**. There is no display and that is deliberate: nk
cannot draw, and an empty window only makes a working boot look like a failure.

## Stages

Each one ends in something that runs. Do not start the next until the current
one boots.

- **0 — done.** Reach Rust from the reset vector, own the exception table, and
  say so on the serial port.
- **1 — done.** Device tree, MMU, frame allocator, kernel heap, GICv3, the
  virtual timer, and preemptive round-robin threads. Two kernel threads
  alternate on a tick, neither of them yielding.
- **2 — done.** `ldk` compiles unmodified Linux drivers against Linux's own
  headers for aarch64 and reports what they need. virtio-blk: **108 symbols**.
  virtio-net: 207, of which **139 are new** — the other 68 came free.
- **3 — done.** Unmodified `virtio_mmio` + `virtio_blk` read a sector off a
  QEMU disk. 113 of the 154 symbols they ask for are implemented; the other
  41 are stubs that never ran.
- **4 — done.** `virtio_net` sends an ARP request and receives the reply:
  `10.0.2.2 is at 52:55:0a:00:02:02`. Needed a real vmemmap. `e1000` is
  untouched.
- **5** — decide with `ldk report`'s numbers whether USB, DRM or WiFi is worth
  attempting. Genode is funded and staffed and still does not do GPU.

And, separately from the driver stages, the beginning of the only road to a
desktop:

- **User space — started.** A program runs at EL0 in its own address space and
  makes Linux system calls. See below.

## Linux boots on nk, and answers system calls

```
[    0.000000] Linux version 6.12.0+ (gcc 14.2.0) #10
[    0.000000] Memory: 62328K/65536K available
[    0.000000] printk: legacy console [lkl_console0] enabled
[    0.000000] NET: Registered PF_INET protocol family
[    0.000000] io scheduler mq-deadline registered
[    0.000000] Btrfs loaded, zoned=no
[    0.000000] Run /init as init process

Linux is up on nk. Asking it something:

  getpid()  -> 1
  gettid()  -> 24
  getuid()  -> 0

Now the same question from EL0:
  entering EL0...

  the process exited with status 1
```

That last number is the whole thing. **A process running at EL0, in its own
page tables, executed `svc`; nk caught it; nk handed it to Linux; Linux's own
`sys_getpid` answered; and the answer came back out through the process's exit
status.** The chain from bare aarch64 to the Linux system-call ABI is closed.

Linux's memory is nk's frame allocator. Its threads are nk's scheduler. Its
locks are nk's semaphores and mutexes. Its clock is nk's timer. Its console is
nk's UART -- every line above arrived through `lkl_host_ops.print`.

### How small the interface is

`arch/lkl` is a real, maintained Linux architecture port. It is **5,168
lines** -- against 26,636 for `arch/um` and 179,127 for `arch/arm64` --
because its "hardware" is a struct of function pointers the host fills in.
Built for aarch64 with `-mno-outline-atomics` it produces `lkl.o`: 19.7MB
stripped, the entire kernel, with **two** undefined symbols, `lkl_bug` and
`lkl_printf`.

`kernel/lkl/nk-host.c` is 288 lines and supplies those two plus the struct.
`kernel/core/src/hostops.rs` is nk's half. For comparison,
`kernel/linux/emul/` -- the driver shim -- is about two thousand lines and
grows with every driver ported. **This does not grow at all.**

### The bug that mattered

Everything blocked, forever, with no output. The wait graph -- which is why
nk's semaphores now carry identities and the watchdog prints them -- showed a
state the code plainly could not produce:

```
sem 6   count 1    waiters 0b00010000
```

A semaphore holding a token, with somebody still waiting for it.

`Semaphore::down` blocks in the middle of its own critical section: it holds a
reference to the semaphore across a context switch while another task mutates
the same object through a reference of its own. Written with `&mut self` that
is aliasing undefined behaviour, and **the compiler acts on it** -- it keeps
`count` in a register across the switch and re-tests the stale value when the
task resumes. The task then blocks again on a semaphore that was raised while
it slept.

The fix is `AtomicI32`/`AtomicU64` and `&self`. The lesson is more general
than the bug: **a synchronisation primitive cannot be written with `&mut`.**
Exclusive access is exactly the thing it does not have, and Rust is entitled
to believe the signature.

### The others, in the order they were found

**LKL uses thread id zero as "nobody owns the CPU".** `lkl_cpu_get` tests `if
(cpu.owner && !thread_equal(cpu.owner, self))`. nk numbers tasks from zero, so
the boot task -- the first to take the CPU -- was indistinguishable from no
owner at all. `nk-host.c` offsets every id by one. Nothing reports a sentinel
collision.

**A counting semaphore must wake exactly one waiter.** Waking all of them and
letting the losers re-check looks harmless. It is not: LKL counts its own
sleepers, one `up` per sleeper, and every spurious wakeup re-enters the wait
loop and increments that count again.

**A timer callback must not run in interrupt context.** Linux's callback
re-enters Linux and Linux takes mutexes; a mutex nk cannot grant blocks the
caller, and blocking inside an interrupt handler marks the *interrupted* task
blocked and switches away from a stack halfway through an exception.

**Divide before multiplying.** `delta_ns * HZ / 1_000_000_000` overflows a u64
for a long timeout and the wrapped deadline lands in the past or the far
future. Neither fires, and a Linux whose clock has stopped reports nothing.

**Sixteen task slots is nk's number, not Linux's.** Linux creates two dozen
kernel threads before anything useful runs. Sixty-four now, and the waiter
bitmask widened to match -- a task whose slot is past the end of that mask can
be blocked and never woken.

**Linux's sections are load-bearing.** It builds tables *by section* and
refers to their bounds by symbols the linker script must define --
`__start_notes`, `__per_cpu_start`, `__start___ex_table`. Discarding one does
not give a missing symbol; it gives "relocation refers to a symbol in a
discarded section", which names the section and not the reason.

**FP and SIMD had to be enabled.** `CPACR_EL1.FPEN` is zero out of reset and
nk never needed it -- nk is built `-mgeneral-regs-only`. Linux's generic code
uses SIMD freely, and a `memcpy` is enough. nk does not save those registers
across a context switch, which is correct only while nothing holds live FP
state across one.

### What is true, and what is not, about the process boundary

Every launched EL0 process now has a dedicated nk scheduler thread and a
Linux thread-group leader. Its first LKL call is the existing private
`new_thread_group_leader` syscall (245); calling getpid first would instead
attach it as a thread of init. Linux `unshare(CLONE_FS | CLONE_FILES)` then
separates its descriptor table and filesystem context from init. PID equals
TID for these single-threaded processes, and the binding stays in host TLS
through syscall and scheduler switches.

`exit` records the status in nk, runs LKL's TLS destructor on the owning host
thread, and finishes that nk thread. The destructor invokes Linux `do_exit`
and Linux reaps the backing task. nk's joining parent can then reclaim the
user pages, private page tables, kernel stack and scheduler slot. The real
exit status currently belongs to nk; LKL's destructor uses `do_exit(0)`.
Linux wait4 status propagation is not implemented.

The scheduler saves/restores TTBR0 and IRQ state across switches. EL0 IRQs
save the full integer exception frame and user SP before entering the timer
handler. Kernel threads use the kernel root, so they cannot retain a reaped
process's mappings. Two concurrent ELF fixtures store different PIDs at the
same user stack address, survive timer preemption, and exit with their own
PIDs. The boot checks also verify independent descriptor tables and umasks,
ESRCH for both exited Linux PIDs, and that init survives both exits.

This reuses LKL's existing task lifecycle; it does not implement fork or
execve. LKL's memory and signal-handler sharing remain its host-task model;
user signals, FP/SIMD context, ordinary binary startup and child inheritance
from arbitrary nk parents still need integration.

And a limitation worth stating before it is discovered: the user/kernel
separation is nk's alone. Linux will not check a pointer for us. See
"Marshalling, and why a pointer cannot simply be passed" below.

### Filesystem-backed ELF bootstrap

`--lkl` now creates `/nk-init` in Linux's existing memory-backed rootfs,
closes it, reopens it, and reads it through Linux VFS calls. No filesystem
implementation was added to nk. The seed is an embedded ELF fixture; this is
not yet a disk-backed root or an externally supplied init binary.

nk validates ELF64 little-endian AArch64 ET_EXEC headers before mapping
PT_LOAD segments, copies file bytes, zero-fills the memory tail, preserves
write/execute permissions, and enters the file's entry point at EL0. It
rejects dynamic images, overlapping load pages, overflowing/truncated ranges,
kernel addresses and entries outside executable file data. Processes load at
`0x400000` now, where aarch64 links a non-PIE executable. The bootstrap stack
has empty argument and auxiliary-vector terminators, not the full libc startup
contract.

The fixture checks zero-filled memory, rejects a kernel pointer with EFAULT,
opens `/nk-init` by path and reads its own ELF magic back, checks that a
syscall nk has not described returns ENOSYS, prints via nk's checked console
write, and exits with Linux's getpid.

### Linux does its own user access, and there is no table

`arch/lkl` selects `UACCESS_MEMCPY`: it assumes kernel and user share one flat
address space, so `copy_from_user` is a `memcpy`. That is true of every host
LKL was written for and false of one that runs its processes at EL0 with their
own translation tables.

nk's first answer was a table. For each system call it forwarded, it recorded
which arguments were pointers, which direction the data travelled and how long
it was, then copied each buffer across itself. It worked, and it was the wrong
shape: a list of every system call a program might ever make, which is the
thing this project exists to avoid writing. A call with no entry was refused,
so the list of what to add next was written by whatever binary ran and hit a
wall.

**Linux already knows which arguments are user pointers.** It marks them
`__user` and reaches them through `copy_from_user`, on every architecture, for
all four hundred and fifty of them. `ldk lkl` patches `arch/lkl` to ask the
host rather than assume:

```c
unsigned long lkl_copy_from_user(void *to, const void *from, unsigned long n);
unsigned long lkl_copy_to_user(void *to, const void *from, unsigned long n);
unsigned long lkl_clear_user(void *to, unsigned long n);
```

`useraccess.rs` answers them: `AT S1E0R` and `AT S1E0W` to translate with
EL0's permissions, a page at a time, refusing anything that names kernel
memory. Three functions, and every system call works for the same reason it
works on real hardware. `syscall.rs` is now a pass-through and has nothing to
keep up to date.

Three things this depends on, in order of how much they cost to get wrong:

**The live `TTBR0` is the calling process's.** Linux runs a system call on the
thread that made it, and nk restores each task's `TTBR0` when it schedules it,
so whenever this code runs on a process's behalf that process's tables are
installed -- including in the middle of a call that blocked and came back.
The exception is Linux touching user memory from some *other* task, a
workqueue finishing asynchronous work say; that fails with EFAULT rather than
reading the wrong process, because `AT S1E0R` against the wrong tables does
not find the address. Nothing a program has run here has needed it.

**The same three functions serve two callers.** A program at EL0, whose
pointers must be checked; and nk itself, which calls into Linux with kernel
buffers to seed the rootfs and read an ELF. A per-task flag says which, set
only around a forwarded call, and it has to be saved and restored rather than
just set -- `execve` reads a file through Linux while itself serving a user
syscall.

**`clear_user` is on the ordinary path, not an edge case.** Linux uses it to
zero the tail of a partially read page and the unwritten part of a structure.

The fixture guards the boundary that matters: it asks `openat` to open the
kernel image and requires EFAULT, which goes down Linux's own
`strncpy_from_user` and back out through `useraccess.rs`.

### The other half of the ABI: whose constants?

Getting Linux to read the pointer is not the same as agreeing what it means.

`busybox ls /` failed with "can't open '/': Invalid argument" and nothing
anywhere mentioned a flag. arm64 **overrides** four of the open flags that
`asm-generic` defines -- `O_DIRECTORY` is `1 << 14` on arm64 and `1 << 14` is
`O_DIRECT` in `asm-generic` -- and `arch/lkl` uses the generic ones. So a
program opening a directory was asking LKL for direct I/O, on a filesystem
that has none, and getting EINVAL.

The fix belongs in the kernel, not in nk. Translating constants per system
call is a table that has to be right for every call that ever takes a flag --
the same shape of mistake as the marshalling table. `ldk lkl` copies arm64's
`uapi/asm/fcntl.h` into `arch/lkl` so the kernel's ABI *is* the one the
binaries were compiled against, once, for all of them. A real `arch/lkl` for
aarch64 would do exactly this.

Only that file. arm64's other `uapi/asm` headers describe structures LKL
defines for itself -- `ptrace`, `sigcontext` -- and copying those would
replace working definitions with ones for hardware LKL does not have.

### busybox

```
$ scripts/run-kernel.sh --lkl --initrd busybox.cpio
  execve: replaced this process with 1975064 bytes at 0x400680
total 692
drwxr-xr-x    2 0        0                0 Jan  1 00:00 bin
drwxr-xr-x    2 0        0                0 Jan  1 00:00 dev
drwxr-xr-x    2 0        0                0 Jan  1 00:00 etc
-rwxr-xr-x    1 0        0           705024 Jan  1 00:00 nk-init
  the process exited with status 0
```

Debian's `busybox-static`, unmodified, started by nk's `execve`, listing
Linux's rootfs. It is the useful test precisely because it is indifferent to
us: it makes whatever calls it needs and reports an errno when one is
missing. With the table gone it does not meet a wall at all -- `prctl` and
`ioctl`, which had been printing "no descriptor yet", simply work.

### A program gcc compiled, running on nk

`kernel/init/hello.c` is ordinary C. It is built with `gcc -static -O2`
against ordinary glibc, by a compiler that has never heard of nk, and it is
not modified in any way:

```
$ docker run --rm -v "$PWD:/w" -w /w nethos-ldk \
      gcc -static -O2 -o kernel/ldk/build/nk-hello kernel/init/hello.c
$ scripts/run-kernel.sh --lkl --init kernel/ldk/build/nk-hello
  ...
  hello from a real compiled binary, on nk.
    argv[0] is /nk-init, argc is 1, and the heap works too.
  and its destructor ran on the way out.
  the process exited with status 7
```

`printf` with arguments, `malloc`, `argv` read off the stack nk built, a
destructor, and a return from `main` through glibc's `exit`.

That is the thing the whole project is for. Everything before it ran code
written for nk; this ran a Linux binary because nk answers Linux's numbers
with Linux's meanings. `--init FILE` embeds the binary in the kernel image --
the rootfs is memory-backed and there is no disk to read one from yet.

Getting there needed, in the order the binary asked for them:

- **PT_TLS accepted, not refused.** The loader rejected it as unsupported,
  which rejected every static binary gcc produces. It should never have been
  on that list: a static glibc sets its own thread pointer from its own
  program headers, so the loader's whole part in TLS is to map the segment --
  which PT_LOAD already covers -- and report AT_PHDR correctly.
- **A stack measured in pages, not one page.** A libc sets up TLS, tunables,
  locale and stdio before it reaches `main`; 256KB, mapped up front, because
  nk has no fault handler that could yet tell a growing stack from a wild
  pointer.
- **`mprotect`.** A libc makes its relocated GOT read-only at startup --
  GNU_RELRO -- and without this the binary stops with glibc's own message:
  "cannot apply additional memory protection after relocation".
- **`set_tid_address`, `prlimit64`.** Answered by nk. The first because its
  argument is a user address Linux would store flat and later write through;
  what the caller uses is the *return*, and that is the real tid. The second
  because telling a libc the truth about the stack it was given beats
  -ENOSYS, which makes it assume a default it will not get.
- **`set_robust_list` and `rseq` refused.** Both are optimisations a libc
  asks for and does without. -ENOSYS is honest; pretending to have registered
  a robust list nk would never walk is not.
- **`brk` returning what was asked for.** It was returning the page it
  rounded up to. Linux tracks the break at byte granularity even though it
  maps whole pages, and a libc told it got more than it asked for hands the
  difference out twice.
- **`TPIDR_EL0` saved across a context switch.** nk never reads the thread
  pointer, which is exactly why it was missed: it belongs entirely to EL0, so
  nothing in the kernel notices it being wrong. A libc puts `errno`, the
  malloc tcache and the locale behind it, so leaving one process's value in
  place while another runs gives the second process the first one's heap
  bookkeeping -- and the crash lands in malloc, some distance from the switch.

### The register that was not zero

For a while a real binary could run `main` but not return from it. Returning
sent glibc into `exit` and control arrived back at `_start`, where the program
re-entered itself and died writing `__libc_stack_end` -- read-only by then,
because RELRO had been applied on the first pass.

**`x0` at process entry is `rtld_fini`.** The aarch64 ABI says so: a function
the dynamic loader wants run at exit, and zero when there is none. Linux
clears every register on `execve` for exactly this reason. nk's `enter_user`
took the entry address in `x0` and never cleared it, so glibc read `_start`
out of `x0`, registered it with `__cxa_atexit`, and called it on the way out.
The program was doing precisely what it was told.

Nothing in the failure pointed at the entry path. What found it was
`run-kernel.sh --trace`, which is worth keeping for the next one: `-d
exec,nochain` with `-dfilter` bounded to the binary's own address range logs
every basic block executed **at EL0 and nothing else**, which is the one thing
a kernel cannot see into by itself. Adding `cpu` to the log gives the register
file before each block, and the value `0x400600` sitting in `x27` at the
`blr` inside `__run_exit_handlers` -- one instruction after glibc's
`PTR_DEMANGLE` -- named the bug in a line.

Two notes on the tool. QEMU's own documentation offers `-dfilter start-last`;
this build rejects it with "Invalid range" and wants `start+size`. And the log
file is created lazily on the first matching write, so a filter that matches
nothing is indistinguishable from a filter that is not being passed at all --
which cost a run to notice, because the `--trace` block had been placed above
the line that *creates* the argument array it appends to.

Every register is cleared now, not just `x0`. The rest are kernel state, and
handing them to EL0 is a leak whether or not anything reads them.

### A userland that is not part of the kernel

`--init FILE` embeds a binary in `nk.bin`, which is fine for one program and
useless for a userland: a real one is many files. `--initrd FILE` is the
answer, and it is the ordinary Linux one.

A cpio archive is what an initial ramdisk is, because cpio is the format you
can unpack without already having a filesystem: a flat stream of (header,
name, data), no index, no compression, no seeking. QEMU's `-initrd` leaves it
in RAM and names the range in `/chosen/linux,initrd-start` and `-end`, which
nk already had a device-tree parser for. `kernel/core/src/cpio.rs` reads it
and `user::unpack_initrd` writes it into Linux's rootfs through the same VFS
syscalls everything else uses -- open, write, mkdir. No new filesystem, no new
driver.

```
$ (cd root && find . | cpio -o -H newc > ../initrd.cpio)
$ scripts/run-kernel.sh --lkl --initrd initrd.cpio
  initrd: 0x48000000..0x480ac600 (689 KiB)
  ...
  initrd: 3 entries unpacked into Linux's rootfs
  rootfs: /nk-init came from the initrd (705456 bytes)
  hello from a real compiled binary, on nk.
    argv[0] is /nk-init, argc is 1, and the heap works too.
    and /etc/nk-greeting says: a userland that is not part of the kernel image
```

Three things this had to get right, none of them obvious:

- **The initrd's pages must be reserved.** It sits in RAM like everything
  else and nothing else knows it is there. Handing them to the frame
  allocator does not fail anywhere near the initrd -- it fails later, in
  whatever was given the page, with the archive's bytes in it.
- **The `/chosen` properties are not a fixed width.** They carry as many
  bytes as the address needs, so both four and eight are ordinary; a parser
  that assumes one works on one machine and reads rubbish on the next.
- **Parents are created as they are met, not assumed.** A cpio archive
  usually lists a directory before its contents and is not required to, so
  `mkdir` treats "already there" as success -- which it will be, constantly.

An initrd's `/nk-init` wins over the built-in fixture. That also made
"is this nk's own test program?" a *runtime* question rather than a build-time
one, which it always was: the self-checks that follow a run -- that a process
exits with its own Linux pid, that it spun long enough to be preempted -- are
claims about the fixture and not about an arbitrary binary.

### execve, and why the order is the whole difficulty

`execve` is nk's, not Linux's. LKL has no user space to exec into --
forwarding it would ask Linux to replace an address space it does not have --
so nk reads the file through Linux's VFS and does the replacing itself.

Two orderings have to be right, and neither is arbitrary:

**argv and envp are copied before anything is built.** They live in the
address space being replaced: an array of pointers, each into user memory,
with no length anywhere -- the array ends at a NULL and each string at a NUL.
This is the shape the marshalling table cannot describe, so it is walked, one
`copy_from_user` per pointer and one per string, bounded at 64 entries and
16KB because a process that asks for a million arguments should be told no
rather than answered.

**The old address space is freed only once `TTBR0` points at the new one.**
The kernel is mapped through the same tables as the process -- that is what
`new_address_space` copying three levels is for -- so a process that frees its
own address space before leaving it does not survive to report the mistake.

The consequence of building first is the thing execve actually promises: a
failed one leaves the caller with everything it had. `exec-parent.c` checks
that by execing a path that does not exist, confirming `ENOENT`, and carrying
on to exec the one that does.

The kernel stack is wound back before the new program starts. `execve` is
called from inside a syscall and never returns through it, so the exception
frame, the handler and the loader beneath it are all dead the moment the new
image runs; leaving them there leaks the stack for the life of the task, which
one exec would not notice and a shell would. `enter_user_fresh` is
`enter_user` with `mov sp, x3` in front of it.

`TPIDR_EL0` is cleared too. It belonged to the program that is gone, and the
memory it pointed at has just been freed.

### fork, and wait4

`fork` is nk's because the address space is, and `wait4` is nk's because the
exit status is -- a process's status is recorded when it calls `exit`, and
Linux's own task is torn down with `do_exit(0)` underneath.

The child is a copy of the parent **at the instruction it forked on**, so it
is entered by restoring a saved exception frame rather than by jumping to an
entry point: `resume_user` is `el0_sync_entry`'s exit path with the frame
passed in rather than found on the stack, and `x0` set to zero, which is what
`fork` returns in the child.

Three things worth writing down:

**The copy is a real copy.** Copy-on-write is the obvious improvement and it
needs a fault handler that can tell a write to a shared page from a wild
pointer, which nk does not have -- and getting that wrong turns a bug in one
process into silent corruption in another. Permissions are copied with the
pages, so the child's text stays executable and its RELRO stays read-only.
Only the process's own pages: the first gigabyte is shared with the devices,
and those level-2 entries are *blocks*, so following one as though it were a
table turns a fork into freeing the kernel's memory one entry at a time.

**`fork` returns the child's pid, and only the child can obtain one.**
Attaching to Linux binds the task to the host thread doing the attaching, so
the parent blocks on a semaphore until the child has published its pid. That
is a real serialisation and it is the honest one: the alternative is inventing
a pid before Linux has agreed to it.

**`clone` is a menu and nk implements one column of it.** Sharing memory makes
a thread and sharing nothing makes a process; anything asking for `CLONE_VM`,
`CLONE_FILES` or `CLONE_THREAD` is refused with ENOSYS rather than quietly
given its own memory, which would look like it worked until two threads
disagreed about a variable.

A wait status is not an exit code: the low byte says how the process died and
the second says with what, so a normal exit is the code shifted up by eight.
A libc's `WEXITSTATUS` undoes exactly that and gets nonsense from a plain code.

**The child inherits its parent's descriptors, and not through LKL.** LKL's
`new_host_task` clones every task from LKL's own init, never from the caller
-- `kernel_thread()` clones `current`, and it switches to `host0` first -- so
a forked child started with an empty table no matter who forked it.

Rather than patch that, nk uses the syscalls Linux already has for reaching
into another process's table: `pidfd_open` to name the parent and
`pidfd_getfd` to pull each descriptor across. It is what a debugger or a
container runtime does, it needs no kernel change, and it copies the *file
description* rather than opening the file again -- so parent and child share
the offset, which is what fork means and what stops two processes appending to
the same log from overwriting each other.

`pidfd_getfd` allocates the lowest free descriptor, which is not the number
the parent used, and the number is what a program depends on; so each is moved
into place with `dup3`. That cannot dislodge one already placed, because
descriptors in use are not free and the lowest free is therefore never one of
them. The copy happens before the parent is told the child exists, so the
parent cannot close a descriptor in between.

There is no syscall for "list the open descriptors", so it is a scan of the
first sixty-four.

### A console Linux owns, and a shell

nk answered writes to descriptors 1 and 2 itself: it looked at the number, and
if it was 1 or 2 the bytes went to the PL011 without Linux being told. That
works for a program that prints and is exactly wrong for a shell, because
`echo x > /tmp/out` is a `dup2` of a *file* onto descriptor 1 and nk would
have gone on writing to the UART. Descriptors have to mean what Linux says
they mean, which means the console has to be a file.

`arch/lkl/drivers/nk-console.c` is a real tty driver, and a small one, because
everything hard about a tty -- line discipline, canonical mode, echo, job
control -- is Linux's and already written. What it adds is a way out
(`lkl_ops->print`, the host operation LKL already uses for printk) and a way
in (an interrupt nk raises when a key arrives, and a host call to collect it).
`/dev/console` reaches it through `struct console.device`: Linux's
`console_device()` walks the registered consoles and asks each for the tty
driver behind it, and LKL's own console has none.

Input is decoupled the way every LKL device is, and for a reason: a key
arrives in an interrupt, and Linux cannot be called from there -- LKL's kernel
runs under a lock nk does not hold. So nk buffers the byte and raises an
interrupt; Linux's handler calls back to collect it. The ring is nk's memory
and nothing in Linux touches it, which is what makes `nk_console_read` safe to
call from Linux's interrupt context.

Each process opens `/dev/console` onto 0, 1 and 2 before it enters EL0, and nk
only answers by descriptor number for a process that has none.

**One thing had to be fixed in LKL for a shell to fork more than twice.** LKL
makes every host task with `kernel_thread()`, switching to `host0` -- its own
init -- to do it. `kernel_thread()` returns `-ERESTARTNOINTR` when a signal is
pending on the caller, which is right for a task that will handle the signal
and retry. `host0` never handles anything: it is a kernel thread with no
signal handling at all, so one pending signal is pending for ever and no host
task can be created again. busybox reported `can't fork: Unknown error 513`.
`patch-lkl.py` drops them, which is safe for the reason it is necessary --
nothing reads them.

With all of that, `busybox sh -c 'echo redirected > /tmp/out; busybox ls -l /;
busybox cat /tmp/out'` runs: a shell, three children of its own, a file it
created, and output sent somewhere other than the console and read back.

**The patches to `arch/lkl` live in `kernel/ldk/patch-lkl.py`**, one function
each with its reason. They are all the same kind of change: `arch/lkl` was
written for a host that is a Unix process, and nk is a host that is a kernel
with hardware. Where the two disagree the fix belongs in the architecture
port, not in a translation layer on nk's side.

### An init that is a shell script

A script is not a thing a loader can enter: the file names the program that
can read it. Linux resolves that in `binfmt_script`, and nk follows the same
rules -- the first line only, bounded at Linux's own 256 bytes, the first word
is the interpreter and *everything after it is one argument* however many
spaces it contains, four levels of nesting at most.

The part that matters is the part that is easy to miss: **`argv[0]` is
discarded and the script's own path becomes the interpreter's first
argument.** That is exactly why a copy of busybox at `/nk-init` exits 127 --
busybox chooses its applet from `basename(argv[0])`, nk passes the path it
loaded, and there is no applet called `nk-init` -- and why `#!/bin/busybox sh`
says what was meant. nk follows `#!` for the init as well as for `execve`, so
an initrd can ship a script and no part of nk has to know what busybox is.

```
#!/bin/busybox sh
echo "hello from a shell script, on nk"
echo "argv0 is $0, and I am pid $$"
busybox ls -l /bin
echo written > /tmp/from-script
busybox cat /tmp/from-script
```

```
hello from a shell script, on nk
argv0 is /nk-init, and I am pid 26
total 1932
-rwxr-xr-x    1 0        0          1975064 Jan  1 00:00 busybox
written
```

### Input, and a shell that reads it

```
$ printf 'hello there\nquit\n' | scripts/run-kernel.sh --lkl --initrd sh.cpio
nk shell ready
you typed: hello there
you typed: quit
goodbye
```

The path is: the PL011 raises its receive interrupt, nk takes it on the SPI
the device tree names and buffers the bytes in a ring of its own, a thread
raises LKL's console interrupt, and the driver's handler asks nk for what it
has and hands it to the tty.

Four things were wrong on the way, and three of them were the same mistake --
**data delivered before there was anything to receive it is data thrown
away**:

**A backgrounded command's standard input is `/dev/null`.** `run-kernel.sh`
backgrounds QEMU whenever `--timeout` is given, so with a timeout the console
was write-only and a key typed at it went nowhere. Nothing about that is
visible from inside nk: the PL011 simply never raises its interrupt.

**Clearing "anything stale" cleared something real.** `enable_receive` began
by writing `ICR`, which is the ordinary thing to do when enabling an interrupt
source. The PL011 raises the receive interrupt *once*, when the character
arrives, and the raw status is what remembers it -- so a character typed
before the kernel got that far had its interrupt discarded while the character
itself sat in the register, and the input then waited for a keystroke that had
already happened.

**A flip buffer with no tty takes every byte and delivers none.** The port is
where the line discipline hangs, and until an `open` has attached one there is
nowhere for the characters to go. `tty_insert_flip_string` accepted all eight
bytes and reported `tty=NULL`; they were gone. The driver collects nothing
until somebody has the console open, and the open drains the ring -- so keys
typed before init gets that far, which is *every* key when the input is a
pipe, wait rather than being handed over early.

**And raising LKL's interrupt takes LKL's CPU lock**, which a handler must not
do -- the lesson nk already learned with timers. The UART handler only buffers
and marks; a thread does the part that can block.

Worth recording how the last one was found, because two guesses came first and
both were wrong. Guess one: LKL's interrupt dispatch was not running because
every task was blocked inside a system call. Guess two: the pump needed to
enter LKL's CPU itself. The measurement that settled it took one line -- print
when Linux asks nk for input -- and it printed, which killed both guesses at
once and pointed at the tty. The next line printed what the tty did with the
bytes, and `tty=NULL` was the answer.

### Linux on nk driving real hardware

Until now every device Linux touched under `--lkl` was one LKL invented for
itself. This is the other thing: nk hands Linux a device that is really there,
at a real address, with a real interrupt.

```
  virtio: device 2 at 0xa003e00 -> Linux, GIC 79 as LKL irq 3
  ...
NETHOS-DISK-MARKER
```

That is busybox `dd` reading a disk, through Linux's block layer, through the
unmodified `virtio_blk` driver, over real MMIO, with real interrupts, on nk.

Almost all of the mechanism already existed and I had assumed it did not:

- **`lkl_host_ops` has `ioremap` and `iomem_access`.** nk is identity mapped
  and the devices are already Device memory, so `ioremap` returns the address
  and `iomem_access` is a load or a store of exactly the right width.
- **`arch/lkl` has a private `virtio_mmio_device_add(base, size, irq)`
  syscall.** It registers a platform device with those resources and Linux's
  ordinary `virtio_mmio` driver binds to it. There is no bus to enumerate and
  no device tree, so the kernel is simply told.

nk reads the transports out of its own device tree and checks each one's magic
and device id before registering it: QEMU's `virt` provides thirty-two and a
machine usually has two or three, so registering the empty ones would work and
would print thirty failed probes.

Two orderings had to be right, and both stopped the machine dead:

**The interrupt must be routed before the device is registered.** Registering
it probes it, and a probe that waits for the device to answer waits inside
that syscall. Enabling the line afterwards is a boot that stops with no
message, in a driver doing exactly what it should.

**A device interrupt must be masked until Linux has acknowledged it.** The
line is level-triggered: it stays asserted until the driver acknowledges it
*at the device*, and only Linux's driver can. nk cannot service it and must
not simply return, because the GIC offers it again immediately -- several
hundred thousand times a second, with the thread that would have told Linux
starved by the very interrupt it was trying to deliver. So it is masked on
arrival and unmasked once Linux has had it, which is what Linux itself does
for a threaded handler and for the same reason.

`lklirq.rs` exists because this is the third device to need it. Raising
Linux's interrupt takes LKL's CPU lock, and a handler must not take a lock --
the timer learned that, then the console, and now every virtio device. A
handler marks a bit; one thread raises them.

**It is not reliable yet.** The first read succeeds; a second, or a second
process reading concurrently, can hang. The suspicion is the unmask handshake
-- nk unmasks when `lkl_trigger_irq` returns, which is not the same instant as
Linux's handler having acknowledged the device -- but that is a suspicion and
the measurement has not been done.

### Graphics: what it took, and what is left

nk has a GPU. `virtio_gpu` is initialised on DRM minor 0, `/dev/dri/card0`
and `/dev/dri/renderD128` exist, the connector reports real modes, and
`kernel/init/fbtest.c` writes a gradient into `/dev/fb0` -- through `write()`,
because a DRM dumb buffer has to be mapped and a shared file-backed mapping is
not something nk can do yet.

The device arrives the same way the disk does: nk finds it in its own device
tree, hands it to Linux with `virtio_mmio_device_add`, and routes its
interrupt. What made it hard was that `CONFIG_DRM_VIRTIO_GPU` depends on
`MMU`, and everything below follows from turning that on.

**Linux needed virtual memory of its own**, which is written up above. Four
host operations, a window nk reserves before any process exists, and Linux's
linear map made identity with *real* physical memory -- because LKL's `__pa()`
is the identity and a device programmed with an address Linux invented reads
memory that is not there.

Then four separate obstacles, each invisible in its own way:

**The transport was legacy.** QEMU's `virt` builds its virtio-mmio transports
with `force-legacy` left at true, so they report version 1 and never offer
`VIRTIO_F_VERSION_1`. `virtio_blk` does not mind -- it speaks the legacy
protocol -- and `virtio_gpu` returns `-ENODEV`, on a path that is a
`pr_debug`. The device sits on the bus with `DRIVER_FAILED` and nothing
anywhere says why. `-global virtio-mmio.force-legacy=false` fixes it, and the
way to find it is to ask sysfs to bind the driver by hand: the write fails
with the probe's own errno.

**A reverted patch that had not reverted.** `CONFIG_LKL_MEMORY_START` was
still the six-gigabyte address from an experiment, because reverting it meant
deleting the code that *set* it -- and the LKL tree is a docker volume that
survives between builds. A patch there is a migration, not an edit; one that
only knows how to apply itself can never be changed afterwards.
`patch-lkl.py` now writes the values it wants every run.

**A patch in the wrong half of an `#ifdef`.** `VMALLOC_START` and `STACK_TOP`
were set in the `#ifndef CONFIG_MMU` branch of `pgtable.h`. With MMU the
values come from `pgtable-mmu-3level.h`, where vmalloc begins at
`memory_end + 8MB` -- on top of nk's identity map of RAM. The patch applied,
reported success, and changed nothing in effect.

**LKL's DMA ops are PCI's.** `config PCI` selects `ARCH_HAS_DMA_OPS`, which
makes `lkl_dma_ops` the implementation for every device -- and it calls
`lkl_ops->pci_ops->map_page` and casts the device to a `struct pci_dev`.
Legacy virtio never noticed, because it bypasses the DMA API and uses
`virt_to_phys`; a modern device uses it, and nk died at EL1 on a null
dereference. Without `CONFIG_PCI`, Linux uses dma-direct, which is exactly
right now that its linear map is identity with real physical memory.

**Turning the display on is one ioctl.** Writing to `/dev/fb0` fills a shadow
buffer; something has to set a mode on the CRTC before the device scans any of
it out, and QEMU reports "Display output is not active" until it does.
`FBIOPUT_VSCREENINFO` is how fbdev asks for that -- the DRM fbdev helper turns
it into `drm_fb_helper_set_par`, which does the modeset -- so `fbtest.c` sends
it after drawing, and the screen shows the gradient.

`fbcon` does the same thing on an ordinary Linux and was tried and backed out:
with `CONFIG_VT` it also takes the system console, so the kernel log stopped
reaching nk's serial port -- every test's only view of a boot -- and the guest
hung after drawing. One ioctl from userspace costs nothing and changes nothing
else.

`NK_MONITOR=/tmp/mon scripts/run-kernel.sh --lkl --gpu ...` then `screendump`
on that socket produces a picture of what the device is scanning out, which is
the only way to check this from outside the guest.

After that, Mesa needs two things nk does not have: **threads**
(`CLONE_THREAD` and `futex`) and **shared file-backed `mmap`**, since DRM
buffers are `MAP_SHARED` on `card0`. Then it is a rootfs problem -- `libdrm`
and Mesa, with `llvmpipe` for software rendering. Accelerated Mesa through
virgl remains a question about the *host*: `virtio-gpu-gl` and virglrenderer
need a working host GL context under HVF, and that is worth settling with an
experiment on QEMU alone before any of it involves nk.

Three flags exist because this was hard to see into: `--gpu`, `QEMU_LOG` for
the machine's own complaints about what the guest did, and `NK_MONITOR` for a
screenshot.

### A real filesystem, on real storage

```
root: ext4 mounted from /dev/vda
this file lives on a real disk
root: boot log so far:
a boot happened
```

Everything nk had written until now lived in a memory-backed rootfs and went
when the machine did. That is ext4 on a virtio-blk disk: nk finds the device
in its own device tree and hands it to Linux, Linux's own ext4 mounts it, and
a program at EL0 reads a file `mke2fs` put there on the host and appends one
of its own. The second boot reads what the first wrote, which is the only way
to tell persistence from a filesystem that merely worked.

The image is built with `mke2fs -d`, which populates a filesystem from a
directory without mounting anything or being root -- worth knowing, because
the usual way to build a root image needs both.

nk starts **one** init, the way a kernel does. It runs a second process only
for its own fixture, where the whole point is to check that two of them are
independent -- different Linux pids, separate descriptor tables, address
spaces that survive being preempted into each other. Those are claims about
nk, provable only against a program written to prove them, and running a
supplied init twice made every demo read oddly: two shells racing for the same
disk, two greetings, a spurious "another process has it".

The disk is mounted by the program nk runs, not by nk. A root filesystem
proper means `switch_root`, and that wants to be pid 1 in an initramfs, which
nk's processes are not: they are ordinary Linux tasks that nk attached. That
is a real distinction and this does not claim to have crossed it.

### What a real binary still cannot do

Signals, and any *shared* file mapping -- `MAP_SHARED` on a file is refused
rather than faked, because honouring it means writeback and a page cache
shared with Linux's. Private file mappings and threads both work; they have
their own sections below. Nothing survives a reboot unless it is on the
virtio disk: the rootfs itself is memory-backed.

### The process image: a stack, a heap, and mappings

**The initial stack is an interface nothing declares.** A libc's `_start`
takes no arguments; it reads argc, argv, envp and the auxiliary vector off the
stack at a layout the kernel is simply expected to have built. Get it wrong
and the program does not fail at a syscall nk could name -- it dereferences
whatever happened to be there. nk builds the real thing now: the strings, then
sixteen bytes for AT_RANDOM, then a sixteen-byte-aligned vector carrying
AT_PHDR (worked out from whichever segment contains the program headers),
AT_PHENT, AT_PHNUM, AT_PAGESZ, AT_BASE, AT_ENTRY, the four ids, AT_HWCAP,
AT_CLKTCK, AT_SECURE, AT_RANDOM and AT_NULL.

AT_HWCAP says nothing. Claiming no optional CPU feature is always safe;
claiming one nk has not enabled at EL0 -- FP, SVE -- is a trap the libc
springs on itself at its first instruction that uses it.

AT_RANDOM comes from Linux's generator with **GRND_INSECURE, and that is not
optional**: plain `getrandom` blocks until the CRNG is seeded, and on a
machine whose only entropy is a virtual timer it may never be. The first
attempt deadlocked the boot thread inside Linux with every other task idle,
which is precisely what the watchdog was built to report. GRND_INSECURE is
Linux's own answer -- bytes now, from a pool that says it is not trustworthy
yet.

**`brk`, `mmap` and `munmap` are nk's, not Linux's.** LKL is one flat region
with no user half at all, so forwarding them would move Linux's own break and
hand back an address the process cannot reach. The heap grows up from the
first page past the loaded image; anonymous mappings grow down from 16MB below
the stack. The two run out of room by *meeting*, which nk detects and refuses,
rather than by one silently landing on the other. A private file mapping (`MAP_PRIVATE` on a
descriptor) is read through Linux at map time and never written back, which
is exactly what a private mapping promises and is all a dynamic loader needs.
`MAP_SHARED` on a file is still refused rather than faked: honouring it means
writeback and a page cache that is nk's problem as well as Linux's, and
returning memory that does not contain the file is worse than returning
nothing.

`brk` reports failure the way Linux does -- by returning the old break, never
an errno -- because a libc that receives an errno here will not recognise it.
`munmap` over a hole succeeds, which is what makes it safe for a libc to call
over a range it is unsure of; the addresses are not reused, so a freed region
stays free while something might still hold a pointer into it. That wastes
address space rather than memory: the pages themselves go back.

The fixture checks all of it from EL0 -- walks its own argv and auxv, grows
the break and stores through it, maps a page and confirms it is zeroed, and
unmaps it -- so a wrong layout exits with a failure code rather than printing
a line that says it worked.

### The low half, and the global mapping that was blocking it

`USER_BASE` is `0x400000`. Getting there took three things, and the middle one
is the reason this has its own section.

**The devices had to stop occupying the whole first gigabyte.** `paging` mapped
`0..1GB` as one block because that was the cheapest thing that worked. The
machine has 34MB of devices, at `0x8000000..0xa200000`, so the map is now a
table of 2MB blocks covering only those. Narrowing it immediately exposed three
shim bugs that had been writing to address 0 and getting away with it: a
`cpumask_var_t` that was never allocated (`CPUMASK_OFFSTACK=y` makes it a
pointer), `alloc_netdev_mqs` never allocating `_tx`/`_rx`, and `free_skb`
freeing a head that belonged to `build_skb`'s caller. **A too-generous mapping
does not prevent bugs; it hides them.**

**Every mapping nk made was global.** With the devices out of the way,
`0x400000` was free -- and a process there faulted at level 2, on descriptors
that read back correct at every level in raw memory, with `AT S1E1R` agreeing
with the fault, **only under HVF**. Under `--tcg` the same kernel and the same
tables ran it to completion.

The cause was two defaults nk had never had reason to question. The `nG` bit
is off unless you set it, and a global translation is valid in *every* address
space regardless of ASID -- so the kernel's own constant walking of the low
half, where it is identity mapped, left entries that the process's
translations then collided with. And every address space used ASID 0, nk
flushing the whole TLB at each switch instead: slower, and it hides exactly
this, because with distinct ASIDs a stale entry from another address space
*cannot be used*, rather than being avoided by a flush somebody has to
remember to write. At 512GiB neither ever showed, because the kernel never
walks there.

User pages are `nG` now and each address space carries its own ASID in
`TTBR0[63:48]`. That also means a TTBR value and a table pointer stopped being
the same number, which is what `paging::table_of` exists to make unmissable:
dereferencing the register value reads memory at `asid << 48 | table`, and the
fault names the address rather than the mistake.

**Teardown had to change with it.** It used to free everything under the
process's top-level entry, which was safe only while that entry was the
process's alone. In the low half, L0[0] holds a *copy* of the kernel's L1 --
including a one-gigabyte RAM **block** at L1[1] that the old code would have
followed as though it were a table, freeing a gigabyte of kernel memory one
page-table entry at a time. It now frees only the three tables an address
space owns plus the leaf tables below them, and tells a table from a block
before following anything.

`TTBR1` is enabled too, and aliases the whole kernel for free: with `T1SZ` 16
the top regime translates bits [47:0] of a high address, which for
`PA | 0xFFFF_0000_0000_0000` *are* the physical address, so pointing `TTBR1`
at nk's existing identity tables costs nothing and no extra memory. Moving the
kernel to *run* from there is still right -- it would remove the copy of the
kernel's tables every address space carries -- but it is no longer what stands
between nk and an ordinary binary.

Validation: `python3 -m unittest discover -s tests -p 'test_kernel_elf.py'`
compiles the actual parser for host tests, including every truncated prefix
of a valid file. `test_kernel_boot.py` exercises the VFS-to-ELF-to-EL0 chain
and keeps the standalone and driver-shim boot paths covered.

### Threads, and a futex that had to be nk's

`pthread_create` is `clone` with `CLONE_VM|CLONE_THREAD|CLONE_SETTLS` and the
two `*_SETTID` flags, and `pthread_join` is a futex. Both are nk's, and the
futex is the more interesting of the two.

**The futex could not be forwarded to Linux.** A futex is an address two
threads agree on, and everything about that is a fact about the *address
space* -- which is nk's. Linux has no mapping for the address and could not
hash it into the same bucket for two threads even if it did, so forwarding
would give each thread a private queue and a `pthread_join` that never
returns. `kernel/core/src/futex.rs` keys on `(table_of(TTBR0), address)`
instead: threads share a page table so they share a key, processes do not so
the same numeric address in each is a different futex -- which is precisely
what `FUTEX_PRIVATE_FLAG` means. It is a linear scan of 64 slots, because a
handful of threads each waiting on one address is not a hash table's problem.

The wake/sleep window is the classic one: check the value, then sleep, and a
wake landing in between is lost. Interrupts are masked across both, a waker
sets the waiter's `woken` flag before making the task runnable, and
`block_on` restores interrupts only after the task is already marked blocked.

**The bug worth recording is the one that was not in the futex at all.** The
probe passed once, then failed every time the debugging prints came out --
the signature of a race that printing had been hiding. Instrumentation
recorded in the waiter rather than printed (a hang is exactly the state in
which nothing is printing) showed the joining thread reading `26` at an
address the exiting thread had provably written `0` to, in the same address
space, with the write returning success. Nothing about that is possible in
the order it appeared to happen -- which meant the order was wrong.

It was. glibc passes the *same* address for `parent_tid` and `child_tid`, and
nk's `clone` wrote the new thread id there after its handshake with the new
task, so a thread short enough to finish first had its `clear_child_tid` zero
overwritten by its creator writing the tid back. The join then waited on a
thread id belonging to a task that had already exited. Linux writes both tids
inside `copy_process`, before the child can run, and for this exact reason;
nk now has the child write them itself before it releases its creator and
before it reaches user code. The lesson is the general one: a value that must
be visible before a task runs cannot be written by whoever is waiting for
that task to start.

`CLONE_VM` also means the address space outlives the thread, so `sched` grew
`shares_mm` and reaping a thread no longer tears down page tables its
siblings are still executing from.

### Mesa, measured rather than estimated

Mesa is not one library. `libEGL` dlopens `libEGL_mesa`, which dlopens a
`_dri.so`, which dlopens `libgallium`, which needs `libLLVM`. Every arrow is
a runtime open, mmap, relocation and TLS allocation, so the first question is
not how big Mesa is but whether that mechanism works on nk at all.

It does. `kernel/init/drmprobe.c` dlopens `libdrm.so.2` -- a library that was
never on its link line -- resolves `drmGetVersion` out of it, and uses it to
make a real `DRM_IOCTL_VERSION` call against the virtio_gpu nk gave Linux.
What comes back is `virtio_gpu 0.1.0`, from the unmodified driver. Everything
Mesa needs from the loader is therefore present.

What is left is size, and it is worth stating exactly:

| piece | size |
| --- | --- |
| `libLLVM.so.19.1` | 118 MB |
| `libgallium-25.0.7.so` | 34 MB |
| `libdrm` + `libEGL` + `libgbm` + `dri` | under 1 MB |

`libgallium` has `libLLVM.so.19.1` in its `DT_NEEDED`, so llvmpipe cannot be
had without it -- `swrast_dri.so` is a 133KB stub that dlopens the real thing.
Against that, nk's rootfs lives in Linux's memory pool, which is 64MB, and
nk's private file mappings are read eagerly at map time rather than faulted
in. Neither number is a law: the pool is a constant and QEMU has a gigabyte.
But it does mean Mesa is now **two memory changes and no kernel features** --
which is a different kind of problem from the one this document has been
about, and the reason for measuring it before starting.

### What is next

Room for Mesa: a rootfs that is not in Linux's 64MB pool (the ext4 disk
already works; `switch_root` is the missing step) and file mappings that
fault in rather than reading whole segments up front. Then signals and
`MAP_SHARED` file mappings with writeback. Threads, dynamic linking, private
file mappings, futexes, `dlopen` and DRM access are done and covered by
tests; none of that establishes desktop compatibility or GPU acceleration.

## Historical symbol survey

The following survey is not a complete porting contract: unresolved symbol
counts exclude inline architecture code, configuration dependencies and
runtime semantics. The LKL host interface above is the implemented route.

### The 203 symbols: initial estimate

This measurement changed the plan, and it was made because the question was
asked twice and the answer given was wrong. It is recorded in full because it
is the most important number in the project.

**Take all of it.** Build Linux's `mm/`, `fs/`, `kernel/`, `block/`,
`net/core/`, `lib/`, `ipc/` and `security/` for arm64 -- 1219 objects, about
3.8 million lines, defining 15,899 symbols. Then ask what is still unresolved:

```
  unresolved after taking all of that:            932
  of which arch/arm64 supplies:                   203   <- the whole contract
```

**203 symbols is the entire interface between Linux and the machine it runs
on.** Everything above it -- the VFS, ext4, the page cache, the scheduler, the
network stack, the syscall layer, every binary's ABI -- comes for free once
those are answered.

And the 203 are not evenly weighted. Well over half are features that can be
declined outright:

| group | examples | needed for a desktop? |
| --- | --- | --- |
| hibernation | `swsusp_arch_suspend`, `arch_hibernation_header_save` | no |
| kexec / crash dumps | `machine_kexec`, `copy_oldmem_page` | no |
| hardware breakpoints | `arch_install_hw_breakpoint`, `hw_breakpoint_slots` | no |
| memory tagging | `mte_sync_tags`, `mte_invalidate_tags` | no |
| SVE / SME | `sve_set_current_vl`, `sme_do_dvmsync` | no |
| pointer auth | `ptrauth_set_enabled_keys` | no |
| 32-bit compat | `compat_arch_ptrace`, `aarch32_setup_additional_pages` | no |
| contiguous PTEs | `contpte_set_ptes` and eleven siblings | no, an optimisation |
| huge pages | `huge_pte_alloc`, `pmd_set_huge` | not at first |
| perf and stack walking | `perf_reg_value`, `arch_stack_walk` | no |

What is genuinely required is perhaps sixty functions, and nk already has the
hard ones in some form: page tables, a context switch, user address spaces,
`copy_from_user` through `AT S1E0R`, cache and TLB maintenance, an interrupt
controller, a timer.

**For scale**, the arch port that does exactly this and runs a full Linux
userland is `arch/um` -- User Mode Linux:

```
  arch/um   91 C and assembly files    26,636 lines
            87 headers                  4,033 lines
```

Thirty thousand lines, to unlock three point eight million.

### So the earlier answer was wrong

This document previously said a desktop needed "~200 syscalls with real
semantics" and would take years. That is the cost of *reimplementing* the
Linux ABI, which is what gVisor and Fuchsia's Starnix and FreeBSD's
Linuxulator do, and it is genuinely years -- gVisor implements 277 of 351
syscalls and is a funded team's multi-year project.

It is the wrong plan. **You do not implement Linux's ABI. You implement the
machine underneath it, and Linux implements its own ABI, as it already does.**
The shim in `kernel/linux/emul/` is that mistake in miniature, growing
sideways: every driver ported adds a few more Linux functions written by hand.
An arch port inverts it -- write the 203, get everything.

### What it costs, honestly

The trade is what nk *is*. With `arch/nk/`, Linux's memory manager and
scheduler replace nk's: `frames.rs`, `heap.rs` and `sched.rs` become the
backing for Linux's, or go. nk is then the architecture layer of a Linux
kernel -- boot, MMU, GIC, timer, context switch, user access -- plus
everything above the kernel, which was always the interesting part of NETHOS
anyway.

There is also a contract the symbol count does not show: the *header*
contract. `asm/pgtable.h`, `asm/thread_info.h`, `asm/ptrace.h` and their
neighbours define types and macros Linux's core compiles against, and there
are 4,033 lines of them in `arch/um`. And nk's core is Rust, while an arch
port is C compiled by kbuild -- so the parts of nk that would become
`arch/nk/` have to be C, or wrapped in it.

None of that is years.

## What can be borrowed, measured rather than argued

"Why write any of this -- why not take existing modules?" is the right
question to keep asking, and the answer is a number, not an opinion. `ldk`
exists to produce it. Compiling a file for arm64 and counting the symbols it
needs from outside itself:

```
  file                               defines  needs    KB
  net/ethernet/eth.o                      24     22    69
  fs/read_write.o                         55     35   197
  lib/vsprintf.o                          19     48   193
  drivers/gpu/drm/drm_gem.o                47     73   177
  mm/vmalloc.o                            56    114   477
  mm/page_alloc.o                         85    121   702
  fs/namei.o                             104    125   480
  drivers/net/virtio_net.o                  0    189   700
  kernel/fork.o                            51    200   370
  kernel/sched/core.o                     154    261   700
  net/core/dev.o                          260    279  1298
```

Nothing there is out of reach. The per-file cost of borrowing from Linux is
tens to a couple of hundred symbols, which is the same order as the drivers
already ported. **The instinct to borrow rather than write is correct, and
these numbers say so.**

The whole DRM core is the useful case to price, because it is what stands
between nk and a GPU. Built for arm64 it is 85 objects and 11.6MB; it defines
1232 symbols and needs 989, of which **427 come from outside DRM**. Three
times virtio-blk's 154 -- large, and not absurd.

The obstacle is not the number. It is *which* symbols:

```
  __arch_copy_from_user   __arch_copy_to_user   kern_unmount
  kill_anon_super         kobject_uevent_env    __folio_batch_release
```

`copy_to_user`. `kern_unmount`. `kobject_uevent_env`. DRM's entire purpose is
to serve ioctls from userspace; it mounts an internal filesystem for its
objects and reports them through sysfs. Porting it to nk would produce a
working interface **with no caller**, because the caller is Mesa, and Mesa is
userland.

So the real gate is not "can modules be borrowed" -- they can, and should be.
It is that a desktop needs *userspace*, and userspace is where borrowing stops
helping: the Linux ABI is not a library with an interface, it is the kernel's
entire observable behaviour, depended on in detail by binaries nobody is going
to recompile.

There is a premade answer even to that, and it should be known rather than
rediscovered: **LKL** links the whole Linux kernel as a library, and **rump
kernels** do the same for NetBSD under a BSD licence. Either would give nk
syscalls, a VFS and a network stack tomorrow. Both also mean the kernel
underneath is Linux, or NetBSD, and the part that is nk becomes the boot code
and the platform glue. That is a real and respectable design -- it is simply a
different project from this one, and worth choosing deliberately.

**What is reachable without any of it**: nk can talk to virtio-gpu *directly*,
with no DRM at all. The virtqueue code virtio-blk proved already works, and
virtio-gpu's 2D protocol is a short list of commands -- create a resource,
attach backing pages, set the scanout, transfer, flush. No DRM core, no
userspace, no Mesa: nk drawing to a screen by itself. That is weeks, not years,
and it is the next real milestone after the vmemmap.

## User space, and why it is the gate

```
  user:   108 bytes of program at 0x8000000000, stack at 0x8000100000, ttbr0 0x4000d000
  entering EL0...

hello from EL0 -- this is user space, on nk.

  the process exited with status 14
```

Everything nk had done until this point lived entirely inside the kernel and
was reachable only by nk's own code calling it. A desktop is not that. It is
several hundred existing binaries, compiled years ago against Linux's syscall
ABI, that nobody is going to recompile. **Nothing above the driver layer is
possible until a program can run at EL0 and be answered.** One now can.

The numbers are Linux's -- `write` is 64, `exit` is 93 -- because that is what
those binaries contain. Inventing a cleaner numbering would be inventing a
system nothing can be run on.

**That status of 14 is the interesting part.** It is `EFAULT`, and it is the
privilege boundary being demonstrated rather than asserted. The program asks
the kernel to write out eight bytes *of the kernel's own image*; the kernel
refuses, the program carries the errno to `exit`, and it is visible from
outside. A status of 0 there would mean the kernel had cheerfully printed its
own memory to whoever asked.

The refusal costs one instruction. `user_to_phys` translates a user pointer
with `AT S1E0R`, which asks the MMU to do the translation **as EL0 would** and
leaves the answer in `PAR_EL1`. A page the kernel can reach but the process
cannot fails there, which is the entire point of checking a user pointer
rather than dereferencing it -- a software table walk would have to
reimplement the permission rules to get the same answer. It is the smallest
honest `copy_from_user`, and it is also slow: one translation per byte.
Batching by page needs no new mechanism.

### What it cost, and the constraint that is still there

An exception from EL0 does **not** change `TTBR0`. So the first instruction of
the handler is fetched through the *process's* tables, and a table without the
kernel in it faults before anything can report why. Every address space
therefore starts as a copy of the kernel's top-level table -- three levels of
it now that processes live in the low half and share the first gigabyte with
the devices.

A kernel in `TTBR1`'s half needs none of that, and it remains the right
structural change. It is no longer a prerequisite for anything, though: see
"The low half, and the global mapping that was blocking it" above for what
actually stood in the way, which was not the address layout.

`SP_EL0` is the other cost, and it is Linux's design for Linux's reason. In
kernel mode it holds the current task, because that is where the stack-canary
lives for every Linux file the shim compiles (`-mstack-protector-guard-reg=
sp_el0`). In user mode it is the user's stack pointer. So it is saved into the
exception frame on the way in and put back on the way out.

### What is deliberately absent

One process. No `fork`, no `exec`, no ELF loader -- the program is a hundred
bytes of assembly in the kernel image, because nk has no filesystem to load
one from. No signals, no threads, no `mmap`, no scheduler involvement. Each of
those is a real piece of work and each is separable; what is here is the
mechanism they all attach to, and an unimplemented syscall now logs its own
number, which makes the list of what to do next something a real binary can
be asked to produce.

## Stage 4, and the vmemmap

```
  reply: 10.0.2.2 is at 52:55:0a:00:02:02
  52 54 00 12 34 56 52 55 0a 00 02 02 08 06 00 01
  08 00 06 04 00 02 52 55 0a 00 02 02 0a 00 02 02
```

Destination our MAC, ethertype `0806`, opcode `0002`, sender `10.0.2.2`.
`virtio_net.c` unmodified: it read its own MAC out of the device's
configuration space, transmitted through `ndo_start_xmit`, took its own
interrupt, ran NAPI and handed the reply up through `gro_receive_skb`.

**The wall was `struct page`, and it was predicted in writing.** `emul/mm.c`
said at Stage 3:

> the moment something *dereferences* a struct page -- reads a page flag,
> takes a reference, follows a mapping -- it faults on an address that is not
> mapped, and that is when the real vmemmap has to be built.

`receive_buf` calls `virt_to_head_page`, which reads `page->compound_head`.
virtio-blk never did: it only ever converted an address into a page and
straight back, so the pages it named never had to exist.

So they exist now. Eight megabytes of `struct page` for a 512MB guest,
allocated and mapped at the address Linux's own arithmetic chooses. Three
things had to be built for it, and each is worth having anyway:

- **`paging::map_normal`** -- the first mapping in nk of an address that is
  not simply itself. `virt_to_page` computes where the array is; nk does not
  get to choose.
- **`frames::alloc_contiguous_aligned`** -- a 2MB block descriptor has no room
  for the low bits of a physical address, so the hardware ignores them, and a
  block made from a misaligned address silently points somewhere else. There
  is no fault for this; it is asserted instead.
- **`nk_vmemmap_range`**, in C, using Linux's own `virt_to_page`. A second
  copy of that arithmetic in Rust would be a second chance to get it wrong,
  and getting it wrong is what cost Stage 3 its longest afternoon.

The address turned out to be `0x1ffc1000000` -- below 2^48, so it fits in
TTBR0 and nk still has no high-half mapping at all. That was luck rather than
design, and it will not survive user space.

Two other things Stage 4 established:

**HVF cannot run this port.** virtio-net makes an MMIO access QEMU's HVF
backend refuses to decode -- the same `assert(isv)` as the writeback load in
`mmio.rs`, from driver code this time rather than nk's. `--tcg` separated "nk
is wrong" from "the hypervisor cannot do this" in one run, for the second
time. Development of this port happens under TCG.

**A time-based wait is the wrong shape under TCG.** The virtual timer counts
guest cycles rather than following the host clock, so a three-second deadline
is around seven hundred real ones and is indistinguishable from a hang -- it
was diagnosed as one. The probe wait counts yields instead, which is the thing
actually being waited for.

## The old Stage 4 note, kept for the diagnosis

`virtio_net.c`, unmodified, now registers a `net_device`, is opened, brings up
its NAPI contexts, reads its MAC address out of the device's configuration
space -- **52:54:00:12:34:56**, which is QEMU's, so the read is real -- and
transmits an ARP request through `ndo_start_xmit`. 149 symbols implemented,
108 stubbed.

Then the receive path faults, in exactly the place `emul/mm.c` predicted in
writing:

> The limit is exact and worth knowing: the moment something *dereferences* a
> struct page -- reads a page flag, takes a reference, follows a mapping -- it
> faults on an address that is not mapped, and that is when the real vmemmap
> has to be built.

`receive_buf` calls `virt_to_head_page`, which reads `page->compound_head`.
Every `struct page` nk hands out is a computed address in a vmemmap that was
never allocated: fine while virtio only converts it back to a physical
address, which is all virtio-blk ever did, and fatal the moment anyone looks
inside one.

**The fix is known and is the next piece of work**: allocate a real `struct
page` array for RAM -- 8MB for this guest -- and map it at `VMEMMAP_START`.
`TTBR1` is enabled now (it was disabled outright via `TCR_EL1.EPD1` when this
was written), so the high half is available to map it into.

Two other things Stage 4 established:

**HVF cannot run this port.** virtio-net makes an MMIO access QEMU's HVF
backend refuses to decode -- the same `assert(isv)` as the writeback load in
`mmio.rs`, from driver code this time rather than nk's. `--tcg` was what
separated "nk is wrong" from "the hypervisor cannot do this", in one run.
Development of this port happens under TCG.

**A time-based wait is the wrong shape under TCG.** The virtual timer counts
guest cycles rather than following the host clock, so a three-second deadline
is around seven hundred real ones and is indistinguishable from a hang. The
probe wait counts yields instead, which is the thing actually being waited
for.

## What Stage 3 cost

The method worked exactly as advertised: link, boot, read the name of the stub
it stopped on, implement that, boot again. It stopped on `bus_register`, then
`execute_with_initialized_rng`, then `get_random_bytes`, then
`of_property_read_bool`, and so on. **113 of 154 symbols ended up implemented
and 41 are stubs that never ran** — the "implement only what it reaches" claim
in the plan, measured.

The bugs that were not that shape are the ones worth keeping.

**A data symbol defined as a function is unrecoverable.** An undefined ELF
symbol carries no type, so nothing says whether `virtio_check_mem_acc_cb` is a
function or a pointer to one. It is a pointer:

    extern bool (*virtio_check_mem_acc_cb)(struct virtio_device *dev);

Defining it as a function compiled and linked in silence, and the caller then
loaded the first eight bytes of its machine code and branched to them. The
fault reported an address of `0xd65f03c052800020`, which is `mov w0, #1; ret`
— `return true` — and pointed nowhere near the cause. `ldk syms` now reports
which symbols are never *called*, from the relocations, and generates a
pointer rather than a function for those. The same bug then reappeared as
`hex_asc_upper`, a character lookup table, which made every `%x` and every
negative `%d` in the kernel log print rubbish; that one slipped past the first
version of the check because a byte load uses a relocation type the check did
not list.

**Read the name of a callback, not what it looks like.**
`virtio_check_mem_acc_cb` asks whether the system *restricts* what memory a
device may reach — it is a question, not a permission. Answering `true`
("nothing to refuse", which is what it looks like it means) makes
`virtio_features_ok` demand `VIRTIO_F_ACCESS_PLATFORM` of every device and
reject them all with "device must provide VIRTIO_F_VERSION_1", which reads
like a fault in the device.

**arm64's `virt_to_page` does not use `virt_to_pfn`.** This one was the last
blocker and the least visible:

    virt_to_page(x) = VMEMMAP_START + ((x - PAGE_OFFSET) / PAGE_SIZE) * sizeof(struct page)
    page_to_pfn(p)  = p - vmemmap
    vmemmap         = (struct page *)VMEMMAP_START - (memstart_addr >> PAGE_SHIFT)

The first uses `PAGE_OFFSET`, the second uses `memstart_addr`, and they are
inverses only when the two agree. With `memstart_addr` at zero — the obvious
guess — `virt_to_phys` and `virt_to_pfn` are both *correct* and `sg_phys` still
comes out 2⁵² too high, because only the page round-trip is broken. The driver
then hands the device a descriptor pointing at an address that does not exist,
**the device reports success**, and the buffer is never written. Nothing fails;
the data simply does not arrive. `memstart_addr = PAGE_OFFSET` restores the
pairing. The test now prefills the buffer with `0xAA` rather than zeroes,
because a read that never happens leaves it untouched and zeroes are
indistinguishable from a disk full of them.

**The console had one failure mode, and it was the worst one.** `put` spun
unbounded on a full FIFO, so any console problem presented as the machine
stopping mid-line with no message — in the one device that reports every other
problem. It is bounded now, and writes anyway when the bound runs out: a
dropped character is a far smaller problem than a kernel that appears to have
died. `rust_exception` also writes a raw marker and the ESR through the
smallest possible path *before* it formats anything, and `nk` powers the
machine off through PSCI when it finishes, so "hung" and "finished" are no
longer the same observation.

### Open, and not understood

`hexdump` written with `print!("{:08x}")` stops the kernel dead after its first
few lines — no fault, no panic, the CPU idle in `wfi`, every later print lost,
**in the Linux-linked build only**. The identical `print!` calls work
everywhere else, including in the line immediately after it. It is not the
UART (removing flow control entirely changes nothing) and not an exception
(the raw marker in `rust_exception` never appears). The version in the tree
formats by hand and avoids it, which is the better thing for a memory-inspection
routine regardless — but the cause is unknown and this is a real bug.

## What Stage 2 settled

`kernel/ldk/` works, and two decisions in it are worth keeping.

**kbuild compiles the drivers, not us.** The obvious approach is to
reconstruct Linux's include paths and flags — `-I include`, `-I
arch/arm64/include`, `-D__KERNEL__`, and a dozen more — and drive the compiler
directly. That is a large and silent source of wrongness: a header found in the
wrong place gives a driver that compiles and behaves differently. `make
ARCH=arm64 drivers/virtio/virtio_mmio.o` uses exactly the flags Linux would, so
the question of whether we got them right never arises. It also means a port
manifest is four filenames and nothing else.

**The Linux tree lives in a container, in its own volume.** It cannot live on
macOS at all: Linux has filenames differing only in case, and a
case-insensitive APFS volume silently loses one of each pair on extraction. It
also cannot share `nethos-kernel`'s volume, which carries a dirty in-tree x86
build — an out-of-tree `O=` build refuses to start against an unclean source,
and the fix for that is `make mrproper`, which would destroy another tool's
working state without asking. `ldk fetch` does reuse that volume's downloaded
tarball rather than pulling 150MB again.

`pkg/npkg_elf.py` gained the reader. It already parsed `DT_SONAME`/`DT_NEEDED`
for packages; a relocatable object has no program headers and no `.dynamic` at
all, so the symbol path walks the *section* table and `.symtab` instead. Same
file, same reason it was written in the first place: the tool has to read its
own inputs without binutils.

The measurement, today:

```
  port           objects   needs   done   stub   todo
  virtio-blk           4     108      0    108      0
  virtio-net           4     207      0    207      0
                             139 new beyond the ports above
```

108 is what the plan predicted for a first driver, and the "new beyond" figure
is what makes Stage 5 a decision rather than a guess.

## What Stage 1 already cost

**MMIO through `read_volatile` is not safe on aarch64, and the reason
generalises.** `read_volatile`/`write_volatile` guarantee that an access
happens, once, in order. They do not guarantee *which instruction*. Three
volatile 32-bit accesses to nearby GIC registers were compiled to:

```
    ldr w13, [x10, #0x80]!
```

a load with pre-index writeback. The architecture defines `ESR_EL1.ISV` as 0
for a data abort on any load or store with writeback — the syndrome cannot
describe "and also update the base register", so the fault carries no
instruction decode at all, and a hypervisor trapping it has nothing to emulate
from. QEMU's HVF backend asserts outright; KVM is no better placed. **On real
hardware it works**, which is the worst failure mode available: correct until
the machine is virtualised.

`kernel/core/src/mmio.rs` therefore writes the accessors in inline assembly,
which is exactly why Linux's `__raw_readl`/`__raw_writel` have always been
`asm volatile` rather than a volatile pointer. Use them for every register
access; do not reach for a raw pointer.

Finding it needed all three instruments: `--tcg` proved the kernel's logic was
right and the hypervisor was the problem, a `println!` inserted anywhere in the
function made it vanish (which is the signature of a codegen artefact), and
`llvm-objdump` around the faulting address named the instruction. Guessing
produced two wrong answers first — the byte-wide priority write, and the timer.

**The physical timer is not available under a hypervisor.** The device tree
lists four timer interrupts and index 1 is the non-secure physical one, which
looks like the obvious choice for a kernel at EL1. Under HVF, EL2 belongs to
Apple's hypervisor, `CNTHCTL_EL2.EL1PCEN` is not set for guests, and `msr
CNTP_TVAL_EL0` traps — arriving as a synchronous exception with `EC` 0,
"unknown reason", which says nothing about what happened. nk uses the **virtual**
timer, index 2, INTID 27, which works bare-metal and virtualised alike. Linux
picks it for the same reason whenever it does not own EL2.

**A new task starts with interrupts masked.** Every task except a brand new one
resumes by returning through the IRQ epilogue, whose `eret` restores `SPSR_EL1`
and with it the interrupt mask. A new task is reached by `cpu_switch`'s plain
`ret`, so it inherits `DAIF` as the timer handler left it — masked, because the
CPU masks interrupts on exception entry. The symptom is precise and misleading:
the first thread starts, runs, and the machine stops. Nothing has crashed. It
is spinning with the only thing that could preempt it switched off. `task_start`
in `boot.s` clears `DAIF` before the task's first instruction.

**EOI before the context switch, not after.** `cpu_switch` does not return to
its caller; it returns into a different task. An interrupt EOI'd after it is
never EOI'd at all, and the GIC offers no further interrupt at that priority.
The symptom is a timer that ticks exactly once.

## What Stage 0 already cost, so nobody pays it twice

**QEMU passes no device tree to an ELF kernel.** Handed `-kernel nk` (an ELF),
QEMU loads the segments, jumps to the entry point, and stops caring: no
bootloader stub, no DTB, `x0` zero. This was not guessed — a scan of the first
2MB of RAM from inside the kernel found no FDT magic anywhere and nothing but
zeroes at the bottom of memory.

The fix is the 64-byte **arm64 Linux image header** at the top of `boot.s`
(`Documentation/arch/arm64/booting.rst`). With it, QEMU takes the Linux boot
path instead: it generates a device tree, places it in RAM, and enters with
`x0` pointing at it. `scripts/run-kernel.sh` therefore hands QEMU the flat
binary and keeps the ELF only for gdb's symbols.

`text_offset` is `0x80000` and the "load me anywhere" flag is clear, because nk
is not position-independent: `linker.ld` links at `0x40080000` and the header
is the agreement that it will be loaded there.

**Unaligned accesses fault before the MMU is on.** With the MMU off every
access is Device-nGnRnE, which does not permit them, and LLVM will emit them
for ordinary struct copies. Hence `-C target-feature=+strict-align` in
`.cargo/config.toml`. rustc warns that the feature is unstable and passes it
through anyway; the warning on every build is expected and is not a problem to
fix.

**Every CPU starts at `_start`.** QEMU starts as many as `-smp` asks for, all
at the reset vector. `boot.s` parks everything that is not affinity 0, because
otherwise several CPUs race to zero the same BSS. SMP bring-up is Stage 1's.

**Drop from EL2 while you are there.** `-M virt` gives EL1 today, but
`virtualization=on` and real firmware do not. The EL2 path in `boot.s` also
sets `CNTHCTL_EL2` and zeroes `CNTVOFF_EL2` — skipping those costs nothing now
and costs an afternoon at Stage 1, where the generic timer traps to EL2 and the
only symptom is that no tick ever arrives.

## Licensing, plainly

Linux driver source is GPL-2.0. Compiling it into nk makes the resulting binary
a combined work under GPL-2.0, and shipping an image built that way carries the
obligation to offer the source.

The `core/` and `linux/` split is the standard mitigation, and it is what
Genode does: the core is separable and independently licensed, and the shim
and every ported driver are GPL-2.0 and marked so.

Worth knowing before going further: **this route entangles NETHOS with the
Linux project more than shipping Debian's kernel binary does, not less.** The
only production alternative is rump kernels — NetBSD drivers as portable
components, BSD-licensed, no strings — at the price of far worse coverage of
modern hardware. Both are real choices. This one assumes Linux drivers.

## Working here

**Each build variant has its own target directory.** `NK_LKL_LIB` and
`NK_LINUX_LIB` are build-script inputs, so a plain kernel, a driver port and
the whole of Linux invalidate each other's builds. One directory means a full
rebuild at every switch, and with the suite running classes in parallel it is
worse than slow: they take the same cargo lock, and a class whose watchdog is
twelve seconds spends them waiting for a build it did not ask for. The plain
build keeps `kernel/target`, which is what a bare `cargo build` produces and
what gdb and the tests name.

**The suite used to take six and a half minutes and now takes eight seconds,
and none of it was the kernel.** Every class ran to its watchdog -- twelve
seconds for a run that takes one, ninety for one that takes a second -- and
the reason was a `sleep`.

`run-kernel.sh --timeout N` runs QEMU in the background and starts a watchdog
beside it, `( sleep N; kill ... ) &`. When QEMU exits the script kills the
watchdog, and killing a subshell does not kill the `sleep` inside it. The
orphan goes on holding this script's standard output -- a pipe, for anything
reading the run -- so a reader waiting for end-of-file waits the whole
watchdog after the machine has already switched itself off. The fix is
`</dev/null >/dev/null` on the watchdog, so it never holds the pipe at all.

**This was diagnosed wrongly for most of a day**, and the wrong diagnosis is
worth recording: `psci::poweroff` was blamed, and written up here as not
working under HVF. It works. A run that takes four seconds took four seconds;
it was the reader that waited thirty. The lesson is the ordinary one -- the
symptom was "the process does not end", and the process that would not end was
never measured, only the one that was interesting.

Three other things had to be right before reading nk's own end marker ended a
run, and each looked like the fix on its own:

- **`for line in proc.stdout` reads ahead.** Iterating a file object buffers
  several kilobytes, so on a pipe it hands back nothing until the buffer
  fills -- and a kernel's whole output is a few kilobytes. `iter(f.readline,
  '')` does not.
- **QEMU is a grandchild.** `run-kernel.sh` backgrounds it and waits, so
  terminating the shell leaves QEMU holding the pipe; anything that then
  reads to end-of-file waits for QEMU's watchdog, which is the wait being
  avoided. The harness gives the run its own process group and kills that.
- **Cargo's package-cache lock is global, not per target directory.** A dozen
  classes starting at once queue on it, and a class whose watchdog is twelve
  seconds can spend all twelve waiting for a build it did not ask for. The
  runner builds every variant serially first and the classes then boot what is
  there.

Getting the variants separated mattered for correctness, not only speed:
`NK_INIT` is a build-script input, so `--lkl` and `--lkl --init` are different
kernels, and while they shared a directory whichever built last won and the
other silently booted the wrong program.

`psci::poweroff` does switch the machine off, under HVF and under TCG, and
always did. A clean exit is what distinguishes "the kernel finished" from "the
kernel hung", which is the whole reason that code is there, and it has been
earning its keep the entire time.

**Run the tests with `tests/run-kernel-tests.sh`.** Every class boots QEMU
from scratch in `setUpClass`, so `unittest discover` is a dozen independent
boots run one after another -- six and a half minutes of a machine that is
idle for most of it. The script runs them in parallel and reports per class,
which is under two minutes and, more usefully, names the class that failed
instead of burying it in one very long log. `tests/run-kernel-tests.sh
Initrd` runs a subset; `KEEP=1` keeps the per-class output for reading.

The rule from `CLAUDE.md` applies more here than anywhere: **measure before
concluding.** The missing device tree above looked like four different bugs
before a memory scan settled it in one run. A kernel gives almost no feedback,
so the cheap instruments are worth building early — `--gdb`, the exception
dump, and `tests/test_kernel_boot.py`, which boots the real thing on the real
emulator and reads the serial console, because at this stage there is no other
kind of test.
