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

Stage 0 is done: nk boots, reaches Rust, owns the exception table, and is
handed a device tree. There is no MMU, no allocator and no scheduler yet.
