# nk

The NETHOS kernel. Its own core, running **unmodified Linux drivers** on a
reimplemented Linux internal API — not translated drivers, because translating
them is not a thing that works.

A sibling of NETHOS, not a replacement: the shipping images still boot Debian's
kernel and nothing outside this directory knows it exists.

```bash
../scripts/run-kernel.sh        # build and boot on QEMU virt, HVF-accelerated
make test                       # the boot test
```

Ctrl-A X to quit QEMU.

**`docs/KERNEL.md` is the real documentation** — the architecture, why it is
shaped this way, the stages, and what Stage 0 already cost. Read it first.

Stages 0 and 1 are done. nk boots on QEMU `virt` under HVF, parses the device
tree, turns on the MMU, allocates frames and heap, brings up GICv3 and the
virtual timer, and runs preemptive kernel threads — two of which alternate on
the tick without either ever yielding.

Stage 2 is done too: `ldk` compiles unmodified Linux drivers against Linux's
own headers and reports what they need. virtio-blk asks for 108 symbols;
virtio-net asks for 207, of which only 139 are new. See `ldk/README.md`.

Stages 3 and 4 are done too: unmodified `virtio_blk` reads a sector off a
disk, and unmodified `virtio_net` sends an ARP request and receives the reply.

**And the whole Linux kernel now boots on nk.** `arch/lkl` is a real Linux
architecture port whose machine is a struct of function pointers;
`kernel/lkl/nk-host.c` fills it in with nk's threads, locks, memory, timer and
console. `run-kernel.sh --lkl` links it. Linux comes up with TCP/IP, io
schedulers and filesystems, and a process at EL0 asks it `getpid` and gets 1.

nk also runs **user space** -- a program at EL0, in its own address space,
making Linux system calls, with a `copy_from_user` that refuses a pointer into
kernel memory. That is the gate for everything above the driver layer,
including any hope of running the NETHOS desktop on nk rather than on Debian's
kernel. `docs/KERNEL.md` is honest about how far that is.
