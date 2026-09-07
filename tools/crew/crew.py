#!/usr/bin/env python3
"""
crew — one repository, several agents, no collisions.

NETHOS is worked on by more than one agent at a time: Claude Code running
Opus, opencode running Spark, and a human with an editor open. They share a
checkout, so two of them writing the same file is not a merge conflict to be
resolved later -- it is one of them silently discarding the other's work.

crew is the shared board they all read and write. It is deliberately small:

  * State is JSON in `.crew/`, one file per concern. If crew has a bug you can
    still answer "who holds this file" with `cat`.
  * There is no daemon. Every command is a read-modify-write under one lock,
    so an agent that crashes leaves nothing running and nothing to clean up.
  * Nothing here can *prevent* a write -- it advises, and the hooks on each
    side enforce. An agent that ignores crew is a bug in that agent's wiring,
    not a hole to be plugged here.

The five things it does:

    claim/release   who is editing what, so nobody else touches it
    say/inbox       messages between agents, and to the human
    task            a shared queue, with hard work routed to the stronger model
    handoff         when one agent runs out of budget, its work moves
    status          the board, in one screen

Claims go stale rather than sticking forever: an agent that dies mid-edit
would otherwise lock a file until somebody noticed. A heartbeat older than
STALE seconds is treated as gone, and its claims stop blocking anyone. That is
the right trade -- a stale claim that blocks is a broken repository, a stale
claim that warns is a message.
"""

from __future__ import annotations

import argparse
import errno
import json
import os
import shutil
import subprocess
import sys
import time

# How long an agent may go quiet before its claims stop blocking others.
# Twenty minutes is longer than any single tool call and shorter than any
# session anyone would leave open by accident.
STALE = 20 * 60

# Who the agents are, and what each is for. The point of the whole tool is in
# this table: the models are not interchangeable, so the work should not be
# handed out as though they were.
ROLES = {
    "opus":  "Claude Code (Opus) -- the hard problems: kernel, debugging, design",
    "spark": "opencode (Spark) -- the broad ones: tests, docs, refactors, sweeps",
    "mac":   "the human -- whatever they have open in an editor",
}

HUMAN = "mac"


# ---------------------------------------------------------------------------
# where the state lives
# ---------------------------------------------------------------------------

def repo_root(start: str | None = None) -> str:
    """The checkout crew is coordinating.

    Found by walking up for a `.git`, not by asking git: this runs from hooks
    in environments where the working directory is not the repository and
    where spawning git for every claim check would be the slowest thing in the
    edit path.
    """
    if os.environ.get("CREW_ROOT"):
        return os.path.realpath(os.environ["CREW_ROOT"])
    # realpath, not abspath: on macOS /tmp is a symlink to /private/tmp, so a
    # root and a file discovered by different routes disagree about their own
    # names and every relative path comes out as ../../private/...
    here = os.path.realpath(start or os.getcwd())
    while True:
        if os.path.isdir(os.path.join(here, ".git")):
            return here
        parent = os.path.dirname(here)
        if parent == here:
            # No repository: fall back to the working directory rather than
            # failing, so `crew` is still usable in a scratch tree.
            return os.path.realpath(start or os.getcwd())
        here = parent


def state_dir() -> str:
    d = os.path.join(repo_root(), ".crew")
    os.makedirs(d, exist_ok=True)
    return d


def path_of(name: str) -> str:
    return os.path.join(state_dir(), name)


class Lock:
    """A directory used as a mutex.

    `os.mkdir` is atomic on every filesystem this will ever run on, which
    `open(..., "x")` is not guaranteed to be over a network mount. The holder
    writes its pid so a stuck lock can be explained rather than just deleted.
    """

    def __init__(self, timeout: float = 5.0):
        self.dir = path_of("lock")
        self.timeout = timeout

    def __enter__(self):
        deadline = time.time() + self.timeout
        while True:
            try:
                os.mkdir(self.dir)
                with open(os.path.join(self.dir, "pid"), "w") as fh:
                    fh.write(str(os.getpid()))
                return self
            except OSError as exc:
                if exc.errno != errno.EEXIST:
                    raise
                if time.time() > deadline:
                    # Whoever held it is gone or wedged. Waiting longer helps
                    # nobody, and a coordination tool that hangs is worse than
                    # one that occasionally races.
                    shutil.rmtree(self.dir, ignore_errors=True)
                    continue
                time.sleep(0.02)

    def __exit__(self, *_):
        shutil.rmtree(self.dir, ignore_errors=True)
        return False


