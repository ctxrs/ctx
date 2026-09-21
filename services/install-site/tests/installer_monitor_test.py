#!/usr/bin/env python3
"""Hermetic monitor contracts; never runs ctx, installer, network or Buildkite."""
from copy import deepcopy
import base64
from datetime import datetime, timezone
import hashlib
import io
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import tarfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[3]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / "services/install-site/tests" / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


runner = load("installer-monitor")
scheduler = load("installer-monitor-schedule")
COMMIT = "a" * 40
NOW = datetime(2026, 9, 7, 12, 20, tzinfo=timezone.utc)


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.result = self.root / "first-result.json"
        evidence = self.root / "first-result-evidence"
        evidence.mkdir()
        script = evidence / "install.sh"
        script.write_text("#!/bin/sh\necho fixture\n")
        log = evidence / "smoke.log"
        log.write_text("original failure evidence\n")
        self.value = {"schema_version": 1, "mode": "live", "status": "passed", "version": "1.3.1",
                      "elapsed_seconds": 60, "cleanup": True,
                      "installer_sha256": hashlib.sha256(script.read_bytes()).hexdigest(),
                      "core_sha256": "c" * 64, "pro_sha256": "d" * 64,
                      "installer_path": str(script), "log_path": str(log),
                      "checks": {k: True for k in ("core_installed", "pro_installed", "managed", "setup", "daemon", "path", "search")}}
        self.write()

    def write(self):
        self.result.write_text(json.dumps(self.value))

    def test_complete_live_report(self):
        self.assertEqual(runner.verify_result(self.result, 0)["version"], "1.3.1")
        runner.retain_smoke_evidence(self.result, self.root)
        self.assertEqual((self.root / "smoke.log").read_text(), "original failure evidence\n")
        self.assertEqual(hashlib.sha256((self.root / "served-installer.sh").read_bytes()).hexdigest(), self.value["installer_sha256"])

    def test_every_required_check_is_strict(self):
        for key in self.value["checks"]:
            for invalid in (False, 1, "true", None):
                with self.subTest(key=key, invalid=invalid):
                    value = deepcopy(self.value)
                    value["checks"][key] = invalid
                    self.result.write_text(json.dumps(value))
                    with self.assertRaises(ValueError): runner.verify_result(self.result, 0)

    def test_skipped_candidate_failed_or_missing_never_green(self):
        for key, value in (("mode", "candidate"), ("status", "skipped"), ("status", "failed"),
                           ("schema_version", 2), ("schema_version", True), ("cleanup", False),
                           ("elapsed_seconds", float("nan")), ("elapsed_seconds", 301),
                           ("checks", {}), ("version", ""), ("core_sha256", "bad")):
            result = dict(self.value, **{key: value})
            self.result.write_text(json.dumps(result))
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                runner.verify_result(self.result, 0)
        self.result.unlink()
        with self.assertRaises(ValueError): runner.verify_result(self.result, 0)

    def test_exit_code_cannot_be_laundered_by_passing_report(self):
        with self.assertRaises(ValueError): runner.verify_result(self.result, 1)

    def test_retained_installer_must_match_hash(self):
        Path(self.value["installer_path"]).write_text("changed")
        with self.assertRaises(ValueError): runner.retain_smoke_evidence(self.result, self.root)
        self.assertEqual((self.root / "smoke.log").read_text(), "original failure evidence\n")

    def test_partial_download_retains_curl_log_and_original_failure(self):
        self.value.update(status="failed", installer_sha256=None, failed_stage="installer_download",
                          error="command exited 28 at installer_download; see smoke.log")
        Path(self.value["installer_path"]).write_text("#!/bin/sh\npartial download")
        diagnostic = "curl: (28) Operation timed out after 30000 milliseconds\n"
        Path(self.value["log_path"]).write_text(diagnostic)
        self.write()
        original_report = self.result.read_bytes()
        runner.retain_smoke_evidence(self.result, self.root)
        self.assertEqual((self.root / "smoke.log").read_text(), diagnostic)
        self.assertEqual(self.result.read_bytes(), original_report)
        self.assertFalse((self.root / "served-installer.sh").exists())
        with self.assertRaisesRegex(ValueError, "live installer smoke failed \\(exit 28\\)"):
            runner.verify_result(self.result, 28)

    def test_retained_paths_cannot_escape_or_leak_home(self):
        for path in ("/etc/passwd", str(self.root / "home/private.json")):
            self.value["log_path"] = path
            self.write()
            with self.subTest(path=path), self.assertRaises(ValueError):
                runner.retain_smoke_evidence(self.result, self.root)

    def test_child_environment_drops_credentials_and_behavior_overrides(self):
        with patch.dict(os.environ, {"BUILDKITE_API_ACCESS_TOKEN": "private", "CTX_INSTALL_SMOKE_SCRIPT": "/fake",
                                     "CTX_RELEASE_METADATA_URL": "https://fake", "CTX_INSTALL_NO_SETUP": "1"}):
            env = runner.clean_environment(self.root, self.result, "nobody")
        for name in ("BUILDKITE_API_ACCESS_TOKEN", "CTX_INSTALL_SMOKE_SCRIPT", "CTX_RELEASE_METADATA_URL", "CTX_INSTALL_NO_SETUP"):
            self.assertNotIn(name, env)
        self.assertEqual(env["CTX_INSTALL_NO_PRO_TRIAL"], "1")
        self.assertEqual(env["CTX_ANALYTICS_ENABLED"], "false")

    def test_current_14_pair_and_15_single_binary_reports(self):
        self.value["version"] = "1.4.12"
        self.write()
        self.assertEqual(runner.verify_result(self.result, 0)["version"], "1.4.12")
        self.value["version"] = "1.5.0"
        self.value["pro_sha256"] = None
        del self.value["checks"]["pro_installed"]
        self.value["checks"]["single_binary"] = True
        self.write()
        self.assertEqual(runner.verify_result(self.result, 0)["version"], "1.5.0")
        for key, value in (("single_binary", False), ("single_binary", 1), ("pro_installed", True)):
            invalid = deepcopy(self.value)
            invalid["checks"][key] = value
            self.result.write_text(json.dumps(invalid))
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                runner.verify_result(self.result, 0)
        self.value["pro_sha256"] = "d" * 64
        self.write()
        with self.assertRaises(ValueError): runner.verify_result(self.result, 0)

    def test_fixture_download_uses_exact_public_pipeline_commit(self):
        with patch.dict(os.environ, {"BUILDKITE_COMMIT": COMMIT}), patch.object(runner.subprocess, "run") as run:
            runner.acquire_fixture(self.root, {})
        self.assertIn(f"https://raw.githubusercontent.com/ctxrs/ctx/{COMMIT}/", run.call_args.args[0][-1])
        for commit in ("", "main", "a" * 39):
            with patch.dict(os.environ, {"BUILDKITE_COMMIT": commit}), patch.object(runner.subprocess, "run") as run:
                with self.assertRaises(ValueError): runner.acquire_fixture(self.root, {})
                run.assert_not_called()

    def test_timeout_retains_first_log(self):
        log = self.root / "first-attempt.log"
        code = runner.run_smoke([sys.executable, "-c", "import time; print('first failure', flush=True); time.sleep(10)"],
                                {}, log, 0.1)
        self.assertEqual(code, 124)
        self.assertIn("first failure", log.read_text())
        with self.assertRaises(FileExistsError): runner.run_smoke(["unused"], {}, log, 1)

    def test_exit_status_is_preserved(self):
        code = runner.run_smoke([sys.executable, "-c", "raise SystemExit(23)"], {}, self.root / "exit.log", 1)
        self.assertEqual(code, 23)

    def test_nonroot_runner_does_not_attempt_privileged_group_reset(self):
        identity = SimpleNamespace(pw_uid=1000, pw_gid=1000)
        with patch.object(runner.os, "geteuid", return_value=1000), patch.object(runner.subprocess, "Popen") as spawn:
            spawn.return_value.wait.return_value = 0
            runner.run_smoke(["bash", "smoke.sh"], {}, self.root / "ordinary-user.log", 1, identity)
        for key in ("user", "group", "extra_groups"):
            self.assertNotIn(key, spawn.call_args.kwargs)

    def test_root_runner_drops_privileges_and_supplementary_groups(self):
        identity = SimpleNamespace(pw_uid=65534, pw_gid=65534)
        with patch.object(runner.os, "geteuid", return_value=0), patch.object(runner.subprocess, "Popen") as spawn:
            spawn.return_value.wait.return_value = 0
            runner.run_smoke(["bash", "smoke.sh"], {}, self.root / "root-runner.log", 1, identity)
        self.assertEqual(spawn.call_args.kwargs["user"], 65534)
        self.assertEqual(spawn.call_args.kwargs["group"], 65534)
        self.assertEqual(spawn.call_args.kwargs["extra_groups"], [])


