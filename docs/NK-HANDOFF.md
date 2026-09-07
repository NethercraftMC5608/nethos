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
- **disk presence moving the C repro** — `exitpoll` passes with the virtio
  disk attached, mounted, and 4MB read through it. Disk alone is not the
  trigger.
- **the late #16 face as a kernel wedge** — it was the watchdog stopping the
  machine after 4x100-tick rounds, not a wedge: `/proc/timer_list` showed the
  in-flight poll's `hrtimer_wakeup` still queued in the future, and a fresh
  `lkl_syscall(getpid)` returned 1 at the same moment. Fixed by waiting for
  init (121-round backstop) in `826e305`.
- **virtio `intr 0x0` / `status 0x7`** — identical in green boots; no
  discriminating power for the late face.
- **lost wakeups in nk's sem layer** — per-sem ups/downs balance on every
  parked sem; nk delivers every wakeup.
- **LKL CPU lock stuck mid-wedge** — acquirable from a fresh watchdog entry
  mid-wedge; no dead owner.
- **per-task `brk`/`mmap_next` copies** — diverged across threads sharing page
  tables and handed out overlapping mappings (two overlapping 128MB PROT_NONE
  reserves measured, EL0 permission fault DFSC 0b001111 in an innocent
  thread). Fixed by per-TTBR0 shared layout in `826e305`.
- **`set_user_memory` reading TTBR0** — named the wrong address space at
  spawn/exec; fixed by passing the root explicitly in `826e305`.
- **thread-count/lock shape, import size, diag volume for the serve wedge** —
  5 sleepers + ThreadingHTTPServer + GET serves; +`import nethosd` serves;
  +real `diag()` at 0.2s serves; +real `backend()`/`SWAY.request` serves. All
  exonerated; the wedge needs the real `nethosd.main()`.
- **aarch64 syscall numbers in watchdog reports** — there is no `poll`
  syscall; glibc `poll()` arrives as `ppoll` (73). 115 is `clock_nanosleep`,
  not `clone` (220). A growing 115 with exits lagging is healthy sleep loops.
- **`sys_brk` as the MemoryError source** — it returns the old break on every
  failure path, never an errno; only `sys_mmap` returning -ENOMEM surfaces to
  CPython as MemoryError (five sites in `user.rs`, undetermined which).

Those last two are the shape of this bug: it hides behind other real bugs.
Two correct fixes landed today and neither closed it.

**Update 2026-09-08 (unattended desktop run, `826e305` merged):** the
netprobe shape is green — `poll-timeout` + `request`/`json` +
`NETHOSD_SHAPE_OK` in one boot, soak/writeback/signals 3/3. Full nethosd
moved from the #17 EL0 NULL fault (gone: zero esr/fault lines) to a serve
wedge: ThreadingHTTPServer binds/listens, the client connects, the GET times
out; server thread parked in `ppoll`, main thread READY-spinning in
userspace; snapper + main thread raise `MemoryError` (mmap -ENOMEM site
still undetermined — needs one `println!` per `sys_mmap` -12 return). The
rare early face (frozen `armed=224`, child stuck pre-python in execve/fork
handshake) is still open. M1 still red.

**Update 2026-09-08 later the same session (`1cc1803`, `193ce5d`): M1 IS
GREEN, 3x.** The serve wedge was address-space exhaustion, not a hang:
full nethosd peaks at ~730MB of live anonymous reservations against a
~732MB usable window (`USER_MMAP_TOP` 0x2F000000 minus brk ~0x13a6000);
the red boot's extra 128MB arena from elr `0x2edd9c40` (glibc, anon
`flags 0x22` fd -1) was refused and CPython's 128MB/64MB retry loop
printed ~2700 `mmapfail` lines with ~581MB of frames still free. Whether
the last arena fits is thread-interleaving timing — a race, 3/4 green in
the final probes. The `mmapfail` lines are kept as diagnostics (no
behaviour change). M1 proof, client-printed in the serial log of a
`run-kernel.sh --lkl --disk npkg.img --initrd nethosd-e2e-full.cpio`
boot (`/tmp/m1-main2.log`, `/tmp/m1-main3.log`, `/tmp/mmap7.log`):