def load(name: str, default):
    try:
        with open(path_of(name)) as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return default


def save(name: str, data) -> None:
    """Write via a temporary file and rename, so a reader never sees half."""
    tmp = path_of(name + ".tmp")
    with open(tmp, "w") as fh:
        json.dump(data, fh, indent=2, sort_keys=True)
        fh.write("\n")
    os.replace(tmp, path_of(name))


def append_log(entry: dict) -> None:
    entry.setdefault("at", time.time())
    with open(path_of("log.jsonl"), "a") as fh:
        fh.write(json.dumps(entry, sort_keys=True) + "\n")


def read_log() -> list[dict]:
    out = []
    try:
        with open(path_of("log.jsonl")) as fh:
            for line in fh:
                line = line.strip()
                if line:
                    try:
                        out.append(json.loads(line))
                    except ValueError:
                        continue
    except OSError:
        pass
    return out


# ---------------------------------------------------------------------------
# identity
# ---------------------------------------------------------------------------

def whoami() -> str:
    """Which agent is running this.

    Explicit beats guessed: CREW_AGENT is what the hooks set, and the guesses
    below exist only so a human typing `crew status` in a terminal is not
    mistaken for one of the agents.
    """
    name = os.environ.get("CREW_AGENT")
    if name:
        return name.strip().lower()
    if os.environ.get("CLAUDECODE") or os.environ.get("CLAUDE_CODE_SESSION"):
        return "opus"
    if os.environ.get("OPENCODE") or os.environ.get("OPENCODE_SESSION"):
        return "spark"
    return HUMAN


def now() -> float:
    return time.time()


def ago(t: float) -> str:
    s = max(0, int(now() - t))
    if s < 60:
        return f"{s}s ago"
    if s < 3600:
        return f"{s // 60}m ago"
    return f"{s // 3600}h{(s % 3600) // 60:02d}m ago"


def agents() -> dict:
    return load("agents.json", {})


def touch(name: str, **fields) -> dict:
    """Record that an agent is alive, and whatever else it wants remembered."""
    with Lock():
        a = load("agents.json", {})
        me = a.setdefault(name, {"since": now(), "available": True})
        me["seen"] = now()
        me.update(fields)
        save("agents.json", a)
        return me


def is_live(record: dict) -> bool:
    return now() - record.get("seen", 0) < STALE


# ---------------------------------------------------------------------------
# the protocol, as the agents are told it
# ---------------------------------------------------------------------------

# One text, injected into both agents -- Claude Code through a SessionStart
# hook, opencode through a system-prompt transform. Written once here because
# two copies of a protocol drift, and the first symptom of drift is two agents
# editing the same file while each believes it followed the rules.
PROMPT = """\
# crew: you are not the only agent in this checkout

Another AI agent and the human who owns this repository are working in the
same files at the same time. Use `crew` -- `python3 tools/crew/crew.py`, or
`crew` if it is on PATH -- so you do not overwrite their work or repeat it.

Who is here:

  opus    Claude Code (Opus). The hard problems: kernel work, real debugging,
          design decisions, anything where being wrong is expensive.
  spark   opencode (Spark). The broad ones: tests, docs, refactors, sweeps,
          anything mechanical or wide.
  mac     the human. May have files open in an editor at any moment.

## The rules

1. **Before editing, claim.** `crew claim <paths> -m "what you are doing"`.
   It refuses if somebody else holds the file, and that refusal is the point.
   Claim a directory when you are working across one.
2. **When you are finished with a file, release it.** `crew release <paths>`,
   or `crew release` for everything you hold. A claim you forget is a file
   nobody else can touch.
3. **Say what you are doing.** `crew say "rewriting the ELF loader"`. The
   others see it. Say it when you start something big, when you finish, and
   when you learn something that changes what somebody else should do.
4. **Read your messages.** `crew inbox`. Do it when you start and whenever
   you are about to pick up something new.
5. **Take work from the queue rather than inventing it.** `crew task take`
   hands you the task that suits your model; `crew task list` shows the rest;
   `crew task done <id>` when it is finished.

## Ask for help -- this is expected, not a failure

If you are stuck, out of your depth, or about to spend a long time on
something the other model would do better, **ask**:

    crew ask "the virtio IRQ never fires after ~380 requests" --file kernel/core/src/lklirq.rs

That posts a message to the other agent and puts the problem on the queue
addressed to them. Spark should ask Opus for anything subtle: a race, a
memory bug, an architectural call, a diagnosis that has already resisted one
attempt. Opus should ask Spark for anything wide and mechanical: sweeping a
rename across a tree, writing out a test matrix, checking a hundred files.

Asking early is cheaper than being wrong slowly. A question costs one message.

## When an agent runs out of budget

`crew handoff opus --to spark --reason "out of budget"` releases everything
opus held and requeues its work for spark, which then takes hard tasks too.
`crew resume opus` when it is back. If you are told you are running out,
hand off before you stop rather than leaving claims behind.

## What crew cannot do

It advises; it does not lock the filesystem. If `crew` says a file is held,
that is a person or an agent actively editing it -- go and do something else.
Only use `--force` when you have positive evidence the holder has gone.
"""


