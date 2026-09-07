"""crew: the coordination between the agents sharing this checkout.

Every test gets its own state directory, because the thing under test is
shared mutable state and a test that inherits another's board is testing
something nobody wrote.
"""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
CREW = ROOT / 'tools/crew/crew.py'


class CrewCase(unittest.TestCase):
    def setUp(self):
        self.repo = pathlib.Path(tempfile.mkdtemp(prefix='crew-'))
        (self.repo / '.git').mkdir()
        self.addCleanup(shutil.rmtree, self.repo, ignore_errors=True)

    def crew(self, agent, *args, **kw):
        env = dict(os.environ, CREW_AGENT=agent, CREW_ROOT=str(self.repo))
        return subprocess.run([sys.executable, str(CREW), *args],
                              cwd=self.repo, env=env, capture_output=True,
                              text=True, timeout=30, **kw)

    def board(self):
        return json.loads((self.repo / '.crew/claims.json').read_text())


class Claims(CrewCase):
    """Who is editing what, which is the whole reason the tool exists."""

    def test_a_claim_is_recorded(self):
        self.crew('opus', 'claim', 'a/b.rs', '-m', 'threads')
        self.assertEqual(self.board()['a/b.rs']['agent'], 'opus')

    def test_another_agent_is_refused(self):
        self.crew('opus', 'claim', 'a/b.rs', '-m', 'threads')
        r = self.crew('spark', 'claim', 'a/b.rs')
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('held by opus', r.stderr)
        # And the refusal did not quietly take it anyway.
        self.assertEqual(self.board()['a/b.rs']['agent'], 'opus')

    def test_the_same_agent_may_reclaim(self):
        self.crew('opus', 'claim', 'a/b.rs')
        self.assertEqual(self.crew('opus', 'claim', 'a/b.rs').returncode, 0)

    def test_a_directory_claim_covers_what_is_under_it(self):
        # The case that makes claims usable at all: "I am rewriting this
        # subtree" rather than forty individual files.
        self.crew('opus', 'claim', 'kernel/core', '-m', 'rewrite')
        r = self.crew('spark', 'claim', 'kernel/core/src/user.rs')
        self.assertNotEqual(r.returncode, 0)

    def test_a_file_claim_blocks_the_directory_above_it(self):
        self.crew('opus', 'claim', 'kernel/core/src/user.rs')
        self.assertNotEqual(self.crew('spark', 'claim', 'kernel').returncode, 0)

    def test_unrelated_paths_do_not_collide(self):
        self.crew('opus', 'claim', 'kernel/core')
        self.assertEqual(self.crew('spark', 'claim', 'docs').returncode, 0)

    def test_force_takes_it(self):
        self.crew('opus', 'claim', 'a/b.rs')
        self.assertEqual(
            self.crew('spark', 'claim', 'a/b.rs', '--force').returncode, 0)
        self.assertEqual(self.board()['a/b.rs']['agent'], 'spark')

    def test_release_frees_it(self):
        self.crew('opus', 'claim', 'a/b.rs')
        self.crew('opus', 'release')
        self.assertEqual(self.crew('spark', 'claim', 'a/b.rs').returncode, 0)

    def test_release_leaves_other_agents_claims_alone(self):
        self.crew('opus', 'claim', 'a/b.rs')
        self.crew('spark', 'claim', 'c/d.rs')
        self.crew('opus', 'release')
        self.assertIn('c/d.rs', self.board())

    def test_a_stale_agent_stops_blocking(self):
        # An agent that died mid-edit must not lock a file forever. Written by
        # ageing the heartbeat rather than by sleeping: a test that waits
        # twenty minutes is a test nobody runs.
        self.crew('opus', 'claim', 'a/b.rs')
        agents = self.repo / '.crew/agents.json'
        data = json.loads(agents.read_text())
        data['opus']['seen'] = 0
        agents.write_text(json.dumps(data))
        self.assertEqual(self.crew('spark', 'claim', 'a/b.rs').returncode, 0)


class Check(CrewCase):
    """The shape the hooks use: exit status, not output."""

    def test_free_is_zero(self):
        self.assertEqual(self.crew('opus', 'check', 'a/b.rs').returncode, 0)

    def test_mine_is_zero(self):
        self.crew('opus', 'claim', 'a/b.rs')
        self.assertEqual(self.crew('opus', 'check', 'a/b.rs').returncode, 0)

    def test_someone_elses_is_not(self):
        self.crew('opus', 'claim', 'a/b.rs')
        self.assertNotEqual(self.crew('spark', 'check', 'a/b.rs').returncode, 0)


