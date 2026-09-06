# NETHOS roadmap

Ordered by what blocks what, not by size. Anything marked **blocked** has a
named cause, not a guess.

## 0. RESOLVED: left-click on layer surfaces

**Cause:** the launcher's full-screen overlay surface had `opacity: 0` when
closed but never `pointer-events: none`, and nothing hid it until the launcher
had been opened once. An invisible sheet on the overlay layer swallowed every
pointer event on the screen. Fixed in c8fba66. The investigation below is kept
because the measurements were what found it.

## 0b. Original investigation: left-click on layer surfaces

The one that blocks the most, because it makes finished features look broken.

Measured on the laptop, 2026-08-15, by injecting real pointer events through
Wayfire's `stipc` plugin and watching `nethosd`'s request log:

| target | position | result |
| --- | --- | --- |
| panel launcher button | y=28, inside the 46px exclusive zone | `POST /api/launch` fires |
| context menu item | y=170, outside the exclusive zone | nothing |
| dock icon | dock sets `exclusive=0` while auto-hiding | nothing |
| window title bar close | Wayfire's own decoration | window closes |

So injected clicks work, and our surfaces receive **hover and right-click**
everywhere -- a context menu opens, and its items highlight under the pointer.
Only the left button fails, and only outside a reserved exclusive zone.
Widening the input region with `nethosHost.inputRect` while a menu is open
does not help: a click outside the menu but inside the widened rectangle does
not even dismiss it, which means the surface never saw the button at all.

Two candidates, in order of suspicion:

1. Wayfire routes button events to layer surfaces by exclusive zone rather
   than by input region. If so, dynamic input regions are the wrong mechanism
   for us entirely.
2. `set_input_region` is applied to the GdkSurface but not committed in a way
   the compositor picks up until something else forces a commit.

The fix that avoids both: give menus their own surface. The overlay surface
that hosts the launcher is full-screen, always correctly focusable, and
already receives clicks -- the launcher works. Context menus should be
rendered there and the chosen action routed back to the surface that asked
for it, over the existing event bus. That also removes the input-region
juggling from the panel and the dock.

**Already fixed on the way to finding this:** the dismissal handler closed the
menu on `pointerdown`, removing the element before `click` could fire on it,
so an item could never activate even where clicks do land.

## 1. Foundations

- [x] **Snapshots and rollback**, as `nethos-snapshot`. Deliberately not A/B
      partitions or btrfs: those roll back the whole machine, which is right
      for a kernel that will not boot and wrong for what actually goes wrong
      here -- an update that changes a stylesheet and a daemon and leaves the
      desktop broken while the system underneath is fine. A snapshot is ~100KB
      and takes about a second. Measured: a corrupted nethos.css went 28881
      bytes -> 32 -> 28881.
- [x] **Updater** tied to it. `nethos-update` snapshots before applying and
      restores automatically if the install fails part way -- half an update
      is worse than none, because some files are new and some are old and
      nothing says which. Both are in Settings.
- [ ] Still worth having for the kernel case: A/B partitions per
      `docs/ABUPDATE.md`, for updates that can leave a machine unbootable.
- [ ] **npkg triggers.** The largest correctness gap. npkg unpacks packages
      but runs no maintainer scripts, so anything a `postinst` would create is
      absent. Four separate bugs came from this in one evening: the `netdev`
      group (wifi silently unavailable), a dangling `regulatory.db`
      alternatives link (no 5GHz), a missing `/etc/nsswitch.conf` (no DNS at
      all), and a missing `polkitd` user (dead power buttons). Each was
      diagnosed as broken hardware first.

## 2. Interface

- [x] Desktop icons, from ~/Desktop, through the same /api/files the Files app
      uses.
- [x] Wallpapers -- four, drawn rather than shipped, each with a dark form.
- [x] Loading screen: a compositor background colour so the first frame is not
      black, plus a splash surface that waits for the panel and gives up after
      ten seconds rather than hiding a failure.
- [x] Control centre: battery with time remaining, brightness, Wi-Fi.
- [x] Volume, with mute. PipeWire, pipewire-pulse and wireplumber are in the
      desktop set now; before this the machine had no sound at all.