def do_prompt(args) -> int:
    print(PROMPT)
    return 0


# ---------------------------------------------------------------------------
# claims
# ---------------------------------------------------------------------------

def normalise(p: str) -> str:
    """A repository-relative path, so two agents in different working
    directories are talking about the same file.

    A relative path normally means "relative to where I am", and that is what
    happens whenever the caller is inside the checkout. When it is not -- a
    hook or a plugin can run from anywhere -- the only sensible reading of
    `a/b.rs` is relative to the repository, and resolving it against some
    unrelated working directory produced claim keys like
    `../../../Users/mac/.../a/b.rs` that matched nothing.
    """
    root = repo_root()
    if os.path.isabs(p):
        ap = os.path.realpath(p)
    else:
        base = os.path.realpath(os.getcwd())
        if base != root and not base.startswith(root + os.sep):
            base = root
        ap = os.path.realpath(os.path.join(base, p))
    try:
        rel = os.path.relpath(ap, root)
    except ValueError:
        return ap
    # Genuinely outside the checkout: keep it absolute rather than describing
    # it as a walk up out of the repository, which two agents in different
    # directories would spell differently.
    if rel == ".." or rel.startswith(".." + os.sep):
        return ap
    return rel.replace(os.sep, "/")


def overlaps(a: str, b: str) -> bool:
    """Whether two claims are about the same work.

    A claim on a directory covers everything under it. That is what makes
    "I am rewriting kernel/core/src" expressible at all -- claiming forty
    files one at a time is not something an agent will remember to do.
    """
    a = a.rstrip("/")
    b = b.rstrip("/")
    return a == b or a.startswith(b + "/") or b.startswith(a + "/")


def claims() -> dict:
    return load("claims.json", {})


def blocking(paths: list[str], me: str) -> list[tuple[str, dict]]:
    """Claims held by somebody else that would collide with `paths`.

    Stale holders are skipped: see the note at the top about why a claim that
    blocks forever is worse than one that expires.
    """
    held = claims()
    live = agents()
    out = []
    for path, info in held.items():
        owner = info.get("agent")
        if owner == me:
            continue
        if not is_live(live.get(owner, {})):
            continue
        if any(overlaps(normalise(p), path) for p in paths):
            out.append((path, info))
    return out


def do_claim(args) -> int:
    me = whoami()
    paths = [normalise(p) for p in args.paths]
    with Lock():
        held = load("claims.json", {})
        live = load("agents.json", {})
        clashes = []
        for path, info in held.items():
            if info.get("agent") == me or not is_live(live.get(info.get("agent"), {})):
                continue
            if any(overlaps(p, path) for p in paths):
                clashes.append((path, info))
        if clashes and not args.force:
            for path, info in clashes:
                print(f"held by {info['agent']}: {path}"
                      f"{'  -- ' + info['note'] if info.get('note') else ''}",
                      file=sys.stderr)
            print("\nnot claimed. talk to them, or --force if they have "
                  "clearly gone.", file=sys.stderr)
            return 1
        for p in paths:
            held[p] = {"agent": me, "note": args.note or "", "at": now()}
        save("claims.json", held)
        a = load("agents.json", {})
        rec = a.setdefault(me, {"since": now(), "available": True})
        rec["seen"] = now()
        if args.note:
            rec["doing"] = args.note
        save("agents.json", a)
    append_log({"kind": "claim", "agent": me, "paths": paths, "note": args.note or ""})
    if not args.quiet:
        for p in paths:
            print(f"claimed {p}")
    return 0


def do_release(args) -> int:
    me = whoami()
    want = [normalise(p) for p in args.paths] if args.paths else None
    freed = []
    with Lock():
        held = load("claims.json", {})
        for path in list(held):
            if held[path].get("agent") != me:
                continue
            if want is None or any(overlaps(path, w) for w in want):
                del held[path]
                freed.append(path)
        save("claims.json", held)
    if freed:
        append_log({"kind": "release", "agent": me, "paths": freed})
    if not args.quiet:
        for p in freed:
            print(f"released {p}")
        if not freed:
            print("nothing to release")
    return 0


