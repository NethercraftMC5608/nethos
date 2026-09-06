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
- **1** — device tree parsing, physical frame allocator, MMU on, kernel heap,
  GICv3, generic timer, threads and a round-robin scheduler.
  *Done when two kernel threads alternate on a timer tick.*
- **2** — `ldk`: compile a driver against Linux headers, list its undefined
  symbols, generate stubs, report coverage.
  *Done when `ldk syms virtio-blk` prints a clean list that links.*
- **3** — unmodified `virtio_mmio` + `virtio_blk`. Forces most of the shim that
  will ever exist: `printk`, `kmalloc`, `ioremap`, `request_irq`, spinlocks,
  wait queues, the device/driver model, `dma_alloc_coherent`, workqueues.
  *Done when nk reads a sector off a QEMU disk and prints it.*
- **4** — `virtio_net`, then `e1000`: a different class of device, and then a
  real vendor driver that does not cooperate. Forces `sk_buff`, netdev
  registration, NAPI, streaming DMA.
  *Done when nk answers an ARP request from the host.*
- **5** — decide with `ldk report`'s numbers whether USB, DRM or WiFi is worth
  attempting. Genode is funded and staffed and still does not do GPU.

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

The rule from `CLAUDE.md` applies more here than anywhere: **measure before
concluding.** The missing device tree above looked like four different bugs
before a memory scan settled it in one run. A kernel gives almost no feedback,
so the cheap instruments are worth building early — `--gdb`, the exception
dump, and `tests/test_kernel_boot.py`, which boots the real thing on the real
emulator and reads the serial console, because at this stage there is no other
kind of test.