- [ ] More customisation, and more settings behind the Settings app now that
      the schema-driven form makes adding one cheap.
- [x] Onboarding on first boot: four steps, all of them reversible in
      Settings, and it says so. Shown once, flagged in settings.json.

## 3. Applications

**The App Store is the big one** -- stated priority, and it subsumes several
other items: driver installation becomes an App Store category rather than a
separate tool, and npkg already resolves capabilities well enough to back it.

- [x] App store, including drivers. Backed by npkg: search, install and
      remove, with live output while it works. Drivers are a category rather
      than a separate tool, because they are just packages.
- [x] File explorer. Places, breadcrumbs, rename, trash, and copy/move/delete
      with an internal clipboard; opens at a folder when launched with one.
- [x] Archive extractor, as a verb on the file rather than an application:
      "Extract here" on an archive, with progress on the same event bus.
- [ ] Spotify viewer and similar helpers.

- [x] Shared chrome: `.app-shell`, `.app-toolbar`, `.app-search`, `.row`,
      `.tile`, `.icon-well`, `.chip`, `.empty`, `.spin`, `.stream` in
      nethos.css. The App Store is built entirely from them, so the file
      explorer and extractor should need no new furniture.

## 3b. Window decoration

- [x] NETHOS windows draw their own chrome: rounded corners, three traffic
      lights on the left with the symbol revealed on hover, centred title,
      38px bar. Built in GTK rather than HTML so dragging and resizing are the
      compositor's, not ours.
- [x] Foreign windows match, via firedecor -- which *is* in Debian, as
      `reform-firedecor`, despite the name. It has the corner radius and the
      layout string Wayfire's built-in decorator lacks, so Chromium, Thunar
      and the terminal wear the same rounded frame and the same three lights.
      No C++ needed after all.
- [ ] GTK4 applications still draw their own header bars and ignore
      GTK_CSD=0. That is the remaining gap, and it is upstream's decision
      rather than a missing setting.
- [ ] Window bars are inconsistent, and absent on the terminal. Applications
      that draw their own decorations (client-side) ignore Wayfire's, so a
      NETHOS session shows two different title bars depending on the toolkit.
      Force server-side decoration where the application allows it, and
      configure the ones that do not (foot has a `csd` setting).

## 3c. Troubleshooting

- [ ] A troubleshooter that can restart and diagnose the interface without a
      terminal: restart the shell, restart nethosd, reload surfaces, show the
      diagnostics `nethos-doctor` already collects. Every UI fault in this
      project so far has been invisible from the desktop itself.

## 4. Installer

- [x] Online installer with a real interface. Still drawn straight to
      /dev/fb0 -- a GUI stack would be several hundred megabytes to draw a
      progress bar, on an image whose entire point is being small -- but it
      now has the wallpaper, the mark, a soft shadow and a hairline rim, and
      it still falls back to plain text where there is no framebuffer.
- [ ] Offline image. `scripts/build-x86.sh --sets "..."` already produces one
      with everything included; what is missing is the installer knowing to
      use the local packages instead of the network.

## 4b. Not done, and why

- [ ] **Spotify viewer.** Needs a Spotify developer application and OAuth
      credentials that only you can create; a viewer without them is a window
      that says "not configured". Worth doing once those exist.
- [ ] **Offline installer.** The image can already be built with everything
      included; what is missing is the installer preferring local packages
      over the network.
- [ ] **A/B partitions.** Snapshots cover a bad NETHOS update. They do not
      cover a kernel that will not boot, which is what the partition scheme
      in docs/ABUPDATE.md is for.
- [ ] **GTK4 window decoration.** GTK4 ignores GTK_CSD=0 and keeps its own
      header bars. Upstream's decision, not a missing setting.

## 5. Performance

- [x] Shell memory 1.69GB -> 392MB, by sharing one web process again.
- [x] `xdg-desktop-portal` 25.2s -> gone, by installing the gtk backend.
- [x] Duplicate network stacks (NetworkManager *and* systemd-networkd).
- [ ] Remaining boot time. Of ~28s to a usable desktop, 13.3s is the laptop's
      own firmware and cannot be touched from here. Kernel is 3.5s, userspace
      to `multi-user.target` 4.9s, then ~5s to the shell. The ~14s NETHOS owns
      is what is left to attack.
