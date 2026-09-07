/**
 * Exercises .opencode/plugin/crew.js without running opencode.
 *
 * Worth having as well as tests/test_crew.py: the Python tests cover what
 * crew decides, and this covers what the plugin does with the answer. The
 * distinction is not academic -- it caught the plugin treating "crew is not
 * installed" as "everything is claimed", which refused every write in any
 * checkout without the tool.
 *
 * Stands in for Bun's `$`, which interpolates an array as separate arguments;
 * the plugin relies on that when passing a list of paths.
 *
 *   node tests/crew_plugin_harness.mjs <repo-with-crew> <plugin.js>
 */
import { execFileSync, } from "node:child_process";
import { readFileSync } from "node:fs";

const [, , ROOT, PLUGIN] = process.argv;
if (!ROOT || !PLUGIN) {
  console.error("usage: crew_plugin_harness.mjs <root> <plugin.js>");
  process.exit(2);
}
const { CrewPlugin } = await import(PLUGIN);

const mk = (opts = {}) => (strings, ...values) => {
  const argv = [];
  strings.forEach((s, i) => {
    for (const w of s.trim().split(/\s+/).filter(Boolean)) argv.push(w);
    if (i < values.length) {
      const v = values[i];
      if (Array.isArray(v)) argv.push(...v.map(String));
      else if (v !== undefined) argv.push(String(v));
    }
  });
  const run = () => {
    try {
      const out = execFileSync(argv[0], argv.slice(1), {
        env: { ...process.env, ...(opts.env || {}) },
        encoding: "utf8", stdio: ["ignore", "pipe", "pipe"],
      });
      return { exitCode: 0, stdout: out, stderr: "" };
    } catch (e) {
      return { exitCode: e.status ?? 1, stdout: e.stdout ?? "", stderr: e.stderr ?? "" };
    }
  };
  const p = Promise.resolve().then(run);
  p.quiet = () => p;
  p.nothrow = () => p;
  return p;
};
const $ = Object.assign((opts) => mk(opts), mk());
const claims = () => JSON.parse(readFileSync(`${ROOT}/.crew/claims.json`, "utf8"));
const crewAs = (agent, ...args) =>
  execFileSync("python3", [`${ROOT}/tools/crew/crew.py`, ...args],
    { env: { ...process.env, CREW_AGENT: agent, CREW_ROOT: ROOT }, encoding: "utf8" });

let failed = 0;
const ok = (m) => console.log("ok  " + m);
const fail = (m) => { console.log("FAIL: " + m); failed++; };

const hooks = await CrewPlugin({ worktree: ROOT, directory: ROOT, $ });

// The protocol and the live board reach the model.
const out = { system: [] };
await hooks["experimental.chat.system.transform"]({}, out);
const sys = out.system.join("\n");
/crew ask/.test(sys) ? ok("system prompt tells it to ask for help")
                     : fail("system prompt never mentions asking for help");
/You are `spark`/.test(sys) ? ok("system prompt names the agent")
                            : fail("system prompt does not name the agent");
/The board, right now/.test(sys) ? ok("system prompt carries the live board")
                                 : fail("system prompt has no board");

// A free file is writable, and writing it takes it.
await hooks["tool.execute.before"]({ tool: "write" }, { args: { filePath: "a/b.rs" } });
ok("write to a free file allowed");
await hooks["tool.execute.after"]({ tool: "write", args: { filePath: "a/b.rs" } });
claims()["a/b.rs"]?.agent === "spark" ? ok("writing a file claims it")
                                      : fail("writing did not claim: " + JSON.stringify(claims()));

// A file another agent holds is not.
crewAs("opus", "claim", "c/d.rs", "-m", "threads");
let threw = null;
try {
  await hooks["tool.execute.before"]({ tool: "edit" }, { args: { filePath: "c/d.rs" } });
} catch (e) { threw = e; }
if (!threw) fail("write to a held file was not refused");
else if (!/opus/.test(threw.message)) fail("refusal does not name the holder");
else if (!/crew ask/.test(threw.message)) fail("refusal does not say what to do instead");
else ok("write to a held file refused, naming the holder and the way out");

// Reading is never refused: a claim is about writing.
await hooks["tool.execute.before"]({ tool: "read" }, { args: { filePath: "c/d.rs" } });
ok("reads are never refused");

// Going idle gives back its own files and nobody else's.
await hooks.event({ event: { type: "session.idle" } });
const after = claims();
after["a/b.rs"] ? fail("idle did not release its own claim") : ok("idle releases its own claims");
after["c/d.rs"] ? ok("idle leaves other agents' claims alone")
                : fail("idle released another agent's claim");

// A checkout with no crew installed must get no coordination, not a plugin
// that refuses everything. This is the bug this harness was written for.
const bare = await CrewPlugin({ worktree: "/nonexistent-checkout", directory: "/nonexistent-checkout", $ });
try {
  await bare["tool.execute.before"]({ tool: "write" }, { args: { filePath: "a/b.rs" } });
  ok("a checkout without crew installed is not blocked");
} catch (e) {
  fail("plugin refuses every write when crew is not installed: " + e.message);
}

process.exit(failed ? 1 : 0);