```
NETHOSD_OK   statusline HTTP/1.1 200 OK
NETHOSD_OK   request 405 bytes
NETHOSD_OK   status-shape keys: battery,generation,host,kernel,load,mem,nethos,subscribers,time,uptime,user
NETHOSD_STATUS_OK kernel=6.12.0+ uptime=3.41
```

Next: M2 (compositor). Device MAP_SHARED for dumb buffers is still the
known-absent display path; wl_shm memfd MAP_SHARED is green.

**Update 2026-09-08, M2 IS GREEN (`3d46279`, merge of `3319251`).**
Hand-rolled Wayland server+client probe (`kernel/init/wlprobe.c`,
`scripts/build-wl-test.sh`): server binds `$XDG_RUNTIME_DIR/wayland-0`,
speaks `wl_display.get_registry` → `wl_registry.global` per wayland.xml
1.23.1, client prints the globals. Verified on the main checkout
post-merge (`/tmp/m2-main.log`, 4/4 boots green overall, 0 faults):

```
WL_GLOBAL name=1 interface=wl_compositor version=4
WL_GLOBAL name=2 interface=wl_shm version=1
WL_GLOBAL name=3 interface=wl_output version=2
WL_GLOBAL name=4 interface=xdg_wm_base version=2
WL_REGISTRY_OK globals=4
WL_PROBE_OK
```

No libwayland needed (probe 72KB + libc; libwayland itself measured only
238KB installed, so it would also fit — chosen against for closure risk
against the 99.5%-full address space). Next: bind wl_shm +
wl_shm_create_pool over SCM_RIGHTS memfd (the M3 buffer path), then M3.

**Update 2026-09-08, M3 pixel path GREEN (`d4d047f` + `def2cfa`).**
M3 lane proved the exact pixel path twice over in worktree `../nk-lane-m3`:
(A) client binds wl_shm+wl_compositor, memfd pool over SCM_RIGHTS,
server reads back through the shared mapping — `SHM_SERVER_CKSUM ==
SHM_CLIENT_CKSUM` + post-map coherence `coh=1`; (B) slate `#14181f` to
scanout via fbdev write()+modeset, `M3_PIXELS_OK 1 1.0000` on 1280x800;
(C) full shell slate linear-layer ramp, `M3_GRADIENT_OK 17 1.0000 0`
(17 integer-ramp colours, 0 mismatched rows of 800). Verified on the main
checkout post-merge (`M3_GRADIENT_OK 17 1.0000 0` via `scripts/m3-check.py
--gradient` on a monitor-socket screendump). Found along the way and fixed
in `def2cfa`: PROT_READ shared mappings faulted on first EL0 read
(`protect_user_none` applied to any `!writable && !executable`, clearing
AP to EL0-no-access, DFSC 0b001111) — now gated on `prot == 0`, proven by
`PROT_READ_OK readable` from a MAP_SHARED memfd readback probe.
NOT claimed: WebKit rendering — no HTML/CSS/JS ran. Numbered gaps to true
M3-render: (i) WebKit closure (~96MB lib + 145MB debs) vs the 99.5%-full
window (demand paging and/or raised USER_MMAP_TOP needed); (ii) device
MAP_SHARED for dumb buffers still refused (real compositor cannot scan out
client buffers yet); (iii) done — the PROT_READ AP bug above.

**The live lead:** the serve wedge + the `MemoryError`: which `sys_mmap`
-12 site fires under the real `nethosd.main()`, and whether the parked
server `ppoll` is cause or consequence. Cheapest next step: clean
real-`main()` repro with a net.cpio-identical nk-init (drop the `ip -o`
pipeline), then the mmap-site logging above. `virtio intr 0x0` and the
unmask handshake are retired as leads for the late face (see negatives).

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
