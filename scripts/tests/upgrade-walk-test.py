#!/usr/bin/env python3
"""Offline runner regressions, not evidence of released-binary compatibility."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("upgrade_walk", ROOT / "scripts/release/upgrade-walk.py")
WALK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WALK)
VERSIONS = ["1.6.3", "1.6.4", "2.0.4"]


class UpgradeWalkTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.walk = WALK.Walk(self.root, VERSIONS, 1)
        self.commands = []
        self.current = None
        self.updates = iter(VERSIONS[1:])
        self.tamper = False
        self.missing_history = False
        self.terminal_status = "up_to_date"

    def release(self, version):
        return {
            "url": f"{WALK.METADATA_BASE}/{version}/ctx-release-metadata.env",
            "sha256": hashlib.sha256(version.encode()).hexdigest(),
            "metadata_sha256": "offline-unit-stub",
        }

    def command(self, label, args, env=None):
        self.walk.report["stage"] = label
        environment = self.walk.env if env is None else env
        self.commands.append((args, environment.copy()))
        if args[0] == "sh":
            self.current = "1.6.3"
            self.walk.binary.parent.mkdir(parents=True, exist_ok=True)
            self.walk.binary.write_text(self.current)
            return ""
        self.assertEqual(args[0], str(self.walk.binary))
        self.assertNotIn("CTX_RELEASE_METADATA_URL", environment)
        self.assertEqual(environment["CTX_DAEMON_ENABLED"], "false")
        if args[1] == "--version":
            return f"ctx {self.current}\n"
        if args[1] == "upgrade":
            self.assertEqual(args[1:], ["upgrade", "--format", "json"])
            version = next(self.updates, None)
            if isinstance(version, Exception):
                raise version
            if version:
                self.current = version
                self.walk.binary.write_text("wrong bytes" if self.tamper else version)
            return json.dumps({"ok": True, "applied": version is not None,
                               "update_available": False,
                               "status": "upgraded" if version else self.terminal_status})
        if args[1] == "import":
            self.assertEqual(environment["CTX_DAEMON_AUTOSTART_OFF"], "0")
            self.assertNotIn("--no-daemon", args)
            return "{}"
        if args[1] == "daemon":
            self.assertEqual(args[1:], ["daemon", "disable", "--format", "json"])
            self.assertNotEqual(Path(environment["CTX_DATA_ROOT"]), Path(environment["HOME"]) / ".ctx")
            return '{"running":false}'
        if args[1] == "search":
            self.assertEqual(args[-4:], ["--refresh", "off", "--format", "json"])
            return json.dumps({"retrieval": {"requested_mode": "lexical", "effective_mode": "lexical"},
                               "results": [] if self.missing_history else [{"snippet": "Add a parser test."}]})
        self.fail(f"unexpected command: {args}")

    def execute_offline(self):
        with mock.patch.object(self.walk, "run", side_effect=self.command), \
                mock.patch.object(self.walk, "release", side_effect=self.release), \
                mock.patch.object(self.walk, "download", side_effect=lambda label, url, path: path.write_text("installer")):
            self.walk.execute()

    def test_one_install_real_updater_calls_each_hop_and_terminal(self):
        self.execute_offline()
        self.assertEqual(self.walk.report["status"], "passed")
        installs = [(args, env) for args, env in self.commands if args[0] == "sh"]
        self.assertEqual(len(installs), 1)
        args, env = installs[0]
        self.assertEqual(args[2:], ["--no-setup", "--no-skill", "--no-man", "--no-modify-path"])
        self.assertEqual(env["CTX_RELEASE_METADATA_URL"], self.release("1.6.3")["url"])
        self.assertEqual(sum(args[1] == "upgrade" for args, _ in self.commands), 3)
        self.assertEqual(sum(args[1] == "import" for args, _ in self.commands), 1)
        self.assertEqual(sum(args[1] == "daemon" for args, _ in self.commands), 1)
        self.assertEqual(sum(args[1] == "search" for args, _ in self.commands), 4)
        self.assertEqual([item["version"] for item in self.walk.report["snapshots"]], VERSIONS + ["2.0.4"])
        self.assertFalse(self.walk.report["automatic_cadence_tested"])

    def test_failed_import_still_stops_its_worker_and_preserves_error(self):
        original = self.command

        def command(label, args, env=None):
            if label == "import":
                self.walk.report["stage"] = label
                raise subprocess.TimeoutExpired(args, 1)
            return original(label, args, env)

        with mock.patch.object(self, "command", side_effect=command):
            with self.assertRaises(subprocess.TimeoutExpired):
                self.execute_offline()
        self.assertEqual(sum(args[1] == "daemon" for args, _ in self.commands), 1)
        self.assertEqual(self.walk.report["stage"], "import")

    def test_feed_skipping_bridge_is_not_repaired_or_reinstalled(self):
        self.updates = iter(["2.0.4"])
        with self.assertRaisesRegex(RuntimeError, "expected ctx 1.6.4, got ctx 2.0.4"):
            self.execute_offline()
        self.assertEqual(sum(args[0] == "sh" for args, _ in self.commands), 1)
        self.assertEqual(self.walk.report["status"], "failed")

    def test_updater_size_failure_surfaces_before_expected_version_check(self):
        self.updates = iter([RuntimeError("artifact exceeds 128 MiB")])
        with self.assertRaisesRegex(RuntimeError, "artifact exceeds 128 MiB"):
            self.execute_offline()
        self.assertEqual(self.walk.report["observed_versions"], ["ctx 1.6.3", "ctx 1.6.3"])
        self.assertEqual(sum(args[1] == "upgrade" for args, _ in self.commands), 1)
        self.assertEqual(self.walk.report["failed_hop_retention"]["status"], "passed")
        self.assertEqual(self.walk.report["stage"], "hop-1")

    def test_failed_retention_does_not_mask_original_updater_error(self):
        original = self.command

        def command(label, args, env=None):
            if label == "hop-1":
                self.walk.report["stage"] = label
                self.walk.binary.write_text("damaged")
                self.missing_history = True
                raise RuntimeError("original updater failure")
            return original(label, args, env)

        with mock.patch.object(self, "command", side_effect=command):
            with self.assertRaisesRegex(RuntimeError, "original updater failure"):
                self.execute_offline()
        retention = self.walk.report["failed_hop_retention"]
        self.assertEqual(retention["status"], "failed")
        self.assertIn("changed executable", retention["binary_error"])
        self.assertIn("no longer searchable", retention["history_error"])

    def test_reported_version_cannot_hide_wrong_artifact(self):
        self.tamper = True
        with self.assertRaisesRegex(RuntimeError, "checksum differs"):
            self.execute_offline()

    def test_missing_history_and_nonterminal_feed_fail(self):
        self.missing_history = True
        with self.assertRaisesRegex(RuntimeError, "no longer searchable"):
            self.execute_offline()
        self.missing_history = False
        self.terminal_status = "available"
        with self.assertRaisesRegex(RuntimeError, "did not report up_to_date"):
            self.execute_offline()

    def test_environment_discards_credentials_feed_overrides_and_provider_roots(self):
        with mock.patch.dict(os.environ, {"CTX_RELEASE_METADATA_URL": "https://fake.invalid",
                                         "CTX_ANALYTICS_ENABLED": "true", "BASH_ENV": "/secret",
                                         "CODEX_HOME": "/real-history", "TOKEN": "private"}):
            env = WALK.isolated_environment(self.root)
        for key in ("TOKEN", "BASH_ENV", "CTX_RELEASE_METADATA_URL", "HTTPS_PROXY"):
            self.assertNotIn(key, env)
        for key in ("HOME", "CTX_DATA_ROOT", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME",
                    "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "TMPDIR", "CODEX_HOME", "CLAUDE_CONFIG_DIR"):
            self.assertTrue(Path(env[key]).is_relative_to(self.root))
        self.assertEqual(env["CTX_ANALYTICS_ENABLED"], "false")
        self.assertEqual(env["CTX_UPGRADE_AUTO"], "off")

    def test_failed_subprocess_retains_diagnostic_and_timeout_is_bounded(self):
        with self.assertRaisesRegex(RuntimeError, "exited 7"):
            self.walk.run("failure", [sys.executable, "-c", "import sys; print('size limit', file=sys.stderr); sys.exit(7)"])
        self.assertIn("size limit", (self.root / "01-failure.stderr").read_text())
        with self.assertRaises(subprocess.TimeoutExpired):
            self.walk.run("timeout", [sys.executable, "-c", "import time; time.sleep(30)"])
        self.assertTrue(self.walk.report["commands"][-1]["timed_out"])

    def test_signed_metadata_parser_does_not_evaluate_shell_or_accept_duplicates(self):
        self.assertEqual(WALK.parse_metadata("# comment\nA=value\nB=$(false)\n"),
                         {"A": "value", "B": "$(false)"})
        for invalid in ("A=x\nA=y", "missing separator", " A=x", "A= x"):
            with self.assertRaisesRegex(RuntimeError, "invalid or duplicate"):
                WALK.parse_metadata(invalid)

    @unittest.skipUnless(sys.platform == "linux", "uses Linux process state to distinguish zombies")
    def test_exited_parent_does_not_leave_term_ignoring_descendant(self):
        program = """
import os, signal, time
reader, writer = os.pipe()
child = os.fork()
if child == 0:
    os.close(reader)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    os.write(writer, b'ready')
    while True:
        time.sleep(1)
os.close(writer)
os.read(reader, 5)
print(child, flush=True)
"""
        child = int(self.walk.run("orphan", [sys.executable, "-c", program]).strip())
        def cleanup():
            try:
                os.kill(child, signal.SIGKILL)
            except ProcessLookupError:
                pass
        self.addCleanup(cleanup)
        # SIGKILL is asynchronous; allow only a bounded scheduling interval.
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            try:
                state = Path(f"/proc/{child}/stat").read_text().split(") ", 1)[1].split()[0]
            except FileNotFoundError:
                return
            if state == "Z":
                return
            time.sleep(0.01)
        self.fail("runner left a live descendant after the command parent exited")


if __name__ == "__main__":
    unittest.main()