def do_check(args) -> int:
    """Exit non-zero when somebody else holds one of these paths.

    Shaped for a shell `if`, because that is how the hooks use it.
    """
    me = whoami()
    clashes = blocking(args.paths, me)
    for path, info in clashes:
        print(f"held by {info['agent']}: {path}"
              f"{'  -- ' + info['note'] if info.get('note') else ''}",
              file=sys.stderr)
    return 1 if clashes else 0


# ---------------------------------------------------------------------------
# messages
# ---------------------------------------------------------------------------

def do_say(args) -> int:
    me = whoami()
    text = " ".join(args.text).strip()
    if not text:
        print("nothing to say", file=sys.stderr)
        return 2
    append_log({"kind": "message", "agent": me, "to": args.to or "", "text": text})
    touch(me)
    print(f"{me} -> {args.to or 'everyone'}: {text}")
    return 0


def unread(me: str) -> list[dict]:
    cursors = load("cursors.json", {})
    seen = cursors.get(me, 0)
    msgs = [e for e in read_log() if e.get("kind") == "message"]
    out = [m for m in msgs[seen:] if m.get("agent") != me
           and m.get("to", "") in ("", me)]
    return out


def mark_read(me: str) -> None:
    with Lock():
        cursors = load("cursors.json", {})
        cursors[me] = len([e for e in read_log() if e.get("kind") == "message"])
        save("cursors.json", cursors)


def do_inbox(args) -> int:
    me = whoami()
    msgs = [e for e in read_log() if e.get("kind") == "message"]
    if args.all:
        show = msgs[-args.n:]
    else:
        show = unread(me)
    if not show:
        print("nothing new")
    for m in show:
        to = f" (to {m['to']})" if m.get("to") else ""
        print(f"[{ago(m['at'])}] {m['agent']}{to}: {m['text']}")
    if not args.keep:
        mark_read(me)
    return 0


# ---------------------------------------------------------------------------
# tasks
# ---------------------------------------------------------------------------

def tasks() -> dict:
    return load("tasks.json", {"next": 1, "items": []})


def do_task_add(args) -> int:
    me = whoami()
    with Lock():
        t = load("tasks.json", {"next": 1, "items": []})
        item = {
            "id": t["next"],
            "title": " ".join(args.title).strip(),
            "hard": bool(args.hard),
            "files": [normalise(f) for f in (args.file or [])],
            "note": args.note or "",
            "state": "todo",
            "owner": args.own or "",
            "by": me,
            "at": now(),
        }
        t["next"] += 1
        t["items"].append(item)
        save("tasks.json", t)
    append_log({"kind": "task-add", "agent": me, "id": item["id"],
                "title": item["title"], "hard": item["hard"]})
    print(f"#{item['id']} {'[hard] ' if item['hard'] else ''}{item['title']}")
    return 0


def suited(item: dict, me: str, available: dict) -> bool:
    """Whether this agent should be the one to pick a task up.

    Hard work goes to opus while opus is available; when it is not -- out of
    budget, or handed off -- spark takes everything, because a queue nobody
    may touch is not a queue. This is the entire routing policy and it is
    meant to stay this small.
    """
    if item.get("owner"):
        return item["owner"] == me
    if not item.get("hard"):
        return True
    if me == "opus":
        return True
    return not available.get("opus", False)


def do_task_take(args) -> int:
    me = whoami()
    with Lock():
        t = load("tasks.json", {"next": 1, "items": []})
        a = load("agents.json", {})
        available = {n: r.get("available", True) and is_live(r) for n, r in a.items()}
        todo = [i for i in t["items"] if i["state"] == "todo"]
        if args.id is not None:
            chosen = next((i for i in todo if i["id"] == args.id), None)
        else:
            candidates = [i for i in todo
                          if args.any or suited(i, me, available)]
            # Order, most important first:
            #   asks addressed to me   -- somebody is blocked waiting
            #   work that suits me     -- opus prefers hard, others prefer not
            #   oldest                 -- so nothing starves
            # Sorted rather than picked in a loop because the loop version
            # took whichever came first and quietly ignored the priority.
            candidates.sort(key=lambda i: (
                not (i.get("ask") and i.get("owner") == me),
                (me == "opus") != bool(i.get("hard")),
                i["id"],
            ))
            chosen = candidates[0] if candidates else None
        if chosen is None:
            print("nothing suitable in the queue")
            return 1
        chosen["state"] = "doing"
        chosen["owner"] = me
        chosen["taken"] = now()
        save("tasks.json", t)
        rec = a.setdefault(me, {"since": now(), "available": True})
        rec["seen"] = now()
        rec["doing"] = chosen["title"]
        rec["task"] = chosen["id"]
        save("agents.json", a)
    append_log({"kind": "task-take", "agent": me, "id": chosen["id"],
                "title": chosen["title"]})
    print(f"#{chosen['id']} {chosen['title']}")
    if chosen.get("note"):
        print(f"  {chosen['note']}")
    if chosen.get("files"):
        print(f"  files: {' '.join(chosen['files'])}")
        print(f"  claim them with:  crew claim {' '.join(chosen['files'])} "
              f"-m {json.dumps(chosen['title'])}")
    return 0


