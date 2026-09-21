#!/usr/bin/env python3
"""Hermetic harness regressions, not live/native installation evidence."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("linux_smoke", Path(__file__).with_name("install_linux_smoke.py"))
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)


def search_result():
    # Independent protocol example; never supplied to the real installer.
    return {"retrieval": {"requested_mode": "lexical", "effective_mode": "lexical"},
            "results": [{"provider": "codex", "snippet":
                         "Fix the onboarding bug and make sure local history search stays useful.",
                         "citations": [{"provider": "codex", "target_type": "event", "item_id": "ctx:event:abc",
                                        "ctx_event_id": "abc", "ctx_session_id": "def"}]}]}


class LinuxSmokeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="linux-smoke-harness-test-")
        self.root = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)

    def harness(self, **extra):
        inputs = {"CTX_INSTALL_SMOKE_RESULT": str(self.root / "report.json"), **extra}
        runner = smoke.Acceptance(inputs)
        self.addCleanup(lambda: runner.log.close())
        self.addCleanup(lambda: __import__("shutil").rmtree(runner.root, ignore_errors=True))
        return runner

    def test_environment_excludes_host_history_release_overrides_and_optouts(self):
        with patch.dict(os.environ, {"HOME": "/private/history", "CTX_DATA_ROOT": "/private/store",
                                     "CTX_RELEASE_METADATA_URL": "https://fake.invalid",
                                     "CTX_INSTALL_NO_SETUP": "1", "CTX_INSTALL_NO_DAEMON": "1",
                                     "BASH_ENV": "/private/inject", "DBUS_SESSION_BUS_ADDRESS": "host-bus"}):
            env = smoke.isolated_environment(self.root)
        for key in ("CTX_RELEASE_METADATA_URL", "CTX_INSTALL_NO_SETUP", "CTX_INSTALL_NO_DAEMON", "BASH_ENV"):
            self.assertNotIn(key, env)
        for key in ("HOME", "CTX_DATA_ROOT", "CODEX_HOME", "XDG_CONFIG_HOME", "XDG_RUNTIME_DIR"):
            self.assertTrue(Path(env[key]).is_relative_to(self.root))
        self.assertEqual(env["PATH"], "/usr/bin:/bin")
        self.assertEqual(env["CTX_INSTALL_NO_PRO_TRIAL"], "1")
        self.assertEqual(env["CTX_ANALYTICS_ENABLED"], "false")
        self.assertNotIn("CI", env)  # CI explicitly suppresses the product PATH integration.
        self.assertNotIn(".local/bin", Path(env["HOME"], ".bashrc").read_text())
        self.assertEqual(Path(env["XDG_RUNTIME_DIR"]).stat().st_mode & 0o777, 0o700)

    def test_native_platforms_and_unsupported_are_not_linux_passes(self):
        for system, machine, expected in (("Linux", "x86_64", "linux-x64"),
                                          ("Linux", "aarch64", "linux-aarch64"),
                                          ("Darwin", "x86_64", "macos-x64"),
                                          ("Darwin", "arm64", "macos-arm64")):
            with self.subTest(expected=expected), patch.object(smoke.platform, "system", return_value=system), \
                    patch.object(smoke.platform, "machine", return_value=machine):
                self.assertEqual(smoke.platform_id(), expected)
                if system == "Darwin":
                    runner = self.harness()
                    with contextlib.redirect_stdout(io.StringIO()):
                        self.assertEqual(runner.execute(), 1)
                    report = json.loads(runner.result_path.read_text())
                    self.assertEqual(report["platform"], expected)
                    self.assertTrue(report["blocked"])
                    self.assertFalse(any(report["checks"].values()))
                    self.assertEqual(report["failed_stage"], "preflight")

    def test_candidate_retains_exact_bytes_not_source_path(self):
        candidate = self.root / "proposed script.sh"
        candidate.write_bytes(b"#!/bin/sh\nprintf 'retained installer\\n'\n")
        runner = self.harness(CTX_INSTALL_SMOKE_SCRIPT=str(candidate))
        runner.installer()
        retained = Path(runner.report["installer_path"])
        self.assertNotEqual(retained, candidate)
        self.assertEqual(retained.read_bytes(), candidate.read_bytes())
        candidate.write_text("changed after acquisition")
        self.assertEqual(smoke.sha256(retained), runner.report["installer_sha256"])
        self.assertEqual(runner.report["mode"], "candidate")

    def test_candidate_must_be_absolute_regular_file(self):
        runner = self.harness(CTX_INSTALL_SMOKE_SCRIPT="relative.sh")
        with self.assertRaisesRegex(smoke.SmokeFailure, "absolute regular"):
            runner.installer()

    def test_live_download_always_uses_public_https_url(self):
        runner = self.harness(CTX_RELEASE_METADATA_URL="https://wrong.invalid", CTX_INSTALL_URL="https://wrong.invalid")
        def download(args, **_kwargs):
            self.assertEqual(args[-1], "https://ctx.rs/install")
            self.assertIn("--proto-redir", args)
            Path(args[args.index("--output") + 1]).write_text("#!/bin/sh\n")
        with patch.object(runner, "run", side_effect=download):
            runner.installer()
        self.assertEqual(runner.report["mode"], "live")
        self.assertIsNotNone(runner.report["installer_sha256"])

    def test_citation_requires_fixture_hit_lexical_mode_and_event_identity(self):
        self.assertEqual(smoke.cited_event(search_result()), "abc")
        values = []
        value = search_result(); value["results"] = []; values.append(value)
        value = search_result(); value["results"][0]["citations"] = []; values.append(value)
        value = search_result(); value["results"][0]["snippet"] = "unrelated"; values.append(value)
        value = search_result(); value["retrieval"]["effective_mode"] = "hybrid"; values.append(value)
        value = search_result(); value["results"][0]["citations"][0]["ctx_event_id"] = ""; values.append(value)
        for value in values:
            with self.subTest(value=value), self.assertRaises(smoke.SmokeFailure):
                smoke.cited_event(value)

    def test_failed_command_retains_stage_exit_log_and_cleans_home(self):
        runner = self.harness()
        def fail():
            runner.stage = "install_and_default_setup"
            runner.run(["/bin/sh", "-c", "echo installer-rejected >&2; exit 23"])
        with patch.object(runner, "accept", side_effect=fail), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(runner.execute(), 1)
        report = json.loads(runner.result_path.read_text())
        self.assertEqual(report["failed_stage"], "install_and_default_setup")
        self.assertIn("23", report["error"])
        self.assertIn("installer-rejected", Path(report["log_path"]).read_text())
        self.assertTrue(report["cleanup"])
        self.assertFalse(runner.root.exists())

    def test_complete_acceptance_checks_and_release_owner_comparisons(self):
        runner = self.harness(CTX_INSTALL_SMOKE_EXPECTED_VERSION="1.3.1")
        install = Path(runner.env["HOME"]) / ".local"
        core, pro = install / "bin/ctx", install / "libexec/ctx-pro"
        for path in (core, pro):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("inert harness bytes " + path.name)
            path.chmod(0o700)
        marker = {"schema_version": 1, "manager": "ctx-hosted-installer", "install_path": str(core),
                  "sha256": smoke.sha256(core), "version": "1.3.1"}
        core.with_name("ctx.install.json").write_text(json.dumps(marker))
        envelope = install / "share/ctx/managed-pair-envelope.json"
        envelope.parent.mkdir(parents=True)
        envelope.write_text('{"harness_only":true}')
        # Real bash startup, including distro/user banner output, must resolve
        # the installer-owned PATH. The inert Core file is never executed here.
        profile = Path(runner.env["HOME"]) / ".bashrc"
        profile_text = 'printf "startup banner\\n"\nexport PATH="$HOME/.local/bin:$PATH"\n'
        profile.write_text(profile_text)
        commands = []
        daemon_reads = 0
        real_run = runner.run
        def command(args, timeout=30, capture=None):
            nonlocal daemon_reads
            commands.append(args)
            if args[0] == "bash":
                return real_run(args, timeout=timeout, capture=capture)
            if not capture:
                return None
            value = {"upgrade.json": {"install": {"managed": True}},
                     "daemon.json": {"daemon": {"enabled": True, "running": True, "live_pid": 123,
                                                "core_refresh_endpoint": {"available": True},
                                                "jobs": {"core_refresh": {"published_generation": "fixture-generation"}}}},
                     "daemon-after.json": {"daemon": {"running": True, "live_pid": 123}},
                     "search.json": search_result(), "citation.json": {"text": smoke.EXPECTED_TEXT}}
            if capture == "daemon.json":
                daemon_reads += 1
                if daemon_reads == 1:
                    value[capture]["daemon"]["jobs"]["core_refresh"].pop("published_generation")
            if capture == "search.json":
                self.assertGreaterEqual(daemon_reads, 2, "search must wait for initial refresh publication")
            path = runner.evidence / capture
            if capture == "version.txt":
                path.write_text("ctx 1.3.1\n")
            elif capture == "path.txt":
                path.write_text(str(core) + "\n")
            else:
                path.write_text(json.dumps(value[capture]))
            return path
        with patch.object(runner, "preflight"), patch.object(runner, "installer"), \
                patch.object(runner, "run", side_effect=command):
            runner.accept()
            self.assertTrue(all(runner.report["checks"].values()))
            self.assertEqual(commands[0], ["sh", runner.report["installer_path"]])
            self.assertFalse(any(args[0] in ("systemctl", "systemd", "dbus-daemon") for args in commands))
            self.assertFalse(any("--no-daemon" in args or "--no-setup" in args for args in commands))
            profile.write_text('printf "startup banner\\n"\n')
            with self.assertRaisesRegex(smoke.SmokeFailure, "new_shell_path"):
                runner.accept()
            profile.write_text(profile_text)
            for key in ("CORE_SHA256", "PRO_SHA256", "VERSION"):
                runner.inputs["CTX_INSTALL_SMOKE_EXPECTED_" + key] = "mismatch"
                with self.subTest(key=key), self.assertRaisesRegex(smoke.SmokeFailure, "mismatch"):
                    runner.accept()
                runner.inputs.pop("CTX_INSTALL_SMOKE_EXPECTED_" + key)
            pro.unlink()
            with self.assertRaisesRegex(smoke.SmokeFailure, "missing installed pro"):
                runner.accept()

    def test_lifecycle_cannot_overwrite_acceptance_report(self):
        report = self.root / "acceptance.json"
        report.write_text("sentinel")
        env = dict(os.environ, CTX_INSTALL_LIFECYCLE_CTX_BINARY="/bin/true", CTX_INSTALL_SMOKE_RESULT=str(report))
        run = subprocess.run(["bash", str(Path(__file__).with_name("install_live_smoke.sh"))],
                             env=env, capture_output=True, timeout=5)
        self.assertNotEqual(run.returncode, 0)
        self.assertIn(b"cannot produce", run.stderr)
        self.assertEqual(report.read_text(), "sentinel")

    @unittest.skipUnless(sys.platform == "linux", "Linux process ownership")
    def test_deadline_kills_detached_grandchild_and_retains_failure(self):
        # Subreaper must be the standalone harness, not this unittest process.
        code = r'''
import ctypes, importlib.util, os, pathlib, sys
spec = importlib.util.spec_from_file_location("smoke", sys.argv[1])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
r = m.Acceptance({"CTX_INSTALL_SMOKE_RESULT": sys.argv[2]})
ctypes.CDLL(None).prctl(36, 1, 0, 0, 0)
def fail():
    r.stage = "deadline-test"
    r.run([sys.executable, "-c", "import os,signal,time; pid=os.fork(); os.setsid() if pid == 0 else None; signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(60)"], timeout=0.2)
r.accept = fail
sys.exit(r.execute())
'''
        result = self.root / "deadline.json"
        run = subprocess.run([sys.executable, "-B", "-c", code, str(Path(smoke.__file__)), str(result)],
                             capture_output=True, timeout=12)
        self.assertEqual(run.returncode, 1, run.stderr.decode())
        report = json.loads(result.read_text())
        self.assertEqual(report["failed_stage"], "deadline-test")
        self.assertTrue(report["cleanup"])
        self.assertLess(report["elapsed_seconds"], 10)
        self.assertIn("deadline", report["error"])


if __name__ == "__main__":
    unittest.main()