- [ ] Memory again, after the above: the shell is no longer the largest
      consumer, so the next measurement should come before the next change.


## 6. Requested 2026-08-18, reconciled against the tree

The list as given was about forty items. Roughly a dozen of them are already
built and were reported missing because they did not work -- which this
project keeps producing and which is worth stating as a rule: on a desktop
with no terminal, *absent* and *broken* are the same picture. Four separate
faults behind one report of "ghosting" in a single session, none of them the
thing that was reported.

### Already built (verify before rebuilding)

Snapshots and rollback, the updater, desktop icons, wallpapers, the loading
splash, the control centre (Wi-Fi, brightness, battery), onboarding, the App
Store including drivers, the online installer's interface, window chrome for
NETHOS and foreign windows, the file explorer and the extractor, and the shell
memory work (1.69GB -> 392MB).

### Blocked on one thing, and it is not new

**npkg triggers.** Already section 1's largest correctness gap, and the list
above is downstream of it more than of any missing feature. The count is now
six: the `netdev` group, `regulatory.db`, `/etc/nsswitch.conf`, the `polkitd`
user, the missing XDG user directories (no ~/Desktop, so the desktop had no
icons and Files had no sidebar), and a build that died outright in
fix_alternatives. Every one presented as something else -- broken wifi, broken
DNS, dead power buttons, a broken file manager, a broken build. Nothing else
on this list buys as much as making maintainer scripts run, or emulating the
handful of triggers that matter.

### New, in rough order of leverage

- [ ] **A declarative system manifest.** One file listing every package and
      setting, so a machine can be handed over or rebuilt from it. Cheap here
      because npkg already resolves from a set, and it makes the offline
      installer and the "hand off the system" ask the same feature.
- [ ] **Troubleshooter with an AI mode.** Section 3c already wants a
      troubleshooter that can restart the shell and read diagnostics without a
      terminal. NETHBot is the natural engine: a local model, on-device, that
      already drives a shell with a human-in-the-loop pause on anything that
      asks for a password. Offline recovery and the kernel-panic assistant are
      the same tool reached from a different place, and the second one needs
      A/B partitions (docs/ABUPDATE.md) before it has anywhere to boot from.
- [ ] **Window bars on the terminal.** Already section 3b; foot has a `csd`
      setting and this is a configuration change, not a project.
- [ ] **Right-click and dock buttons.** Reported not working. Section 0 records
      this as resolved in c8fba66, so either it regressed or the report is of
      something adjacent. Testable directly now: injected pointer events plus
      the daemon's request log is what found the launcher/control-centre bug,
      and it is the only method here that exercises the button rather than the
      API behind it.
- [ ] **More settings, more customisation.** Already section 2; the
      schema-driven form makes each one cheap.
- [ ] **Applications in their own repositories**, fetched at build time rather
      than shipped in the payload. The npk format already exists for this.
- [ ] Music player with album art; camera with face detection; a WebKit
      browser; game mode; VPN setup; printing; phone and device integration
      (AirPods, AirDrop-style transfer). All real, none blocking, and each one
      is an application rather than a change to the system.
- [ ] Kernel and scheduling work -- CachyOS or Ubuntu Studio kernels, a swap
      manager, VRAM as swap, CPU scheduling. Worth measuring before building:
      section 5 has ~14s of boot time NETHOS actually owns, and the last
      memory win came from one process change rather than a scheduler.

### nk -- a kernel of our own, hosting unmodified Linux drivers

`kernel/`, and `docs/KERNEL.md` is the documentation. A **sibling project, not
a replacement**: the shipping images keep booting Debian's kernel, and nothing
in `payload/`, `pkg/` or the image build depends on any of this. Stated up
front because the honest horizon for stages 0-4 is 6-12 months, and Genode's
equivalent is a funded team over roughly a decade.

The design rests on one fact: Linux driver source cannot be translated into
another kernel's driver model, because Linux has no stable in-kernel API and a
driver is welded to the kernel's internals rather than written against an
interface. What does work -- Genode's `dde_linux`, LKL, rump kernels -- is to
keep the driver source byte-for-byte, compile it against Linux's own headers,
and reimplement underneath it only the out-of-line symbols the linker names.
The shim is discovered, not designed.

