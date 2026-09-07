# Handing over `opus`

You are taking over the `opus` seat on this repository. Another agent
(`spark`, running opencode) and a human (`mac`) are working in the same
checkout at the same time. Read `docs/CREW.md` first — it is short — then
this.

## The goal

**The NETHOS desktop, running on nk.** Everything ladders to that: nethosd
(stdlib Python), a Wayland compositor, WebKit, and the HTML/CSS shell, all on
our own kernel. `docs/KERNEL.md` is the kernel's own documentation and carries
every stage and every bug that cost real time. Read it before re-deriving
anything.

## First five minutes

    export CREW_AGENT=opus            # or the Claude Code hooks set it for you
    crew status                       # who is here, what they hold, the queue
    crew inbox                        # spark leaves real findings here
    crew task list

Claim before you edit, release when you are done, and say what you are doing.
The hooks do most of it automatically; the part that is not automatic is
telling the others what you have learned.

## What is proven working, so do not re-test it

CPython 3.13 (npkg installs, resolves, verifies) · Mesa with llvmpipe and
OpenGL ES 3.2, pixels read back correctly · threads, futex, `dlopen`, TLS ·
one file-descriptor table shared across threads · `MAP_SHARED` with
writeback · signals with an nk-owned `rt_sigreturn` trampoline · the Wayland
IPC primitives (`socketpair`, `SCM_RIGHTS`, `epoll`, `eventfd`, `memfd`) · TCP
and UDP over loopback, including a full three-way handshake · **nethosd
imports, starts, and answers its `status()` API on nk**.

None of that is speculation; each has a test or a logged measurement.

## The one bug that matters

Task **#16** on the crew board. Everything else on the queue is downstream of
it.

**Symptom, minimal repro:** one UDP loopback round trip, then repeated
`poll(pipe, timeout)`. The first two or three expire correctly and on time,
then it wedges. The user process is `ready` and spinning — not blocked —
while `timers` and `lkl-irq` are blocked.

    bash scripts/build-net-test.sh
    bash scripts/run-kernel.sh --lkl --disk kernel/ldk/build/npkg.img \
        --initrd kernel/ldk/build/net.cpio --timeout 150

It stops after `NET_OK udp loopback delivers datagrams`.

**The same bug wears other faces**, all from one binary and one image, which
is what makes it a race rather than a threshold:

| face | seen as |
| --- | --- |
| wedge | poll stops expiring |
| panic | `lkl_bug("bad count while changing owner")`, `arch/lkl/kernel/cpu.c:90` |
| fault | EL0 read of address 0 in `_PyEval_EvalFrameDefault` |
| silent stall | children run and exit 0 with no output at all |

`spark` measured roughly 5/8 fault, 2/8 stall, 1/8 wedge across identical
boots.

**Eliminated by test, not by argument. Do not spend anything re-checking
these:**

- threads — `kernel/init/exitpoll.c` passes: a thread blocks, a thread exits,
  timed polls expire correctly. 2.6s, no disk.
- virtio/ext4 on its own — the same repro with the disk mounted and 4MB read
  passes.
- CPython and Python threads — pass.
- signals — reproduces at `19b11d5`, before the signal work landed.
- console output — the polls wedge with no printing between them.
- `EINTR` — raw `libc` `poll` via `ctypes` returns `rc=0 errno=0` in exactly
  1.000s after the UDP round trip.
- **thread-id aliasing** — real bug, fixed, symptom unchanged.
- **the IRQ pump wake race** — real bug, fixed, symptom unchanged.

Those last two are the shape of this bug: it hides behind other real bugs.
Two correct fixes landed today and neither closed it.

**The live lead:** pseudo-fs mounts (`devtmpfs`, `proc`, `sysfs`) are fine;
the ext4-on-virtio-blk mount is where the shell wedges. The only thing that
distinguishes them is a **device interrupt**. Every dump in the whole
investigation has `lkl-irq blocked` in it, and where we looked, `intr 0x0` on
the transport. `docs/KERNEL.md` has said for months: *"Not yet reliable past
the first read; unmask handshake suspected, not measured."* That is still the
best-supported line and it is still not measured.

## Also open

- **#17** — an EL0 NULL dereference about a second after nethosd spawns its
  daemon thread. Fully decoded: `ESR 0x92000006` is a data abort from EL0,
  translation fault level 2, a *read*, `FAR` valid at 0. `pc 0x4c3dd4` is in
  python3's own text (Debian's python3.13 is `ET_EXEC` at `0x400000`), four
  instructions after `bl PyObject_Vectorcall` inside
  `_PyEval_EvalFrameDefault`. CPython's contract is that NULL means an
  exception is set, so this is nk answering something in a way CPython treats
  as impossible. **Independent of #16** — it reproduces on a pre-`e19ff4b`
  kernel.
- **#19** — a console mitigation. `e19ff4b` made forked children inherit
  `has_console`, which is correct (before it, a child's `> file` and
  `2>/dev/null` were ignored and went to the UART regardless) but makes child
  output depend on the machinery #16 breaks. Do not revert it. It is a
  mitigation and it can wait until #16 is understood.
- **#9** — soak, gated on #16. Soaking on top of a known race measures the
  race.

## Traps, each of which cost real time

- **The suite is already red on `Npkg` at HEAD**, with no local changes. That
  is #16, not a regression — check before blaming yourself or anyone else.
- **`grep -a`.** Console logs contain NUL bytes, so plain grep calls them
  binary and prints nothing, which reads exactly like "it never happened".
- **Do not use `--include` with busybox grep** (Alpine). It silently matches
  nothing. Use a Debian image. This made me wrongly conclude a panic string
  was not in the kernel source.
- **Do not truncate output you are about to draw a conclusion from.** A
  `head -8` hid the line that mattered and I asserted a green baseline I had
  never actually seen, then sent `spark` to read the wrong code for an hour.
- **Copy the disk image before booting** if another agent might be running:
  `qemu ... Failed to get "write" lock` means you both attached
  `npkg.img`.
- **Use a git worktree to test another commit.** `git worktree add ~/nk-old
  <sha>`, then symlink the real `kernel/ldk/build` into it. Never stash or
  check out in the shared tree — other agents are editing it.
- **The LKL tree is a docker volume that survives builds**, so every patch is
  a migration, not an edit. `patch-lkl.py` writes the values it wants on every
  run for that reason.

## The habit to keep

Measure before concluding, and say plainly when a fix did not work. I broke
that rule three times in one session — blamed another agent's commits for a
regression that predated them, called thread-id aliasing the root cause when
it was not, and retracted a correct hypothesis one message before it was
confirmed. Each cost `spark` real time. The failure mode was always the same:
reasoning confidently from a baseline I had not actually measured.

A fix that does not change the symptom is still worth committing if it is
correct on its own terms — but commit it as that, and say so.
