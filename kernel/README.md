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

Next is Stage 3: implement whichever of those 108 stubs virtio-blk actually
reaches, and read a sector off a disk.