class FakeAPI:
    def __init__(self):
        self.pipeline = scheduler.pipeline_body()
        self.pipeline["provider"] = {"settings": self.pipeline.pop("provider_settings")}
        self.schedule = dict(scheduler.schedule_body(COMMIT, True), id="schedule-id", next_build_at="2026-09-07T18:17:00Z")
        self.queue = {"key": "default", "dispatch_paused": False,
                      "hosted_agents": {"instance_shape": {"name": "LINUX_AMD64_2X4", "cpu": 2, "memory": 4}}}
        self.build = {"state": "passed", "source": "schedule", "branch": "main", "commit": COMMIT,
                      "created_at": "2026-09-07T12:17:00Z", "finished_at": "2026-09-07T12:18:30Z",
                      "jobs": [{"state": "passed", "step_key": "installer-monitor-linux", "exit_status": 0}]}
        self.schedules = [self.schedule]

    def get(self, path):
        if "/queues/" in path: return self.queue
        if path == scheduler.PIPELINE: return self.pipeline
        if "/schedules?" in path: return self.schedules
        if "/builds?" in path: return [self.build] if self.build else []
        raise AssertionError("unexpected read: " + path)


class ScheduleTests(unittest.TestCase):
    def setUp(self): self.api = FakeAPI()

    def inspect(self, readback=False):
        return scheduler.inspect(self.api, COMMIT, True, readback, NOW)

    def test_plan_is_idempotent(self): self.assertEqual(self.inspect()["requests"], [])

    def test_missing_pipeline_plans_only_dedicated_pipeline(self):
        self.api.pipeline = None
        plan = self.inspect()
        self.assertEqual([r["method"] for r in plan["requests"]], ["POST", "POST"])
        self.assertEqual(plan["requests"][0]["body"]["slug"], "ctx-installer-monitor")
        self.assertEqual(plan["requests"][1]["body"]["cronline"], "17 */6 * * * UTC")
        self.assertNotIn("pipeline upload", plan["requests"][0]["body"]["configuration"])

    def test_schedule_update_not_duplicate_create(self):
        self.api.schedule["enabled"] = False
        self.assertEqual(self.inspect()["requests"][0]["method"], "PUT")

    def test_duplicate_and_unknown_schedules_rejected(self):
        self.api.schedules *= 2
        with self.assertRaises(ValueError): self.inspect()
        self.api.schedules = [{"label": "release"}]
        with self.assertRaises(ValueError): self.inspect()

    def test_expensive_or_paused_queue_rejected(self):
        self.api.queue["hosted_agents"]["instance_shape"]["name"] = "LINUX_AMD64_4X16"
        with self.assertRaises(ValueError): self.inspect()
        self.api = FakeAPI()
        self.api.queue["dispatch_paused"] = True
        with self.assertRaises(ValueError): self.inspect()

    def test_release_pipeline_and_webhook_drift_rejected(self):
        self.api.pipeline["configuration"] = "steps: [{command: scripts/buildkite-upload-pipeline.sh}]"
        with self.assertRaises(ValueError): self.inspect()
        self.api = FakeAPI()
        self.api.pipeline["provider"]["settings"]["build_issue_comment_created"] = True
        with self.assertRaises(ValueError): self.inspect()

    def test_readback_requires_fresh_complete_run(self):
        self.assertTrue(self.inspect(True)["healthy"])
        for key, value in (("state", "skipped"), ("jobs", []), ("source", "api"), ("commit", "b" * 40),
                           ("created_at", "2026-09-06T12:17:00Z"), ("finished_at", "2026-09-07T12:23:00Z")):
            self.api = FakeAPI()
            self.api.build[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError): self.inspect(True)

    def test_disabled_failed_missing_schedule_or_missing_run_is_not_healthy(self):
        for change in ("missing", "failed", "disabled", "no-next", "no-run"):
            self.api = FakeAPI()
            if change == "missing": self.api.schedules = []
            if change == "failed": self.api.schedule["failed_at"] = "2026-09-07"
            if change == "disabled": self.api.schedule["enabled"] = False
            if change == "no-next": del self.api.schedule["next_build_at"]
            if change == "no-run": self.api.build = None
            with self.subTest(change=change), self.assertRaises(ValueError): self.inspect(True)

    def test_retry_success_not_healthy(self):
        self.api.build["jobs"][0]["retries_count"] = 1
        with self.assertRaises(ValueError): self.inspect(True)

    def test_null_retry_count_is_a_first_attempt(self):
        self.api.build["jobs"][0]["retries_count"] = None
        self.assertTrue(self.inspect(True)["healthy"])

    def test_original_failed_job_cannot_be_hidden_by_retry(self):
        self.api.build["jobs"].append({"state": "failed", "step_key": "installer-monitor-linux", "exit_status": 1})
        with self.assertRaises(ValueError): self.inspect(True)

    def test_probe_payload_is_private_bounded_and_contains_only_reviewed_inputs(self):
        with patch.object(scheduler, "assert_public_source") as source:
            plan = scheduler.probe_plan(COMMIT)
        self.assertEqual(source.call_count, 2)
        body = plan["create_pipeline"]["body"]
        self.assertEqual(body["visibility"], "private")
        self.assertEqual(body["slug"], "ctx-installer-monitor")
        steps = json.loads(body["configuration"])["steps"]
        self.assertEqual(len(steps), 1)
        self.assertEqual(steps[0]["agents"], {"queue": "default"})
        self.assertEqual(steps[0]["timeout_in_minutes"], 5)
        self.assertTrue(steps[0]["checkout"]["skip"])
        encoded = steps[0]["command"].split("printf '%s' '")[1].split("'", 1)[0]
        payload = base64.b64decode(encoded)
        self.assertEqual(hashlib.sha256(payload).hexdigest(), plan["payload_sha256"])
        with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as archive:
            self.assertEqual(set(archive.getnames()), set(plan["source_file_sha256"]))
            for entry in archive:
                self.assertTrue(entry.isfile())
                self.assertEqual(hashlib.sha256(archive.extractfile(entry).read()).hexdigest(), plan["source_file_sha256"][entry.name])
                self.assertTrue(entry.name in (
                    "services/install-site/tests/installer-monitor.py",
                    "services/install-site/tests/install_live_smoke.sh", "services/install-site/tests/install_linux_smoke.py"))
        self.assertNotIn("schedule", plan)

    def test_readback_of_disabled_schedule_says_not_healthy(self):
        self.api.schedule["enabled"] = False
        self.assertFalse(scheduler.inspect(self.api, COMMIT, False, True, NOW)["healthy"])

    def test_public_checkout_binding_rejects_wrong_commit_or_changed_inputs(self):
        for output in (["b" * 40], [COMMIT, " M installer-monitor.py"]):
            with patch.object(scheduler.subprocess, "check_output", side_effect=output):
                with self.assertRaises(ValueError): scheduler.assert_public_source(COMMIT, ["input"])
        with patch.object(scheduler.subprocess, "check_output", side_effect=[COMMIT, ""]) as git:
            scheduler.assert_public_source(COMMIT, ["input"])
        self.assertEqual(git.call_args.args[0][-2:], ["--", "input"])
        self.assertEqual(scheduler.pipeline_body()["repository"], "git@github.com:ctxrs/ctx.git")

    def test_source_commit_must_be_exact(self):
        with self.assertRaises(ValueError): scheduler.schedule_body("HEAD", True)


if __name__ == "__main__": unittest.main()
