"""Authored protocol cases; real Claude captures are validated separately."""

import concurrent.futures
import fcntl
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

PLUGIN = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PLUGIN / "scripts"))
from correlate import correlate
import observer
from projection import EVENTS, META
from storage import capture, encode

SESSION = "00000000-0000-4000-8000-000000000001"
PROMPT = "00000000-0000-4000-8000-000000000002"
CHILD = "a0123456789abcdef"
CALL = "toolu_01SyntheticASRCall"
SECRET = "PRIVATE_SENTINEL_DO_NOT_PERSIST"


def event(name, **extra):
    return {"hook_event_name": name, "session_id": SESSION, "prompt_id": PROMPT,
            "cwd": "/private/" + SECRET, "transcript_path": "/private/" + SECRET,
            **extra}


class ObserverTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.out = self.root / "out"

    def tearDown(self):
        self.temp.cleanup()

    def run_hook(self, value, raw=False):
        result = subprocess.run([sys.executable, "-I", "-S", str(PLUGIN / "scripts/observer.py")],
                                input=value if raw else encode(value), capture_output=True,
                                env={**os.environ, "CTX_ASR_DIR": str(self.out)}, timeout=2)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"", b""))
        return result

    def rows(self, session=SESSION):
        path = self.out / (session + ".asr.jsonl")
        return [json.loads(line) for line in path.read_bytes().splitlines()] if path.exists() else []

    def dispatch(self, value):
        # Semantic assertions must not require successful capture within 75 ms
        # on a loaded CI host. Exercise the actual entry point with only its
        # timer disabled; subprocess cases below retain the production alarm.
        with mock.patch.dict(os.environ, {"CTX_ASR_DIR": str(self.out)}), \
                mock.patch.object(sys, "stdin", SimpleNamespace(buffer=io.BytesIO(encode(value)))), \
                mock.patch("signal.signal"), mock.patch("signal.setitimer"):
            observer.main()

    def emit(self, name, **extra):
        capture(str(self.out), SESSION, event(name, **extra), SESSION)

    def test_prompt_tools_lifecycle_and_exact_keys(self):
        self.dispatch(event("SessionStart", source="startup"))
        self.dispatch(event("UserPromptSubmit", prompt=SECRET))
        self.dispatch(event("PreToolUse", tool_name="Bash", tool_input={"command": SECRET}, tool_use_id=CALL))
        self.dispatch(event("PostToolUse", tool_name="Bash", tool_use_id=CALL,
                            tool_response={"stdout": SECRET}, duration_ms=0))
        self.dispatch(event("SessionEnd"))
        rows = self.rows()
        self.assertEqual(rows[0]["producer"]["method"], "observer")
        self.assertNotIn("startedAt", rows[0])
        self.assertNotIn("lineage", rows[0])
        self.assertEqual(rows[0]["contentLevel"], "metadata")
        self.assertEqual([r["seq"] for r in rows], list(range(len(rows))))
        calls = [r for r in rows if r["type"] == "tool_call"]
        results = [r for r in rows if r["type"] == "tool_result"]
        self.assertEqual(calls[0]["callId"], CALL)
        self.assertNotIn("requestedBy", calls[0])
        self.assertEqual((results[0]["linkage"], results[0]["durationMs"]), ("exact", 0))
        self.assertEqual(results[0]["capture"]["content"]["state"], "absent")
        self.assertNotIn("permission", results[0])
        self.assertEqual(rows[-1]["state"], "ended")
        for row in rows:
            self.assertNotIn("ts", row)
            self.assertEqual(row["idScope"], "capture")
            if row["type"] != "session":
                self.assertEqual(row["turnId"], PROMPT)
        for path in self.out.iterdir():
            self.assertNotIn(SECRET.encode(), path.read_bytes())
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.out.stat().st_mode & 0o777, 0o700)

    def test_every_event_discards_secret_surfaces(self):
        for name in sorted(EVENTS):
            value = event(name, tool_name=SECRET, tool_input={"password": SECRET},
                          tool_response={"nested": {"content": SECRET}}, tool_use_id=CALL,
                          message=SECRET, prompt=SECRET, title=SECRET, command_args=SECRET,
                          last_assistant_message=SECRET, error=SECRET, error_details=SECRET,
                          new_cwd="/" + SECRET, old_cwd="/" + SECRET, directory="/" + SECRET,
                          compact_summary=SECRET, custom_instructions=SECRET,
                          mcp_server={"name": SECRET}, mcp_server_name=SECRET,
                          requested_schema={"default": SECRET}, content={"secret": SECRET},
                          url="https://example.invalid/" + SECRET, agent_type=SECRET,
                          agent_id=CHILD, final=True, delta=SECRET, action="accept",
                          permission_suggestions=[{"value": SECRET}],
                          background_tasks=[{"command": SECRET}], session_crons=[{"prompt": SECRET}],
                          unknown_field=SECRET, native={"record": SECRET}, _meta={"unknown": SECRET})
            capture(str(self.out), SESSION, value, SESSION)
        for path in self.out.iterdir():
            self.assertNotIn(SECRET.encode(), path.read_bytes(), path.name)
        for row in self.rows():
            self.assertNotIn("native", row)

    def test_no_permission_or_completion_guesses(self):
        self.emit("PermissionRequest", tool_name="Bash", tool_input={})
        self.emit("PostToolUse", tool_use_id=CALL, tool_response={})
        self.emit("Stop", stop_hook_active=False, last_assistant_message="done")
        self.emit("PreCompact", trigger="auto")
        rows = self.rows()
        permission = next(r for r in rows if r["type"] == "permission")
        self.assertEqual(permission["status"], "pending")
        self.assertNotIn("subject", permission)
        self.assertNotIn("decision", permission)
        self.assertFalse(any(r["type"] in {"state", "compaction"} for r in rows))
        self.emit("PermissionDenied", tool_use_id=CALL)
        self.emit("PostCompact", trigger="auto", compact_summary="text")
        self.assertEqual(self.rows()[-1]["replaces"], {"state": "unknown"})

    def test_child_streams_and_revision_chain(self):
        self.dispatch(event("SubagentStart", agent_id=CHILD))
        self.dispatch(event("PreToolUse", agent_id=CHILD, tool_name="Read", tool_use_id=CALL))
        self.dispatch(event("SubagentStop", agent_id=CHILD))
        self.dispatch(event("PostToolUse", tool_name="Agent", tool_use_id="toolu_Parent",
                            tool_response={"agentId": CHILD, "status": "completed"}))
        announcements = [r for r in self.rows() if r["type"] == "subagent"]
        self.assertEqual(len(announcements), 3)
        self.assertEqual(announcements[1]["supersedes"], announcements[0]["id"])
        self.assertEqual(announcements[2]["supersedes"], announcements[1]["id"])
        self.assertEqual(announcements[2]["status"], "completed")
        child = self.rows(CHILD)
        self.assertEqual(child[0]["sessionId"], CHILD)
        self.assertEqual(child[0]["scope"], "subagent")
        self.assertNotIn("lineage", child[0])
        self.assertFalse(any(r["type"] == "tool_call" for r in self.rows()))

    def test_duplicate_call_and_unlinked_result(self):
        for _ in range(2):
            self.emit("PreToolUse", tool_use_id=CALL, tool_name="Read")
        self.emit("PostToolUseFailure", tool_use_id=CALL, is_interrupt=True)
        self.assertEqual(self.rows()[-1]["linkage"], "ambiguous")
        self.assertEqual(self.rows()[-1]["status"], "interrupted")
        self.emit("PostToolUse")
        self.assertEqual(self.rows()[-1]["linkage"], "none")

    def test_common_cwd_tracks_changes_when_cwd_hook_does_not_fire(self):
        self.emit("SessionStart", cwd="/scratch/one")
        self.emit("PreToolUse", cwd="/scratch/two", tool_name="Read", tool_use_id=CALL)
        self.emit("PostToolUse", cwd="/scratch/one", tool_use_id=CALL)
        rows = self.rows()
        roots = [root for r in rows if r["type"] == "declaration" for root in r.get("roots", [])]
        states = [r["cwd"] for r in rows if r["type"] == "workspace_state"]
        self.assertEqual(len(roots), 2)
        self.assertEqual(len(states), 3)
        self.assertEqual(states[0], states[2])
        self.assertNotEqual(states[0], states[1])
        for path in self.out.iterdir():
            self.assertNotIn(b"/scratch", path.read_bytes())

    def test_display_folds_and_elicitation_keeps_no_answers(self):
        for final in [False, False, True]:
            self.emit("MessageDisplay", final=final, message_id=PROMPT, delta=SECRET)
        messages = [r for r in self.rows() if r["type"] == "message"]
        self.assertEqual(len(messages), 1)
        self.assertEqual(messages[0]["assembly"], {"mode": "chunks"})
        self.assertNotEqual(messages[0]["messageId"], PROMPT)
        self.emit("Elicitation", elicitation_id="request1", mode="form")
        pending = self.rows()[-1]
        self.emit("ElicitationResult", elicitation_id="request1", mode="form",
                  action="accept", content={"password": SECRET})
        answer = self.rows()[-1]
        self.assertEqual(answer["supersedes"], pending["id"])
        self.assertEqual(answer["status"], "answered")
        self.assertNotIn("response", answer)

    def test_malformed_and_oversized_fail_open(self):
        for raw in [b"bad", b"{}", b"null", b'{}\n{}', b'"' + b'x' * 1048576,
                    b'{"session_id":"one","session_id":"two"}',
                    b'{"a":NaN}', b'[' * 2000]:
            self.run_hook(raw, raw=True)
        self.assertFalse(self.rows())
        self.run_hook(event("SessionStart", session_id="../../escape"))
        self.assertFalse(self.rows())

    def test_deadline_fails_open_on_stalled_input(self):
        proc = subprocess.Popen([sys.executable, "-I", "-S", str(PLUGIN / "scripts/observer.py")],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                env={**os.environ, "CTX_ASR_DIR": str(self.out)})
        try:
            # Keep stdin open without sending EOF: the production timer must
            # end the blocked read without a hook error or emitted decision.
            self.assertEqual(proc.wait(timeout=2), 0)
            self.assertEqual(proc.communicate(), (b"", b""))
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.communicate()

    def test_no_wait_on_lock_or_unsafe_output(self):
        self.emit("SessionStart")
        before = (self.out / (SESSION + ".asr.jsonl")).read_bytes()
        with open(self.out / (SESSION + ".lock"), "rb") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            self.run_hook(event("UserPromptSubmit", prompt="hello"))
        self.assertEqual((self.out / (SESSION + ".asr.jsonl")).read_bytes(), before)
        self.out.chmod(0o755)
        self.run_hook(event("SessionEnd"))
        self.assertEqual((self.out / (SESSION + ".asr.jsonl")).read_bytes(), before)

    def test_symlinks_hardlinks_and_fifo_are_not_written(self):
        self.out.mkdir(mode=0o700)
        target = self.root / "private"
        target.write_bytes(b"unchanged")
        path = self.out / (SESSION + ".asr.jsonl")
        path.symlink_to(target)
        self.run_hook(event("SessionStart"))
        self.assertEqual(target.read_bytes(), b"unchanged")
        path.unlink()
        os.mkfifo(path, 0o600)
        self.run_hook(event("SessionStart"))
        path.unlink()
        tmp = self.out / (SESSION + ".state.tmp")
        os.link(target, tmp)
        self.run_hook(event("SessionStart"))
        self.assertEqual(target.read_bytes(), b"unchanged")

    def test_append_prefix_and_stale_index_stop_capture(self):
        self.emit("SessionStart")
        path = self.out / (SESSION + ".asr.jsonl")
        before = path.read_bytes()
        self.emit("UserPromptSubmit")
        self.assertTrue(path.read_bytes().startswith(before))
        with path.open("ab") as out:
            out.write(b'{"torn":')
        before = path.read_bytes()
        self.run_hook(event("SessionEnd"))
        self.assertEqual(path.read_bytes(), before)

    def test_limit_is_explicit_and_terminal(self):
        self.emit("SessionStart")
        with mock.patch("storage.MAX_STREAM", 1):
            self.emit("UserPromptSubmit")
        self.assertEqual(self.rows()[-1]["type"], "gap")
        before = self.rows()
        self.emit("SessionEnd")
        self.assertEqual(self.rows(), before)

    def test_concurrent_processes_preserve_valid_prefix(self):
        self.emit("SessionStart")
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            list(pool.map(lambda _: self.run_hook(event("UserPromptSubmit")), range(16)))
        rows = self.rows()
        self.assertEqual([r["seq"] for r in rows], list(range(len(rows))))
        self.assertEqual(sum(r["type"] == "session" for r in rows), 1)
        self.assertEqual(len({r["id"] for r in rows}), len(rows))

    def test_manifest_is_passive_and_tail_hooks_are_bounded(self):
        hooks = json.loads((PLUGIN / "hooks/hooks.json").read_text())["hooks"]
        self.assertEqual(set(hooks), EVENTS)
        for event_name in ["WorktreeCreate", "WorktreeRemove", "FileChanged"]:
            self.assertNotIn(event_name, hooks)
        for event_name, matchers in hooks.items():
            handler = matchers[0]["hooks"][0]
            self.assertEqual(handler["onFailure"], "continue")
            self.assertEqual(handler["timeout"], 1)
            self.assertEqual(handler.get("async", False), event_name not in {"Stop", "SubagentStop", "SessionEnd"})

    def test_uuid_correlation_is_scoped_and_never_guesses(self):
        self.emit("UserPromptSubmit")
        self.emit("PreToolUse", tool_name="Read", tool_use_id=CALL)
        self.emit("PostToolUse", tool_use_id=CALL)
        self.emit("PermissionRequest")
        self.emit("MessageDisplay", final=True, message_id=PROMPT)
        native = self.root / "native.jsonl"
        user = {"type": "user", "sessionId": SESSION, "uuid": "native-user", "promptId": PROMPT,
                "message": {"content": SECRET}}
        tool = {"type": "assistant", "sessionId": SESSION, "uuid": "native-call",
                "message": {"content": [{"type": "tool_use", "id": CALL}]}}
        result = {"type": "user", "sessionId": SESSION, "uuid": "native-result",
                  "message": {"content": [{"type": "tool_result", "tool_use_id": CALL}]}}
        native.write_bytes(b"".join(encode(r) for r in [user, tool, result, tool,
                          {**tool, "uuid": "child-call", "agentId": CHILD}]))
        stream = self.out / (SESSION + ".asr.jsonl")
        links = list(correlate(stream, [native]))
        self.assertIn(["native-call"], [r["nativeRecordUuids"] for r in links])
        self.assertNotIn("child-call", json.dumps(links))
        self.assertNotIn(SECRET, json.dumps(links))
        self.assertTrue(all(r["status"] == "unresolved" for r in links if r["basis"] == "session"))
        with native.open("ab") as out:
            out.write(encode({**tool, "uuid": "duplicate-call"}))
        links = list(correlate(stream, [native]))
        self.assertTrue(any(r["status"] == "ambiguous" for r in links if r["basis"] == "call"))


if __name__ == "__main__":
    unittest.main()
