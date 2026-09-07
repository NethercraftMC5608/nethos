# Autonomous run: the NETHOS desktop on nk

This file is the brief. You are running unattended — **no human will answer
you.** Do not ask questions. When a decision is needed, make it, write down
what you assumed, and keep going.

## Mission

Get the **NETHOS desktop running on nk**, our own kernel. You are not done
until a milestone below is verified by a command that prints a marker you did
not write by hand into the output.

### Definition of done, in order. Each is a real stop-and-check.

**M1 — nethosd answers.** `nethosd` runs as a daemon on nk and a client on nk
gets `HTTP/1.1 200` and valid JSON from `GET /api/status` over loopback.
Verified by: a marker printed by the *client*, plus the JSON keys, in the
serial log of a `run-kernel.sh` boot.

**M2 — a compositor holds a display.** A Wayland compositor starts on nk,
creates a display, and a client connects to its socket and receives a
`wl_registry`. Verified by: the client printing the globals it was offered.

**M3 — the shell renders.** `nethos-view` (WebKit) renders the shell to the
framebuffer, and a QEMU `screendump` contains non-uniform pixel content
matching the shell's background. Verified by: the screendump, checked
programmatically, not by eye.

**M3 is the mission.** M1 and M2 are how you get there and how you prove you
have not fooled yourself.

If M3 turns out to need something genuinely absent from nk, that is a result:
say exactly what is missing, prove it with a measurement, and keep working on
the next thing that is not blocked. Do not stop.

## Read before doing anything

- `docs/NK-HANDOFF.md` — the current state, the open bugs, the eight causes
  already eliminated by test, and the tooling traps. **Do not re-derive
  anything in it.**
- `docs/KERNEL.md` — nk's own documentation. Every stage and every bug that
  cost real time.
- `docs/DESIGN.md` — if you touch the shell, its rules are enforced.

## Plan first

Before writing any code, produce a written plan and save it to
`docs/DESKTOP-PLAN.md`:

1. The dependency graph from where we are to M3. What must be true before
   what.
2. Which pieces are **independent** and can run in parallel, and which are
   serialised behind a blocker.
3. For each parallel lane: its goal, the files it owns, and the single
   command that proves it done.
4. The riskiest unknown in each lane, and the cheapest experiment that would
   retire it.

Re-plan when a lane finishes or a hypothesis dies. Keep the file current — it
is how you recover after a compaction.

## Parallelism

Use subagents aggressively. The rules that make it safe:

- **One git worktree per lane.** `git worktree add ../nk-lane-<name> <sha>`.
  Never let two agents edit the same working tree. Symlink the shared build
  directory in rather than rebuilding it:
  `ln -s <repo>/kernel/ldk/build <worktree>/kernel/ldk/build`.
- **One disk image per agent.** `cp kernel/ldk/build/npkg.img /tmp/<lane>.img`
  before booting. Two QEMUs on one image fail with
  `Failed to get "write" lock`.
- **Lanes own files, not tasks.** Two lanes must never need the same file. If
  they do, the split is wrong — re-cut it.
- **Integrate often.** A lane that has not merged in an hour is a lane whose
  work is about to conflict.

Good lane cuts, given the current state:

| lane | owns | proves |
| --- | --- | --- |
| kernel | `kernel/core/src/**` | the `#16` race is closed |
| daemon | `payload/nethosd/**`, probes | M1 |
| compositor | rootfs builders, `scripts/build-*.sh` | M2 |
| shell | `payload/shell/**`, `payload/lib/**` | M3 |

The compositor and shell lanes can build rootfs images and study what is
needed while the kernel lane works. They must not be idle waiting.

## How to be right

These rules exist because breaking them cost this project a day.

- **Measure before concluding.** Never state a cause you have not tested.
  "I believe X" and "I measured X" are different sentences; write the one
  that is true.
- **Never assert a baseline you have not run.** If you say "this worked
  before commit N", you must have run it at commit N, in this session, and
  seen the line.
- **Never truncate output you are about to conclude from.** A `head -8` once
  hid the line that mattered and sent another agent to the wrong file for an
  hour.
- **A fix that does not change the symptom did not fix it.** Say so, commit
  it as a correct-but-unrelated fix if it is one, and keep looking.
- **Prefer eliminating a cause to guessing one.** Every negative result is
  progress and must be written into `docs/NK-HANDOFF.md` so no other lane
  repeats it.
- **Three outcomes from one binary and one image is a race**, not a
  threshold. Stop looking for a size or content trigger.

