# ldk — the Linux Driver Kit

What nk owes an unmodified Linux driver.

```bash
python3 ldk.py fetch              # a Linux tree, configured for arm64
python3 ldk.py build virtio-blk   # compile its drivers, unmodified
python3 ldk.py syms  virtio-blk   # what they need that nk does not provide
python3 ldk.py stubs virtio-blk   # a panicking stub for each of those
python3 ldk.py report             # coverage, across every port
```

Today:

```
  port           objects   needs   done   stub   todo
  virtio-blk           4     108      0    108      0
  virtio-net           4     207      0    207      0
                             139 new beyond the ports above
```

That last number is the one that matters. virtio-net needs 207 symbols but only
139 of them are new — the other 68 came free with virtio-blk. The second driver
of a class is cheap; the first of a class is not, and this is how to know which
one you are about to attempt before attempting it.

The 108 are *declared*, not *reached*. A driver touches a small fraction of what
it links against, and the method is to boot it and implement whichever stub it
actually stops on. See `docs/KERNEL.md`.

The Linux source is never edited, never patched, and never vendored — only
fetched and compiled. If a port ever needs to change Linux source, the approach
has failed and the edit is hiding it.