- [x] **Stage 0.** Boots on QEMU `virt` under HVF, reaches Rust from the reset
      vector, owns the exception table, and is handed a device tree. Cost one
      real bug: QEMU passes no DTB at all to an ELF kernel, so `boot.s` carries
      the arm64 Linux image header and `run-kernel.sh` boots the flat binary.
- [x] **Stage 1.** Device tree, frame allocator, MMU, kernel heap, GICv3, the
      virtual timer, preemptive threads. Two threads alternate on the tick
      without either yielding. Cost three bugs worth keeping: MMIO through
      `read_volatile` compiled to a writeback load, which no hypervisor can
      emulate and real hardware runs fine; the physical timer traps under any
      hypervisor, so nk uses the virtual one; and a new task starts with
      interrupts masked, which stops the machine without crashing it.
- [x] **Stage 2.** `ldk` compiles unmodified Linux drivers against Linux's own
      headers for aarch64 and reports what they need: **virtio-blk 108
      symbols, virtio-net 207 of which 139 are new**. kbuild does the
      compiling, so Linux's own flags are used rather than reconstructed.
      `pkg/npkg_elf.py` gained a section-table reader for it -- a .o has no
      program headers, which is what everything in that file used before.
- [x] **Stage 3.** Unmodified `virtio_mmio` + `virtio_blk` read a sector off a
      QEMU disk. 113 of 154 symbols implemented, 41 stubs never reached --
      the "implement only what it reaches" claim, measured. Cost four bugs
      worth keeping, all in docs/KERNEL.md: a data symbol defined as a
      function (unrecoverable, and `ldk` now detects it from relocations); a
      callback whose name means the opposite of what it looks like; arm64's
      `virt_to_page` not using `virt_to_pfn`, which produced a read the device
      reported as successful and never delivered; and a console whose only
      failure mode was looking like a hang.
- [x] **Stage 4.** `virtio_net` sends an ARP request and receives the reply --
      `10.0.2.2 is at 52:55:0a:00:02:02`, checked from the raw frame bytes.
      Needed a real vmemmap: 8MB of `struct page`, because `receive_buf`
      dereferences one where virtio-blk only ever computed an address for it.
      Built `paging::map_normal` and `frames::alloc_contiguous_aligned` for
      it. HVF cannot decode one of virtio-net's MMIO accesses, so this port
      runs under `--tcg`. `e1000` still untouched.
- [ ] **Stage 5.** Decide against `ldk report`'s numbers whether USB, DRM or
      WiFi is worth attempting. Genode still does not do GPU.

**Linux on nk, via LKL** -- the route to the desktop, and much shorter than
the one below it.

- [x] **The whole Linux kernel links into nk.** `arch/lkl` is 5,168 lines and
      produces one 19.7MB object with two undefined symbols; `kernel/lkl/
      nk-host.c` is 288 lines and supplies the machine. nk is 14MB with Linux
      inside it, and Linux runs: threads on nk's scheduler, nk's semaphores
      and mutexes, nk's frame allocator, nk's timer.
- [x] **Linux boots.** Full `start_kernel`, TCP/IP, io schedulers, Btrfs and
      XFS, on nk's memory, threads, locks, clock and console. The bug that
      held it was a synchronisation primitive written with `&mut self`:
      `down` holds a reference across a context switch while another task
      mutates the same object, which is aliasing UB, and the compiler kept
      `count` in a register and re-tested the stale value. Atomics and
      `&self`. Also fixed: LKL's use of thread id 0 as a sentinel, a
      semaphore waking every waiter instead of one, timer callbacks running
      in interrupt context, an overflow in the deadline arithmetic, and
      sixteen task slots where Linux wanted sixty-four.
- [x] **EL0 `svc` routed to `lkl_syscall`.** A process at EL0, in its own page
      tables, asks `getpid` and Linux answers 1. The chain from bare aarch64
      to the Linux ABI is closed. Process exit now runs Linux task cleanup
      through the host TLS destructor.