def _set_task_state(ident: int, state: str, me: str) -> int:
    with Lock():
        t = load("tasks.json", {"next": 1, "items": []})
        for item in t["items"]:
            if item["id"] == ident:
                item["state"] = state
                if state == "todo":
                    item["owner"] = ""
                item["at"] = now()
                save("tasks.json", t)
                append_log({"kind": "task-" + state, "agent": me,
                            "id": ident, "title": item["title"]})
                print(f"#{ident} {state}: {item['title']}")
                return 0
    print(f"no task #{ident}", file=sys.stderr)
    return 1


def do_task_done(args) -> int:
    return _set_task_state(args.id, "done", whoami())


def do_task_drop(args) -> int:
    return _set_task_state(args.id, "todo", whoami())


def do_task_list(args) -> int:
    t = tasks()
    items = [i for i in t["items"] if args.all or i["state"] != "done"]
    if not items:
        print("queue empty")
        return 0
    for i in items:
        mark = {"todo": " ", "doing": ">", "done": "x"}[i["state"]]
        who = f" @{i['owner']}" if i.get("owner") else ""
        hard = " [hard]" if i.get("hard") else ""
        print(f" {mark} #{i['id']}{hard} {i['title']}{who}")
        if args.verbose and i.get("note"):
            print(f"      {i['note']}")
    return 0


def other_agent(me: str) -> str:
    """Who to ask. Two agents, so this is not a routing problem yet -- but it
    is the one place that assumption lives, so widening it later is one
    function rather than a search."""
    return "spark" if me == "opus" else "opus"


def do_ask(args) -> int:
    """Escalate to the other model.

    Deliberately both a message and a task: a message alone is missed if the
    other agent is mid-session, and a task alone gives them no idea it is
    urgent or what has already been tried.
    """
    me = whoami()
    to = (args.to or other_agent(me)).lower()
    text = " ".join(args.text).strip()
    if not text:
        print("ask what?", file=sys.stderr)
        return 2
    with Lock():
        t = load("tasks.json", {"next": 1, "items": []})
        item = {
            "id": t["next"],
            "title": text,
            "hard": to == "opus",
            "files": [normalise(f) for f in (args.file or [])],
            "note": (args.tried or "") and f"already tried: {args.tried}",
            "state": "todo",
            "owner": to,
            "by": me,
            "ask": True,
            "at": now(),
        }
        t["next"] += 1
        t["items"].append(item)
        save("tasks.json", t)
    append_log({"kind": "message", "agent": me, "to": to,
                "text": f"[asking for help] {text}"
                        + (f" (already tried: {args.tried})" if args.tried else "")
                        + f"  -- queued as #{item['id']}"})
    touch(me)
    print(f"asked {to}: {text}")
    print(f"queued as #{item['id']}, addressed to {to}")
    if item["files"]:
        print(f"  files: {' '.join(item['files'])}")
    print(f"\ncarry on with something else; {to} picks this up with "
          f"`crew task take`.")
    return 0


# ---------------------------------------------------------------------------
# handoff
# ---------------------------------------------------------------------------

