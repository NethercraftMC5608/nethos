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

Next is Stage 2: `ldk`, the tool that compiles a Linux driver against Linux's
own headers and reports which of its undefined symbols the shim still owes it.
