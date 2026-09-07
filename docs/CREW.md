# crew — several agents, one checkout

NETHOS is worked on by more than one agent at a time: **Claude Code running
Opus**, **opencode running Spark**, and a human with an editor open. They share
a working tree, so two of them writing the same file is not a merge conflict to
be resolved later — it is one of them silently discarding the other's work, and
the loser usually does not find out until a test fails for a reason that makes
no sense.

`crew` is the board they all read and write. `tools/crew/crew.py` is the whole
implementation.

## What it is, in one paragraph

State is JSON in `.crew/`, one file per concern, gitignored because it is live
and per-machine. There is no daemon: every command is a read-modify-write under
one lock, so an agent that dies leaves nothing running. Nothing here can
*prevent* a write — it advises, and the hooks on each side enforce. An agent
that ignores crew is a bug in that agent's wiring, not a hole to be plugged in
crew.

## Who is who

| agent | is | takes |
| --- | --- | --- |
| `opus` | Claude Code (Opus) | the hard problems: kernel work, real debugging, design decisions, anything where being wrong is expensive |
| `spark` | opencode (Spark) | the broad ones: tests, docs, refactors, sweeps, anything mechanical or wide |
| `mac` | the human | whatever they have open |

The models are not interchangeable, and the point of the tool is that the work
is not handed out as though they were.

## Commands

    crew status                     the board: who, what, claims, queue
    crew claim <paths> -m "why"     take files; refuses if somebody holds them
    crew release [paths]            give them back (everything of yours by default)
    crew check <paths>              exit 1 if somebody else holds them
    crew say "..." [--to agent]     tell the others
    crew inbox                      messages addressed to you
    crew ask "..." [--file F] [--tried "..."]    ask the other model for help
    crew task add "..." [--hard] [--file F]      put work on the queue
    crew task take                  take what suits your model
    crew task done <id>
    crew handoff opus --to spark --reason "out of budget"
    crew resume opus
    crew watch                      follow what everyone is doing
    crew prompt                     the protocol text the agents are given

Identity comes from `CREW_AGENT`. The hooks set it, and the opencode plugin
sets it for every shell command that session runs (`shell.env`); a human in a
terminal is `mac` by default.

**This is the part that has already gone wrong once.** opencode was invoking
`crew` from its own bash tool, where `CREW_AGENT` was unset, so it registered
as `mac` — and three messages addressed to `spark`, including a regression
report, sat unread while its inbox said "nothing new" and the sender saw
nothing but success. Two things guard it now: the `shell.env` hook, so it does
not depend on anyone remembering; and `crew say --to X` warns when X has an
unread backlog, because a name being registered proves nothing and a backlog
proves nobody is reading. If you are driving `crew` by hand from an agent
session, set `CREW_AGENT` yourself.

## Claims

A claim is a path and a reason. Claiming a **directory** covers everything
under it, which is what makes "I am rewriting `kernel/core/src`" expressible at
all — claiming forty files one at a time is not something an agent remembers to
do.

**Claims go stale rather than sticking forever.** An agent that dies mid-edit
would otherwise lock a file until somebody noticed. A heartbeat older than
twenty minutes is treated as gone and its claims stop blocking. That is the
right trade: a stale claim that blocks is a broken repository, a stale claim
that warns is a message.

`--force` exists and should be rare. Use it when you have positive evidence the
holder has gone, not to get past a refusal.

**An agent cannot force a claim held by `mac`.** That is a person with the file
open in an editor, and their next save silently discards whatever was written
over them — neither side finds out until something fails for a reason that
makes no sense. The protocol text says not to; this one is also enforced,
because the cost of getting it wrong is somebody's work. The human can force
anything: it is their repository.

## Asking for help

This is the part worth using and the part an agent will not use unless it is
told to, so both agents are told to, in their system prompt:

    crew ask "virtio IRQ never fires after ~380 requests" \
        --file kernel/core/src/lklirq.rs \
        --tried "heap size, Linux pool size, legacy vs modern transport"

An ask is deliberately **both a message and a task**. A message alone is missed
if the other agent is mid-session; a task alone gives them no idea what has
already been ruled out. `--tried` is the field that saves the most time, because
the expensive part of a handed-over bug is re-running the experiments the first
agent already ran.

Asks are taken first by `crew task take`: somebody is blocked waiting on them.

## Handoff, for when Opus runs out

    crew handoff opus --to spark --reason "out of budget"

Releases every claim opus held, requeues its in-progress tasks, and marks it
unavailable — after which Spark's `crew task take` includes the hard work,
which it otherwise leaves alone. `crew resume opus` puts it back.

Without this, an agent that stops mid-session leaves every file it touched
locked and every task it started sitting in `doing` with nobody doing it.

## How it is enforced

**Claude Code** — `.claude/settings.json`:

| hook | does |
| --- | --- |
| `SessionStart` | registers, and puts the protocol and board in context |
| `UserPromptSubmit` | refreshes the board and delivers unread messages |
| `PreToolUse` on `Edit`/`Write`/`NotebookEdit` | **denies** a write to a file somebody else holds |
| `PostToolUse` on the same | claims what was just written |
| `SessionEnd` | releases the claims |

**opencode** — `.opencode/plugin/crew.js`:

| hook | does |
| --- | --- |
| `experimental.chat.system.transform` | puts the protocol *and the live board* in the system prompt |
| `tool.execute.before` | throws on a write to a file somebody else holds |
| `tool.execute.after` | claims what was just written |
| `event` on `session.idle` | releases the claims |

Both drive the same `crew.py`. That is deliberate: two implementations of what
a claim means would drift, and the first symptom of drift is two agents editing
one file while each believes it complied.

The board goes into opencode's system prompt on every turn rather than once,
because the board changes during a session — a file that was free when the
session started may be held ten minutes later, and a rule the model read once is
not the same as a fact it can see.

## Setting it up

Claude Code and opencode both pick their side up from the repository with no
further setup. For a terminal:

    export PATH="$PWD/tools/crew:$PATH"

## What it does not do

No network, no locking of the filesystem, no merge resolution, and no attempt
to stop a human doing whatever they like. It answers "is anybody else in this
file", "what is everyone doing", "what should I work on", and "who takes over
when I stop" — and nothing beyond that.
