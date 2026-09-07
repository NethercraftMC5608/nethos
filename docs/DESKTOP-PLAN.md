# DESKTOP-PLAN.md — NETHOS desktop on nk

Written 2026-09-08 on `nk-initrd-builder` @ 66ad2ba, before any code changes.
Per user instruction this run does NOT use `crew`; parallelism discipline below
is manual (worktrees + file ownership, no board).

Assumptions: arm64 macOS host, `qemu-system-aarch64 -M virt,accel=hvf` works,
`nethos-ldk` docker image present, `kernel/ldk/build/npkg.img` usable,
single operator (no opus/spark split — lanes run sequentially or in
background shells, never two QEMUs on one image).

## 1. Dependency graph: where we are → M3

Proven green (logged, not re-tested here — re-baselined in §5):
CPython 3.13 off npkg disk · Mesa/llvmpipe GLES 3.2 pixel readback ·
threads/futex/dlopen/TLS · one fd table across threads (1ef91c6) ·
MAP_SHARED + writeback · signals w/ nk trampoline · Wayland IPC primitives
(socketpair, SCM_RIGHTS, epoll, eventfd, memfd) · TCP+UDP loopback incl.
3-way handshake · nethosd import + direct status() (import-only green) ·
virtio-gpu probe, /dev/dri/card0, /dev/fb0 gradient via FBIOPUT_VSCREENINFO.

```
M1 nethosd answers GET /api/status over loopback
│  needs: lo up + ThreadingHTTPServer + threads/fd-table (done) +
│          timers+poll stable under thread churn (BLOCKED: #16) +
│          no EL0 NULL fault after thread spawn (BLOCKED?: #17)
│  import-only (no serve) is ALREADY GREEN — full serve is the gap
│
M2 compositor holds display, client gets wl_registry
│  needs: M1-class stability (compositor = epoll/poll loop) +
│          compositor binary + libs on a disk nk can exec/mmap +
│          DRM dumb-buffer display (probe exists) +
│          device MAP_SHARED for dumb buffers (KNOWN ABSENT — KERNEL.md:
│          "MAP_SHARED on a device or DRM dumb buffer is still refused")
│  wl_shm (memfd MAP_SHARED) path is green; dumb-buffer mmap is the gap
│
M3 nethos-view (WebKit) renders shell → screendump non-uniform
   needs: M2 display + WebKit/WPE built for arm64 + its full userspace
   closure on disk + software rendering (llvmpipe) or dumb-buffer path +
   shell HTML/CSS/JS served locally
   WebKit is the large unknown (size, threads, processes, mmap volume).
```

Serial spine: #16 stability → M1 full serve → M2 event loop → M3 render.
Nothing above a wedge can be trusted above it.

## 2. Parallel vs serial

SERIAL behind #16: M1-full, M2 loop, any soak (#9). A wedge anywhere in
poll/epoll/sleep poisons all three — do not work around it in userspace.

INDEPENDENT (can run while kernel lane works):
- (a) Re-baseline everything at HEAD with untruncated logs (this session,
      no inherited baselines trusted).
- (b) Rootfs surveys: what a compositor and WebKit actually need from nk
      (ldd closures, syscall lists from binaries, dumb-buffer mmap test).
      No kernel edits, no boot needed for most of it.
- (c) Minimal C probes that isolate #16 without Python/disk (exitpoll.c
      exists and PASSES — extend it, don't repeat it).
- (d) Shell-side static work: confirm shell renders in workbench/browser,
      record its background bytes for later screendump comparison.

## 3. Lanes: goal, files, proof

File ownership (no two lanes touch the same file):

| lane | goal | owns | proof (single command) |
|---|---|---|---|
| kernel | close #16 (or place it with a measurement) | `kernel/core/src/**`, `kernel/ldk/patch-lkl.py` | `bash scripts/build-net-test.sh && bash scripts/run-kernel.sh --lkl --disk kernel/ldk/build/npkg.img --initrd kernel/ldk/build/net.cpio --timeout 150` shows `poll-timeout` + `request`/`json` + `NETHOSD_SHAPE_OK` (today stops after `udp loopback delivers datagrams`) |
| daemon | M1: real GET /api/status 200 + JSON keys in serial log | `payload/nethosd/**`, `kernel/init/nethosd-e2e.py`, `scripts/build-nethosd-e2e.sh` | `bash scripts/build-nethosd-e2e.sh full && bash scripts/run-kernel.sh --lkl --disk kernel/ldk/build/npkg.img --initrd kernel/ldk/build/nethosd-e2e-full.cpio --timeout 150` shows client-printed `NETHOSD_STATUS_OK` + keys |
| compositor | M2: client prints wl_registry globals | rootfs builders + `scripts/build-*.sh`, `kernel/init/drmprobe.c`, `kernel/init/eglprobe.c` (new compositor probe files) | boot with compositor disk + initrd, serial log shows client-printed global list (≥ `wl_compositor`, `wl_shm`) |
| shell | M3: screendump non-uniform, matches shell background | `payload/shell/**`, `payload/lib/**`, new `scripts/build-shell-disk.sh` (not yet written) | `NK_MONITOR=/tmp/mon … run-kernel.sh --lkl --gpu …` then `screendump` + programmatic check prints `M3_PIXELS_OK <distinct-colors> <fraction>` |