class Messages(CrewCase):
    def test_a_message_reaches_the_other_agent(self):
        self.crew('opus', 'say', 'rewriting the ELF loader')
        self.assertIn('rewriting the ELF loader',
                      self.crew('spark', 'inbox').stdout)

    def test_you_do_not_receive_your_own(self):
        self.crew('opus', 'say', 'hello')
        self.assertIn('nothing new', self.crew('opus', 'inbox').stdout)

    def test_reading_marks_read(self):
        self.crew('opus', 'say', 'hello')
        self.crew('spark', 'inbox')
        self.assertIn('nothing new', self.crew('spark', 'inbox').stdout)

    def test_a_directed_message_goes_only_there(self):
        self.crew('opus', 'say', 'just for you', '--to', 'spark')
        self.assertIn('nothing new', self.crew('mac', 'inbox').stdout)
        self.assertIn('just for you', self.crew('spark', 'inbox').stdout)


class Tasks(CrewCase):
    """Routing: the reason the queue exists rather than a shared list."""

    def setUp(self):
        super().setUp()
        self.crew('opus', 'register')
        self.crew('spark', 'register')
        self.crew('mac', 'task', 'add', 'a subtle race', '--hard')
        self.crew('mac', 'task', 'add', 'tidy the docs')

    def test_opus_takes_the_hard_one(self):
        self.assertIn('a subtle race', self.crew('opus', 'task', 'take').stdout)

    def test_spark_leaves_it_alone(self):
        self.assertIn('tidy the docs', self.crew('spark', 'task', 'take').stdout)

    def test_spark_will_not_take_hard_work_while_opus_is_available(self):
        self.crew('spark', 'task', 'take')          # tidy the docs
        r = self.crew('spark', 'task', 'take')      # only the hard one is left
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('nothing suitable', r.stdout)

    def test_any_overrides_the_routing(self):
        self.crew('spark', 'task', 'take')
        self.assertEqual(
            self.crew('spark', 'task', 'take', '--any').returncode, 0)

    def test_done_takes_it_off_the_queue(self):
        self.crew('opus', 'task', 'take')
        self.crew('opus', 'task', 'done', '1')
        self.assertNotIn('a subtle race', self.crew('mac', 'task', 'list').stdout)


class Asking(CrewCase):
    """Escalation between models, which is the part an agent will not do
    unless the protocol tells it to."""

    def setUp(self):
        super().setUp()
        self.crew('opus', 'register')
        self.crew('spark', 'register')

    def test_an_ask_reaches_the_other_agent(self):
        self.crew('spark', 'ask', 'the IRQ never fires')
        self.assertIn('the IRQ never fires', self.crew('opus', 'inbox').stdout)

    def test_an_ask_is_also_queued_for_them(self):
        self.crew('spark', 'ask', 'the IRQ never fires')
        self.assertIn('the IRQ never fires',
                      self.crew('opus', 'task', 'take').stdout)

    def test_what_was_already_tried_is_carried_over(self):
        # The expensive part of a handed-over bug is re-running the
        # experiments the first agent already ran.
        self.crew('spark', 'ask', 'the IRQ never fires',
                  '--tried', 'heap size, pool size')
        self.assertIn('heap size', self.crew('opus', 'task', 'take').stdout)

    def test_an_ask_is_taken_before_ordinary_work(self):
        self.crew('mac', 'task', 'add', 'something else hard', '--hard')
        self.crew('spark', 'ask', 'the IRQ never fires')
        self.assertIn('the IRQ never fires',
                      self.crew('opus', 'task', 'take').stdout)

    def test_opus_can_ask_spark(self):
        self.crew('opus', 'ask', 'sweep the tree for the old flag name')
        self.assertIn('sweep the tree', self.crew('spark', 'inbox').stdout)


class Handoff(CrewCase):
    """What happens when Opus runs out of budget mid-session."""

    def setUp(self):
        super().setUp()
        self.crew('opus', 'register')
        self.crew('spark', 'register')
        self.crew('mac', 'task', 'add', 'a subtle race', '--hard')
        self.crew('opus', 'task', 'take')
        self.crew('opus', 'claim', 'kernel/core/src/user.rs', '-m', 'mid-edit')

    def test_the_claims_are_released(self):
        self.crew('mac', 'handoff', 'opus', '--to', 'spark')
        self.assertEqual(
            self.crew('spark', 'claim', 'kernel/core/src/user.rs').returncode, 0)

    def test_the_work_is_requeued(self):
        self.crew('mac', 'handoff', 'opus', '--to', 'spark')
        self.assertIn('a subtle race', self.crew('mac', 'task', 'list').stdout)

    def test_spark_now_takes_hard_work(self):
        self.crew('mac', 'handoff', 'opus', '--to', 'spark')
        self.assertIn('a subtle race', self.crew('spark', 'task', 'take').stdout)

    def test_resume_puts_it_back(self):
        self.crew('mac', 'handoff', 'opus', '--to', 'spark')
        self.crew('mac', 'resume', 'opus')
        self.crew('mac', 'task', 'add', 'another hard one', '--hard')
        self.crew('spark', 'task', 'take')   # takes the handed-over one
        r = self.crew('spark', 'task', 'take')
        self.assertNotEqual(r.returncode, 0)