- [x] **Filesystem-backed ELF bootstrap.** Linux rootfs/VFS stores and reads
      an embedded `/nk-init` ELF fixture; nk validates and maps its segments,
      zero-fills BSS and runs it at EL0. Persistent storage, external binaries
      and a complete process startup ABI are still outstanding.
- [x] **Back each launched nk process with a Linux task.** Dedicated host
      threads request Linux thread-group leaders, unshare files/fs, retain
      their identity through EL0 preemption, and release Linux tasks on exit.
      The parent reclaims nk pages and stacks after joining. Fork, execve,
      userspace signals and Linux wait-status propagation remain outstanding.
- [ ] **Desktop runtime integration.** Normal binary addresses, dynamic
      linking, checked syscall buffers, signals, shared memory, futexes and
      DRM/device access must work before desktop configuration can be tested.

**User space** -- nk's own, and the mechanism the above attaches to.

- [x] **A process at EL0.** Own address space, own translation table, Linux's
      syscall numbers (`write` 64, `exit` 93), and a `copy_from_user` that
      translates with `AT S1E0R` so a pointer into kernel memory is refused
      with `EFAULT` -- demonstrated, not asserted: the demo carries the errno
      out through the exit status.
- [ ] **The kernel into TTBR1's half.** Required before processes can live at
      the low addresses every real binary is linked for, and it removes the
      copy of the kernel's tables every address space currently carries.
- [ ] **An ELF loader and a filesystem to load from.** The program is
      currently assembly in the kernel image.
- [ ] `fork`, `exec`, `mmap`, `futex`, signals, `epoll` -- the long tail, and
      the actual size of the problem. `docs/KERNEL.md` has the measurement of
      what a desktop needs and why borrowing stops helping here.

### Not started, and honest about why

- [ ] **ARM.** `build-image.sh` targets arm64 and `build-arm.sh` exists, so
      this is not from zero -- but nothing has booted on real ARM hardware and
      no claim should be made until it has.

## 7. The assistant, and repair when the desktop is not there

Built, 2026-08-18:

- [x] **Troubleshooter**, closing 3c. Reload the surfaces, restart the daemon,
      restart the shell, and the diagnostics `nethos-doctor` collects --
      without a terminal, which is the point. Both restarts go through
      `nethos-reload`; the endpoint's own version used
      `pkill -f 'nethos-view url='`, which matches application windows too.
- [x] **Ask on the panel.** A field you type into on the bar. It speaks to
      NETHBot over the WebSocket its backend already exposes, from the page --
      the daemon is not in the path, so an optional assistant cannot become a
      dependency of the thing that draws the desktop.
- [x] NETHBot behind a button that reports what it found rather than doing
      nothing when pressed. Searched in ~/.local/share/nethbot, /usr/share/
      nethbot and ~/nethbot.

Still to do, in dependency order:

- [ ] **Port NETHBot to Linux.** Its screen-control paths are macOS-bound
      (pyautogui, Quartz) but lazily imported, so the FastAPI backend should
      start on Linux untouched and the shell-command and chat paths -- the
      ones a troubleshooter needs -- should work. Unverified: nothing has run
      it there yet. Its own README already names NethOS as roadmap.
- [ ] **A local model, so it works with no network.** The hybrid design already
      assumes a small local model runs the loop and the cloud is a fallback;
      an offline install needs the fallback to be absent rather than broken.
- [ ] **Repair when the desktop does not come up.** The assistant is only
      useful here if it can run when the thing it is repairing cannot. That
      means somewhere else to boot from, which is the A/B partition scheme in
      docs/ABUPDATE.md -- so this is blocked on that, not on the assistant.
      Snapshots deliberately do not cover it: they restore NETHOS files, and
      the case here is a kernel or an initramfs that will not start.
- [ ] **A GRUB entry that boots to it.** Once there are two partitions, a
      failed boot should offer the repair side rather than a console. Also the
      natural home for a failed or interrupted update, which is the one
      failure mode where the machine knows something went wrong and currently
      has nothing to say about it.
- [ ] **Watch rather than wait.** Notice a fault, ask in the background whether
      it is significant, and only then say something. Every UI bug in this
      project was found by a person noticing, describing it wrongly through no
      fault of their own, and someone else measuring -- four of them in one
      session were not the thing reported.