## When you need to look something up

There is nobody to ask, so research is your escalation path for anything you
do not know. You have web tools; use them properly.

**Discovery, then fetch.** `hound_mcp_smart_search` runs ten backends in
parallel and neural-reranks the results, but it returns **URLs, scores and
snippets — not page content**. Never answer from a snippet. Search, then
`hound_mcp_smart_fetch` the top one or two with `focus='<your question>'` so
you get the relevant blocks rather than the whole page. `site:`,
`exclude_sites` and `freshness` are there when a search is too noisy.

**Multi-page documentation** — a Wayland protocol reference, a WebKit build
guide, an API surface — is `hound_mcp_smart_crawl`, not repeated fetches. The
cheap shape is two phases: `sitemap=true` to map the URLs in one request, then
`crawl_urls=[...]` for only the pages you actually want, with `focus=` to
prioritise and filter. A single page is always `smart_fetch`; do not crawl for
one page.

**Check `content_ok` before you believe anything.** If it is false, the
content is not the page — branch on `next_action` and `page_type` instead of
reading a bot wall and concluding the API changed. PDFs (specifications,
papers) come back as structured markdown; use `pages='1-5'` and the
`table_of_contents` rather than pulling a 400-page document into context.
`cache_ttl=0` forces a fresh fetch of one URL; `hound_cache_clear` is for when
the whole cache is stale.

`hound_mcp_screenshot` is for visual layout questions only. You are a text
agent for almost everything here — fetch the page.

**What is actually worth researching in this project**, and what is not:

Worth it — Wayland protocol semantics and what a compositor must implement;
weston or wlroots backend options for a machine with no input devices and a
DRM dumb-buffer display; WebKit/WPE build requirements and which of them nk
cannot satisfy; the exact contract of a Linux interface you are implementing;
decoding an ESR or a descriptor bit you are not certain of.

Not worth it — anything about *this* kernel. nk is ours and nothing on the web
knows how it behaves. A blog post cannot tell you why `poll` wedges after a
UDP round trip on nk; only a measurement can.

**The ordering that keeps you honest:**

1. This repository's own docs. `docs/KERNEL.md` has cost people days already.
2. The primary source — the Linux tree in the docker volume, the glibc
   binary, the protocol XML. Read the code, not an article about the code.
3. The web, for things genuinely outside this machine.

A measurement on this machine beats a web page every time. If a documented
behaviour and an observed one disagree, the observation is what is true here,
and the disagreement itself is the finding — write it down.

## Tooling traps that will waste your time

- The test suite is **already red on `Npkg`** at HEAD with no local changes.
  That is bug `#16`, not a regression. Check before blaming yourself.
- Use **`grep -a`**. Console logs contain NUL bytes and plain grep calls them
  binary and prints nothing, which reads exactly like "it never happened".
- **Do not use `--include` with busybox grep** (Alpine images). It silently
  matches nothing. Use `debian:trixie-slim` for searching the kernel tree.
- **Never pipe a build through `grep`** — you get the pipeline's exit status
  and a failed build looks successful.
- The LKL tree is a **docker volume that survives builds**, so every patch is
  a migration, not an edit. `patch-lkl.py` rewrites the values it wants on
  every run for that reason.
- Colima **only mounts `$HOME`**. A bind mount from `/private/tmp` silently
  shows an empty directory.

## Never stop

- Do not end a turn with "waiting for", "blocked on", or a question. There is
  nobody to answer.
- If a lane blocks, spawn a subagent to attack the blocker from a different
  angle while you work on something else. There is always something not
  blocked: a probe to write, a negative to record, a rootfs to build, a
  measurement to take.
- If you are out of ideas on the hard bug, go and get more data. Add
  instrumentation, bisect, write a smaller repro. "No ideas" means "not
  enough measurements".
- If you are out of ideas because you do not know something — how a protocol
  works, what a library needs, what a bit means — that is a research problem,
  not a dead end. Search it, read the primary source, and come back with the
  fact. See "When you need to look something up".
- Every ten iterations: update `docs/DESKTOP-PLAN.md`, commit, and re-plan
  against what you now know.
- **Never claim a milestone you have not verified with its own command.**
  A fabricated success ends the run with the desktop still broken, which is
  the only genuinely unrecoverable failure here.

## Committing

Commit early and often, on `nk-initrd-builder`. Stage files explicitly —
never `git add -A` at the repo root; the tree carries other people's work.
Say in the message what you measured, not what you hoped. Push when a lane
lands.