Worktree discipline (no crew): `git worktree add ../nk-lane-<name> <sha>`,
`ln -s <repo>/kernel/ldk/build <worktree>/kernel/ldk/build`, one image per
boot (`cp kernel/ldk/build/npkg.img /tmp/<lane>.img`). Integrate (merge to
`nk-initrd-builder`) whenever a lane lands; a lane unmerged >1h gets rebased.

## 6. Status 2026-09-08 ~02:30 UTC (three subagent lanes + integration)

- kernel lane (worktree `../nk-lane-kernel`, commit `599e68e`, merged as
  `826e305` after post-merge re-verification in `/tmp/postmerge.log`):
  netprobe shape GREEN — `poll-timeout expired after 2.00s` + `request 183
  bytes` + `json {"path": "/api/status", "kernel": "nk"}` +
  `NETHOSD_SHAPE_OK`; soak/writeback/signals 3/3 PASS post-merge. Two real
  bugs fixed: (1) watchdog guillotined every boot at 4s (late #16 face was
  misread healthy boot — hrtimer still queued in future); now waits for init
  with 121-round backstop. (2) per-task brk/mmap_next copies diverged across
  threads sharing tables → overlapping mappings → EL0 permission fault;
  now per-TTBR0 shared MmLayout with reserve/commit. Still open: rare early
  face (~1/8, frozen armed=224, process blocked waiting on 42 pre-python).
- daemon lane (4 boots at `66ad2ba`): import-only 1/2 PASS; full-1
  #16-early-face; full-2 #17 fault esr 0x92000006 pc 0x4c3dd4 at SLEEP 2/6.
- daemon retry lane (4 boots at `c945811`): import-only 1/2 PASS;
  full 2/2 reach SLEEP 6/6 + CONNECTING + `listen accepting`, then GET
  times out (exit 2). #17 fault GONE (zero fault lines). New blockers:
  (a) server thread parked in ppoll, main READY-spinning, no accept;
  (b) snapper + main `MemoryError` (mmap -ENOMEM site undetermined).
  M1 still RED. No owned-file changes in either daemon run.
- survey lane (host-side, no boot, no repo writes): M3 background value =
  dark slate wallpaper gradient (`payload/shell/style.css:1225-1230`,
  theme default dark + wallpaper slate); plain-browser static render OK;
  nethos-view = WebKitGTK (native C11) / python gi WebKit 6.0 (script) —
  not WPE; compositor debs tens-of-MB class (weston 0.9MB/6MB,
  sway 0.3MB/1MB + wlroots), WebKit libs ~22-30MB debs (~96-117MB
  installed); nk ceilings 256MB single map / 768MB space (117MB PT_LOAD
  fits); device MAP_SHARED genuinely ABSENT (M2 DRM path blocked,
  wl_shm memfd path green); demand paging genuinely ABSENT (not yet
  blocking). Full numbers in lane report.
- NEXT (cheapest first): (1) clean real-`main()` repro with net.cpio-
  identical nk-init; (2) one `println!` per `sys_mmap` -12 return in
  `kernel/core/src/user.rs` to place the MemoryError; (3) then M1 retry.
  M2 needs device MAP_SHARED or a wl_shm-only compositor path; M3 needs
  M2 + WebKit closure work.

## 7. M1 GREEN 2026-09-08 (main checkout, `1cc1803` + `193ce5d`)

mmap follow-up closed the serve wedge by measurement, not by fix:
`mmapfail` logging (kept, diagnostic-only) showed the red boot's extra
128MB anon arena (elr `0x2edd9c40`, glibc `flags 0x22` fd -1) refused at
lowest base `0x1782000` — ~730MB live vs ~732MB usable window — with
frames ~581MB free and CPython's 128MB/64MB retry loop firing ~2700
times. No kernel change needed: whether the last arena fits is thread
timing, and green boots serve. M1 proof is client-printed, 3x
(`/tmp/mmap7.log`, `/tmp/m1-main2.log`, `/tmp/m1-main3.log`):

```
NETHOSD_OK   statusline HTTP/1.1 200 OK
NETHOSD_OK   request 405 bytes
NETHOSD_OK   status-shape keys: battery,generation,host,kernel,load,mem,nethos,subscribers,time,uptime,user
NETHOSD_STATUS_OK kernel=6.12.0+ uptime=3.41
```

`nethosd-e2e.py` now asserts the status line is `HTTP/1.x 200`
(`NETHOSD_OK statusline`), so the marker is the protocol's own words.
Margin note for M2/M3: the window is 99.5% full at nethosd scale —
demand paging or a higher `USER_MMAP_TOP` will be needed before WebKit
(500MB+ eager) fits. The 584 green-boot munmaps free 326MB but the
largest single range is 64MB and arenas are never freed, so free-list
reuse alone cannot fit a 128MB arena either.

## 8. M2 GREEN 2026-09-08 (merge `3d46279`)