class Hooks(CrewCase):
    """The editor integration, which is what makes any of this automatic."""

    def hook(self, agent, event, payload):
        env = dict(os.environ, CREW_AGENT=agent, CREW_ROOT=str(self.repo))
        return subprocess.run([sys.executable, str(CREW), 'hook', event],
                              input=json.dumps(payload), cwd=self.repo,
                              env=env, capture_output=True, text=True,
                              timeout=30)

    def edit(self, path):
        return {'tool_name': 'Edit', 'tool_input': {'file_path': path}}

    def test_a_write_to_a_free_file_is_allowed(self):
        r = self.hook('opus', 'pretooluse', self.edit('a/b.rs'))
        self.assertEqual(r.stdout.strip(), '')

    def test_a_write_to_a_held_file_is_denied(self):
        self.crew('spark', 'claim', 'a/b.rs', '-m', 'mine')
        r = self.hook('opus', 'pretooluse', self.edit(str(self.repo / 'a/b.rs')))
        out = json.loads(r.stdout)['hookSpecificOutput']
        self.assertEqual(out['permissionDecision'], 'deny')
        self.assertIn('spark', out['permissionDecisionReason'])

    def test_the_refusal_says_what_to_do_instead(self):
        # A denial the model cannot act on just gets retried.
        self.crew('spark', 'claim', 'a/b.rs')
        r = self.hook('opus', 'pretooluse', self.edit('a/b.rs'))
        reason = json.loads(r.stdout)['hookSpecificOutput']['permissionDecisionReason']
        self.assertIn('crew say', reason)

    def test_a_read_is_never_denied(self):
        self.crew('spark', 'claim', 'a/b.rs')
        r = self.hook('opus', 'pretooluse',
                      {'tool_name': 'Read', 'tool_input': {'file_path': 'a/b.rs'}})
        self.assertEqual(r.stdout.strip(), '')

    def test_writing_claims_the_file(self):
        self.hook('opus', 'posttooluse', self.edit(str(self.repo / 'a/b.rs')))
        self.assertEqual(self.board()['a/b.rs']['agent'], 'opus')

    def test_session_start_hands_over_the_board(self):
        self.crew('spark', 'claim', 'a/b.rs', '-m', 'mine')
        r = self.hook('opus', 'sessionstart', {})
        ctx = json.loads(r.stdout)['hookSpecificOutput']['additionalContext']
        self.assertIn('a/b.rs', ctx)
        self.assertIn('spark', ctx)

    def test_session_end_releases(self):
        self.crew('opus', 'claim', 'a/b.rs')
        self.hook('opus', 'sessionend', {})
        self.assertEqual(self.board(), {})


class Prompt(CrewCase):
    """One protocol text, injected into both agents. Two copies drift."""

    def test_it_names_the_agents_and_their_roles(self):
        out = self.crew('mac', 'prompt').stdout
        for word in ('opus', 'spark', 'mac'):
            self.assertIn(word, out)

    def test_it_tells_them_to_ask_for_help(self):
        out = self.crew('mac', 'prompt').stdout
        self.assertIn('crew ask', out)
        self.assertIn('expected, not a failure', out)

    def test_it_covers_the_handoff(self):
        self.assertIn('crew handoff', self.crew('mac', 'prompt').stdout)


class OpencodePlugin(CrewCase):
    """The opencode half, driven without opencode.

    Separate from everything above on purpose: those tests cover what crew
    decides, this covers what the plugin does with the answer. The difference
    caught a real bug -- the plugin read "crew is not installed" as "every
    file is claimed" and refused every write.
    """

    def test_the_plugin_behaves(self):
        node = shutil.which('node')
        if not node:
            self.skipTest('node not installed')
        (self.repo / 'tools/crew').mkdir(parents=True)
        shutil.copy(CREW, self.repo / 'tools/crew/crew.py')
        r = subprocess.run(
            [node, str(ROOT / 'tests/crew_plugin_harness.mjs'), str(self.repo),
             str(ROOT / '.opencode/plugin/crew.js')],
            capture_output=True, text=True, timeout=120)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


if __name__ == '__main__':
    unittest.main()