def do_handoff(args) -> int:
    """One agent is out. Give its work to whoever is left.

    The case this exists for: Opus runs out of budget mid-session. Its claims
    would otherwise block Spark from every file it had touched, and its
    in-progress tasks would sit in `doing` forever with nobody doing them.
    """
    who = args.agent.lower()
    me = whoami()
    moved, freed = [], []
    with Lock():
        a = load("agents.json", {})
        rec = a.setdefault(who, {"since": now()})
        rec["available"] = False
        rec["reason"] = args.reason or "handed off"
        rec["handed"] = now()
        save("agents.json", a)

        held = load("claims.json", {})
        for path in list(held):
            if held[path].get("agent") == who:
                del held[path]
                freed.append(path)
        save("claims.json", held)

        t = load("tasks.json", {"next": 1, "items": []})
        for item in t["items"]:
            if item.get("owner") == who and item["state"] == "doing":
                item["state"] = "todo"
                item["owner"] = args.to or ""
                item["note"] = ((item.get("note", "") + " ").strip()
                                + f"[handed off from {who}]").strip()
                moved.append(item["id"])
        save("tasks.json", t)
    append_log({"kind": "handoff", "agent": me, "who": who,
                "to": args.to or "", "tasks": moved, "freed": freed,
                "text": args.reason or ""})
    print(f"{who} is out ({args.reason or 'handed off'})")
    if freed:
        print(f"  released {len(freed)} claim(s): {' '.join(freed)}")
    if moved:
        print(f"  requeued task(s): {' '.join('#' + str(m) for m in moved)}")
    if not freed and not moved:
        print("  nothing was held")
    print(f"\n{args.to or 'whoever is left'} now takes hard tasks too: "
          f"crew task take")
    return 0


def do_resume(args) -> int:
    who = args.agent.lower()
    with Lock():
        a = load("agents.json", {})
        rec = a.setdefault(who, {"since": now()})
        rec["available"] = True
        rec["seen"] = now()
        rec.pop("reason", None)
        save("agents.json", a)
    append_log({"kind": "resume", "agent": whoami(), "who": who})
    print(f"{who} is back")
    return 0


# ---------------------------------------------------------------------------
# the board
# ---------------------------------------------------------------------------

def board(brief: bool = False) -> str:
    out = []
    a = agents()
    held = claims()
    t = tasks()

    live = [(n, r) for n, r in sorted(a.items()) if is_live(r)]
    if live:
        out.append("agents:")
        for name, rec in live:
            state = "" if rec.get("available", True) else \
                f"  OUT ({rec.get('reason', 'handed off')})"
            doing = rec.get("doing") or "-"
            out.append(f"  {name:<6} {doing}{state}   ({ago(rec.get('seen', 0))})")
    else:
        out.append("agents: nobody registered")

    mine = [(p, i) for p, i in sorted(held.items()) if is_live(a.get(i.get("agent"), {}))]
    if mine:
        out.append("")
        out.append("claimed -- do not edit these:")
        for path, info in mine:
            note = f"  -- {info['note']}" if info.get("note") else ""
            out.append(f"  {info['agent']:<6} {path}{note}")

    asks = [i for i in t["items"]
            if i.get("ask") and i["state"] == "todo"]
    if asks:
        out.append("")
        out.append("someone has asked for help:")
        for i in asks:
            out.append(f"  #{i['id']} {i['by']} -> {i['owner']}: {i['title']}")

    open_items = [i for i in t["items"] if i["state"] != "done"]
    if open_items and not brief:
        out.append("")
        out.append("queue:")
        for i in open_items:
            mark = ">" if i["state"] == "doing" else " "
            who = f" @{i['owner']}" if i.get("owner") else ""
            hard = " [hard]" if i.get("hard") else ""
            out.append(f"  {mark} #{i['id']}{hard} {i['title']}{who}")
    elif open_items:
        doing = len([i for i in open_items if i["state"] == "doing"])
        out.append("")
        out.append(f"queue: {len(open_items)} open, {doing} in progress")

    return "\n".join(out)


def do_status(args) -> int:
    me = whoami()
    print(board(brief=args.brief))
    new = unread(me)
    if new:
        print("")
        print(f"messages for {me}:")
        for m in new:
            print(f"  [{ago(m['at'])}] {m['agent']}: {m['text']}")
    return 0


def do_register(args) -> int:
    me = args.agent.lower() if args.agent else whoami()
    touch(me, model=args.model or ROLES.get(me, ""), available=True,
          pid=os.getpid())
    append_log({"kind": "register", "agent": me})
    print(f"registered {me}")
    return 0


def do_watch(args) -> int:
    """Follow the log. The human's view of what the agents are up to."""
    seen = 0
    try:
        while True:
            entries = read_log()
            for e in entries[seen:]:
                kind = e.get("kind", "?")
                if kind == "message":
                    to = f" -> {e['to']}" if e.get("to") else ""
                    print(f"{e['agent']}{to}: {e['text']}")
                elif kind in ("claim", "release"):
                    print(f"{e['agent']} {kind}s {' '.join(e.get('paths', []))}"
                          f"{'  -- ' + e['note'] if e.get('note') else ''}")
                elif kind.startswith("task-"):
                    print(f"{e['agent']} {kind[5:]} #{e.get('id')} "
                          f"{e.get('title', '')}")
                elif kind == "handoff":
                    print(f"** {e.get('who')} is out: {e.get('text', '')}")
                else:
                    print(f"{e.get('agent', '?')} {kind}")
            seen = len(entries)
            time.sleep(args.interval)
    except KeyboardInterrupt:
        return 0