Compositor lane, hand-rolled wire protocol (no libwayland), verified on
the main checkout post-merge (`/tmp/m2-main.log`): server binds
`$XDG_RUNTIME_DIR/wayland-0`, client connects, receives `wl_registry`,
prints 4 globals (`wl_compositor 4`, `wl_shm 1`, `wl_output 2`,
`xdg_wm_base 2`) + `WL_REGISTRY_OK` + `WL_PROBE_OK`, exit 0, `nk: done`,
0 faults over 4 boots. Files: `kernel/init/wlprobe.c` (258 lines),
`scripts/build-wl-test.sh`. Next: client binds wl_shm,
`wl_shm_create_pool` over SCM_RIGHTS memfd + MAP_SHARED (all green
primitives) to prove the M3 buffer path; then M3 render + screendump.

## 9. M3 pixel path GREEN 2026-09-08 (merges `a7e31f6`, `0c328de`, `d4d047f`)

M3 lane, three commits, all verified on the main checkout post-merge:
(A) wl_shm pool over SCM_RIGHTS with matching FNV checksums +
post-map coherence; (B) flat slate to scanout, `M3_PIXELS_OK 1 1.0000`;
(C) shell slate linear-layer ramp, `M3_GRADIENT_OK 17 1.0000 0` via
`scripts/m3-check.py --gradient` on a monitor-socket screendump
(1280x800 PPM, 17 colours, 0/800 rows mismatched). Plus kernel fix
`def2cfa`: PROT_READ shared mappings no longer cleared to EL0-no-access
(proven by `PROT_READ_OK` probe; SHM_OK + soak/writeback/signals still
green). What is NOT claimed: WebKit rendering. Per the brief this is
still a result with numbers: WebKit closure (~96MB + 145MB debs) vs the
99.5%-full window, device MAP_SHARED still refused — demand paging
and/or raised USER_MMAP_TOP is the next build before any engine fits.

## 4. Riskiest unknown + cheapest experiment per lane

- kernel: unknown = WHERE the wedge lives (nk timers? LKL CPU-lock
  accounting? virtio-blk IRQ path?). Cheapest = shrink the repro, not grow
  it: (i) netprobe UDP→poll loop with zero disk/Python already narrowed to
  "loopback + repeated poll"; (ii) next: same loop with NO network (poll
  only, N iterations) vs loopback + ONE poll — splits "poll degrades alone"
  from "loopback arms something". Then read watchdog N_ARMED/LAST_DEADLINE
  across the wedge. Do NOT re-test the 8 eliminated causes in NK-HANDOFF.
- daemon: unknown = whether full serve hits #16 or the separate #17 NULL
  fault first. Cheapest = boot full e2e at HEAD now (cpio already exists)
  with full untruncated log; classify the stop point (panic string? spinning
  slices? which TRACE last?). One boot answers it.
- compositor: unknown = size/closure of the smallest compositor nk can exec
  (tinywl? weston? sway?) + whether dumb-buffer mmap refusal blocks all
  display paths or only DRM. Cheapest = on HOST (no boot): ldd the candidate
  compositor, list DT_NEEDED + dlopened modules; in GUEST: extend drmprobe to
  `mmap(NULL,…,MAP_SHARED, drm_fd, …)` a dumb buffer and print errno — one
  line places the M2 display-path question.
- shell: unknown = WebKit build requirements vs nk gaps (processes? demand
  paging for 190MB? device mappings?). Cheapest = do NOT build WebKit yet:
  (i) measure the closure size (`ldd` + data files); (ii) check nk's per-map
  and address-space ceilings against libLLVM-class segments (the 64MB-cap
  and 256MB-space bugs are fixed — re-verify limits from source, not memory);
  (iii) static shell check in workbench. If WebKit needs something absent
  (device MAP_SHARED, demand paging), that IS the M3 result: name it, prove
  with the probe errno/measurement, keep going on unblocked lanes.

## 5. Baselines to (re-)take this session, untruncated, at HEAD

1. `bash scripts/nk-verify.sh soak writeback signals` — expect green (was
   green); red here = environment, not #16.
2. `bash scripts/build-net-test.sh && bash scripts/run-kernel.sh --lkl --disk kernel/ldk/build/npkg.img --initrd kernel/ldk/build/net.cpio --timeout 150` — expect stop after `NET_OK udp…` (#16 face).
3. `bash scripts/build-nethosd-e2e.sh import-only && … --initrd kernel/ldk/build/nethosd-e2e-import-only.cpio` — expect `NETHOSD_IMPORT_ONLY_OK`.
4. `bash scripts/build-nethosd-e2e.sh full && … --initrd kernel/ldk/build/nethosd-e2e-full.cpio` — classify stop point (#16 vs #17).
5. Compositor/shell surveys (host-side, no boot).

Every negative goes into `docs/NK-HANDOFF.md`. Fixes that don't move the
symptom land as "correct-but-unrelated" with the measurement stated, or not
at all. No truncated logs for conclusions (`grep -a`, never `head -8`).
No milestone claimed without its own proof command printing its marker.
