/**
 * crew — the opencode half of the coordination described in docs/CREW.md.
 *
 * The other half is a set of Claude Code hooks in .claude/settings.json. Both
 * drive the same tool, tools/crew/crew.py, so there is one implementation of
 * what a claim means and one copy of the protocol text. A plugin that
 * reimplemented either would drift from the other side, and the first symptom
 * of drift is two agents editing one file while each believes it complied.
 *
 * What this does:
 *   - puts the protocol and the live board into the system prompt, so Spark
 *     knows who else is here without being told
 *   - refuses a write to a file another agent has claimed
 *   - claims a file automatically once Spark has written to it
 *   - releases everything when the session goes idle
 */

const AGENT = "spark";

import { existsSync } from "node:fs";

export const CrewPlugin = async ({ project, directory, worktree, $ }) => {
  const root = worktree || directory || project?.worktree || process.cwd();
  const crew = `${root}/tools/crew/crew.py`;

  // A checkout without crew installed gets no coordination, not a plugin that
  // refuses every write. This was a real bug: `check` exits non-zero both for
  // "somebody holds this" and for "python could not find the script", and
  // treating those the same made a missing file look like a permanent claim
  // on the entire repository.
  const installed = existsSync(crew);

  const run = async (...args) => {
    if (!installed) return { code: 0, out: "", err: "" };
    try {
      const r = await $({
        env: { ...process.env, CREW_AGENT: AGENT, CREW_ROOT: root },
      })`python3 ${crew} ${args}`.quiet().nothrow();
      return {
        code: r.exitCode ?? 0,
        out: (r.stdout?.toString?.() ?? "").trim(),
        err: (r.stderr?.toString?.() ?? "").trim(),
      };
    } catch {
      // Could not run it at all. Say nothing rather than block.
      return { code: 0, out: "", err: "" };
    }
  };

  // opencode's writing tools, and where each keeps the path it is about to
  // write. Names differ between versions, so this is matched loosely: a tool
  // this does not recognise is allowed through rather than blocked.
  const WRITES = /^(write|edit|patch|multiedit|apply_patch)$/i;

  const pathsOf = (args) => {
    if (!args || typeof args !== "object") return [];
    const out = [];
    for (const key of ["filePath", "file_path", "path", "notebook_path"]) {
      if (typeof args[key] === "string" && args[key]) out.push(args[key]);
    }
    for (const key of ["files", "edits"]) {
      if (Array.isArray(args[key])) {
        for (const e of args[key]) {
          if (typeof e === "string") out.push(e);
          else out.push(...pathsOf(e));
        }
      }
    }
    return out;
  };

  await run("register", AGENT);

  return {
    /**
     * The protocol, plus the board as it stands right now.
     *
     * Injected rather than left in AGENTS.md because the board changes during
     * a session: a file that was free when the session started may be held by
     * Opus ten minutes later, and a rule the model read once is not the same
     * as a fact it can see.
     */
    "experimental.chat.system.transform": async (_input, output) => {
      const prompt = await run("prompt");
      const board = await run("status");
      const parts = [];
      if (prompt.out) parts.push(prompt.out);
      if (board.out) parts.push("## The board, right now\n\n" + board.out);
      if (parts.length) {
        parts.push(
          "You are `spark`. Ask opus for help with anything subtle — a race, " +
            "a memory bug, an architectural decision, or a diagnosis that has " +
            "already resisted one attempt — using `crew ask`. That is expected " +
            "of you, not a failure.",
        );
        output.system.push(parts.join("\n\n"));
      }
    },

    /**
     * Every command this session runs in a shell knows who it is.
     *
     * Without this, `crew` invoked from the model's own bash tool has no
     * CREW_AGENT, falls back to the human default, and registers as `mac`.
     * That is not a cosmetic mislabel: a message sent `--to spark` then
     * goes to an agent nobody is running as, and the recipient's inbox says
     * "nothing new" forever. It cost three messages, including a regression
     * report, before anyone noticed.
     */
    "shell.env": async (_input, output) => {
      output.env.CREW_AGENT = AGENT;
      if (installed) output.env.CREW_ROOT = root;
    },

    /** Refuse a write to something another agent is in the middle of. */
    "tool.execute.before": async (input, output) => {
      if (!WRITES.test(input.tool)) return;
      const paths = pathsOf(output.args);
      if (!paths.length) return;
      const r = await run("check", ...paths);
      // Exactly 1 is "somebody else holds this". Anything else -- a crash, a
      // bad argument, a python that is not there -- is crew's problem and not
      // a reason to stop the model working.
      if (r.code === 1) {
        // Throwing is how a plugin refuses a tool call. The message is the
        // only thing the model sees, so it says what to do instead rather
        // than only what went wrong.
        throw new Error(
          `crew: ${r.err || "this file is claimed by another agent"}\n` +
            `Do not edit it. Work on something else, tell them with ` +
            `\`crew say\`, or ask with \`crew ask\`. Use ` +
            `\`crew claim <path> --force\` only if you know they have gone.`,
        );
      }
    },

    /** Having written it, hold it until the session is done with it. */
    "tool.execute.after": async (input) => {
      if (!WRITES.test(input.tool)) return;
      const paths = pathsOf(input.args);
      if (paths.length) await run("claim", ...paths, "-m", "editing", "-q");
    },

    /** Give everything back when the session stops working. */
    event: async ({ event }) => {
      if (event?.type === "session.idle" || event?.type === "session.deleted") {
        await run("release", "-q");
      }
    },
  };
};

export default CrewPlugin;