def do_reset(args) -> int:
    if not args.yes:
        print("this clears every claim, message and task. pass --yes.",
              file=sys.stderr)
        return 2
    shutil.rmtree(state_dir(), ignore_errors=True)
    print("board cleared")
    return 0


# ---------------------------------------------------------------------------
# editor integration
# ---------------------------------------------------------------------------

def paths_from_tool(tool: str, data: dict) -> list[str]:
    """Which files a tool call is about to write.

    Every agent spells this differently, and a guard that only understands one
    of them is a guard that silently passes the others.
    """
    out = []
    for key in ("file_path", "filePath", "path", "notebook_path"):
        v = data.get(key)
        if isinstance(v, str) and v:
            out.append(v)
    for key in ("edits", "files"):
        v = data.get(key)
        if isinstance(v, list):
            for entry in v:
                if isinstance(entry, str):
                    out.append(entry)
                elif isinstance(entry, dict):
                    out.extend(paths_from_tool(tool, entry))
    return out


WRITE_TOOLS = {"edit", "write", "notebookedit", "multiedit", "apply_patch",
               "str_replace_editor", "patch", "create"}


def do_hook(args) -> int:
    """Claude Code's hook protocol: JSON in on stdin, JSON out on stdout.

    Kept in this file rather than in shell wrappers so the rules about what
    counts as a write, and what a refusal says, exist once.
    """
    try:
        data = json.loads(sys.stdin.read() or "{}")
    except ValueError:
        return 0
    me = whoami()
    event = args.event.lower()
    tool = str(data.get("tool_name", "")).lower()
    tool_input = data.get("tool_input", {}) or {}

    if event == "pretooluse":
        if tool not in WRITE_TOOLS:
            return 0
        paths = paths_from_tool(tool, tool_input)
        if not paths:
            return 0
        clashes = blocking(paths, me)
        if not clashes:
            return 0
        who = clashes[0][1]["agent"]
        detail = "; ".join(
            f"{p} is claimed by {i['agent']}"
            + (f" ({i['note']})" if i.get("note") else "")
            for p, i in clashes)
        print(json.dumps({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason":
                f"crew: {detail}. Do not edit it. Either work on something "
                f"else, or tell {who} with `crew say`. If you are certain "
                f"they have finished, `crew claim <path> --force`.",
        }}))
        return 0

    if event == "posttooluse":
        if tool not in WRITE_TOOLS:
            return 0
        paths = [normalise(p) for p in paths_from_tool(tool, tool_input)]
        if not paths:
            return 0
        with Lock():
            held = load("claims.json", {})
            fresh = []
            for p in paths:
                if held.get(p, {}).get("agent") not in (me,):
                    fresh.append(p)
                held[p] = {"agent": me,
                           "note": held.get(p, {}).get("note", "") or "editing",
                           "at": now()}
            save("claims.json", held)
            a = load("agents.json", {})
            rec = a.setdefault(me, {"since": now(), "available": True})
            rec["seen"] = now()
            save("agents.json", a)
        if fresh:
            append_log({"kind": "claim", "agent": me, "paths": fresh,
                        "note": "auto"})
        return 0

    if event in ("sessionstart", "userpromptsubmit"):
        touch(me, available=True)
        text = board(brief=(event == "userpromptsubmit"))
        new = unread(me)
        if new:
            text += "\n\nmessages for you:\n" + "\n".join(
                f"  {m['agent']}: {m['text']}" for m in new)
            mark_read(me)
        if not new and event == "userpromptsubmit" and not claims():
            # Nothing to say. Staying quiet keeps the noise out of a session
            # where nobody else is working.
            return 0
        key = "SessionStart" if event == "sessionstart" else "UserPromptSubmit"
        print(json.dumps({"hookSpecificOutput": {
            "hookEventName": key,
            "additionalContext":
                "crew board (other agents share this checkout):\n" + text,
        }}))
        return 0

    if event in ("sessionend", "stop"):
        # Claims are released but the agent stays registered: it may be about
        # to be resumed, and forgetting it would lose the handoff state.
        args2 = argparse.Namespace(paths=[], quiet=True)
        do_release(args2)
        return 0

    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main(argv=None) -> int:
    p = argparse.ArgumentParser(
        prog="crew",
        description="coordination between the agents sharing this checkout")
    sub = p.add_subparsers(dest="cmd")

    s = sub.add_parser("status", help="the board")
    s.add_argument("--brief", action="store_true")
    s.set_defaults(fn=do_status)

    s = sub.add_parser("register", help="announce an agent")
    s.add_argument("agent", nargs="?")
    s.add_argument("--model")
    s.set_defaults(fn=do_register)

    s = sub.add_parser("claim", help="take files, so nobody else edits them")
    s.add_argument("paths", nargs="+")
    s.add_argument("-m", "--note", help="what you are doing to them")
    s.add_argument("--force", action="store_true",
                   help="take them even if somebody else holds them")
    s.add_argument("-q", "--quiet", action="store_true")
    s.set_defaults(fn=do_claim)

    s = sub.add_parser("release", help="give files back (all of yours by default)")
    s.add_argument("paths", nargs="*")
    s.add_argument("-q", "--quiet", action="store_true")
    s.set_defaults(fn=do_release)

    s = sub.add_parser("check", help="exit 1 if somebody else holds these")
    s.add_argument("paths", nargs="+")
    s.set_defaults(fn=do_check)

    s = sub.add_parser("say", help="tell the others something")
    s.add_argument("text", nargs="+")
    s.add_argument("--to", help="one agent, rather than everyone")
    s.set_defaults(fn=do_say)

    s = sub.add_parser("inbox", help="messages addressed to you")
    s.add_argument("--all", action="store_true", help="including read ones")
    s.add_argument("--keep", action="store_true", help="do not mark as read")
    s.add_argument("-n", type=int, default=20)
    s.set_defaults(fn=do_inbox)

    s = sub.add_parser("ask", help="ask the other agent for help")
    s.add_argument("text", nargs="+")
    s.add_argument("--to", help="which agent (defaults to the other one)")
    s.add_argument("--file", action="append", help="files it concerns")
    s.add_argument("--tried", help="what you have already ruled out")
    s.set_defaults(fn=do_ask)

    s = sub.add_parser("prompt", help="the protocol both agents are given")
    s.set_defaults(fn=do_prompt)

    s = sub.add_parser("handoff", help="an agent is out; move its work")
    s.add_argument("agent")
    s.add_argument("--to", help="who picks it up")
    s.add_argument("--reason")
    s.set_defaults(fn=do_handoff)

    s = sub.add_parser("resume", help="an agent is back")
    s.add_argument("agent")
    s.set_defaults(fn=do_resume)

    s = sub.add_parser("watch", help="follow what everyone is doing")
    s.add_argument("--interval", type=float, default=1.0)
    s.set_defaults(fn=do_watch)

    s = sub.add_parser("hook", help="editor integration (JSON on stdin)")
    s.add_argument("event")
    s.set_defaults(fn=do_hook)

    s = sub.add_parser("reset", help="clear the board")
    s.add_argument("--yes", action="store_true")
    s.set_defaults(fn=do_reset)

    t = sub.add_parser("task", help="the shared queue")
    tsub = t.add_subparsers(dest="taskcmd")

    s = tsub.add_parser("add")
    s.add_argument("title", nargs="+")
    s.add_argument("--hard", action="store_true",
                   help="needs the stronger model")
    s.add_argument("--file", action="append", help="files it will touch")
    s.add_argument("--note")
    s.add_argument("--own", help="assign to a specific agent")
    s.set_defaults(fn=do_task_add)

    s = tsub.add_parser("list")
    s.add_argument("--all", action="store_true")
    s.add_argument("-v", "--verbose", action="store_true")
    s.set_defaults(fn=do_task_list)

    s = tsub.add_parser("take")
    s.add_argument("--id", type=int)
    s.add_argument("--any", action="store_true",
                   help="ignore which model a task is meant for")
    s.set_defaults(fn=do_task_take)

    s = tsub.add_parser("done")
    s.add_argument("id", type=int)
    s.set_defaults(fn=do_task_done)

    s = tsub.add_parser("drop", help="put it back in the queue")
    s.add_argument("id", type=int)
    s.set_defaults(fn=do_task_drop)

    args = p.parse_args(argv)
    if not getattr(args, "fn", None):
        if args.cmd == "task":
            t.print_help()
            return 2
        return do_status(argparse.Namespace(brief=False))
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
