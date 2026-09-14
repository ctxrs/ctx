"""Bounded regressions for startup-owned cold performance measurements."""

from contextlib import ExitStack
import copy
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

import performance_sanity_support as support


def cold_job():
    return {
        "request_id": "startup-P", "owner": "daemon", "trigger": "periodic",
        "trigger_provenance": "daemon_scheduler", "previous_generation": None,
        "status": "completed", "request_state": "published",
        "published_generation": "G", "generation_changed": True,
        "timings_us": {"scan_stage": 10},
        "receipt": {
            "outcome": "completed", "generation_changed": True,
            "published_generation": "G",
            "current": {"current_indexed_documents": 12},
        },
    }


def cold_status():
    return {
        "daemon": {"mode": "source-refresh-only", "jobs": {"core_refresh": cold_job()}},
        "lexical": {"generation_id": "G", "indexed_documents": 12},
    }


class ColdStartupOwnershipTest(unittest.TestCase):
    def test_first_cold_identity_cannot_be_replaced_by_later_changed_job(self):
        job = cold_job()
        job.update(status="running", receipt=None)
        self.assertFalse(support.cold_job_completed(job, "startup-P"))
        self.assertTrue(support.cold_job_completed(cold_job(), "startup-P"))
        with self.assertRaisesRegex(RuntimeError, "identity"):
            support.cold_job_completed(cold_job(), "other-request")

    def test_cold_receipt_rejects_warm_noop_failed_and_missing_facts(self):
        changes = (
            {"request_id": None}, {"owner": "client"}, {"trigger": "search"},
            {"previous_generation": "older"}, {"generation_changed": False},
            {"status": "failed"}, {"status": "retry_backoff"},
            {"request_state": "running"}, {"receipt": None},
            {"published_generation": "other"}, {"timings_us": {}},
        )
        for change in changes:
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                support.cold_job_completed({**cold_job(), **change}, "startup-P")
        for change in (
            {"outcome": "failed"}, {"previous_generation": "old"},
            {"generation_changed": False}, {"published_generation": "other"},
            {"current": None},
        ):
            job = cold_job()
            job["receipt"].update(change)
            with self.subTest(receipt=change), self.assertRaises(RuntimeError):
                support.cold_job_completed(job, "startup-P")

    def test_search_noop_keeps_cold_receipt_and_checks_live_generation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lexical = root / "search/lexical"
            meta = lexical / "index-generations/G/meta.json"
            meta.parent.mkdir(parents=True)
            meta.write_text(json.dumps({"opstamp": 1, "segments": [{"segment_id": "one"}]}))
            manifest = lexical / "ctx-generations/G.json"
            manifest.parent.mkdir()
            manifest.write_text("{}")
            pointer = lexical / "active-generation.json"
            pointer.write_text(json.dumps({"active": {"generation_id": "G", "directory": "G"}}))
            retained = cold_status()
            live = copy.deepcopy(retained)
            live["daemon"]["jobs"]["core_refresh"].update(
                request_id="search-S", trigger="search", previous_generation="G",
                generation_changed=False,
            )
            search = {"retrieval": {"generation_id": "G", "indexed_documents": 12}}
            with patch.object(support, "run_json", return_value=live):
                snapshot = support.refresh_snapshot(
                    search, root, {"CTX_DATA_ROOT": str(root)}, cold_status=retained,
                )
                self.assertEqual(snapshot.request_id, "startup-P")
                self.assertTrue(snapshot.generation_changed)
                self.assertIsNone(snapshot.previous_generation)
                self.assertEqual(snapshot.manifest.body, b"{}")
                self.assertEqual(snapshot.segments, ("one",))
                self.assertEqual(live["daemon"]["jobs"]["core_refresh"]["request_id"], "search-S")
                live["lexical"]["generation_id"] = "other"
                with self.assertRaisesRegex(RuntimeError, "disagree"):
                    support.refresh_snapshot(search, root, {"CTX_DATA_ROOT": str(root)}, cold_status=retained)
                live["lexical"]["generation_id"] = "G"
                retained["daemon"]["jobs"]["core_refresh"]["receipt"]["current"]["current_indexed_documents"] = 11
                with self.assertRaisesRegex(RuntimeError, "disagree"):
                    support.refresh_snapshot(search, root, {"CTX_DATA_ROOT": str(root)}, cold_status=retained)
                retained["daemon"]["jobs"]["core_refresh"]["receipt"]["current"]["current_indexed_documents"] = 12
                pointer.write_text(json.dumps({"active": {"generation_id": "other", "directory": "G"}}))
                with self.assertRaisesRegex(RuntimeError, "pointer disagrees"):
                    support.refresh_snapshot(search, root, {"CTX_DATA_ROOT": str(root)}, cold_status=retained)

    def test_completion_before_readiness_includes_launch_and_lifetime_cpu(self):
        with tempfile.TemporaryDirectory() as temporary, ExitStack() as stack:
            root = Path(temporary)
            env = {"CTX_DATA_ROOT": str(root / "data")}
            clock = [0.0]
            daemon = Mock(pid=123)
            daemon.poll.return_value = None
            outputs = (io.BytesIO(), io.BytesIO())

            def launch(*args):
                self.assertEqual(clock[0], 0.0)
                journal = root / "data/daemon/jobs/core-refresh.json"
                journal.parent.mkdir(parents=True)
                journal.write_text(json.dumps(cold_job()))
                clock[0] = 2.0  # Cold work can complete before Popen returns.
                return daemon, *outputs

            def ready(*args):
                self.assertEqual(args[-1], support.COMMAND_TIMEOUT_SECONDS)
                clock[0] = 9.0  # Readiness must not extend the cold sample.

            stack.enter_context(patch.object(support.time, "monotonic", side_effect=lambda: clock[0]))
            stack.enter_context(patch.object(support, "launch_daemon", side_effect=launch))
            stack.enter_context(patch.object(support, "wait_daemon_ready", side_effect=ready))
            stack.enter_context(patch.object(support, "run_json", return_value=cold_status()))
            stack.enter_context(patch.object(support, "linux_process_cpu_ticks", return_value=300))
            stack.enter_context(patch.object(support.os, "sysconf", return_value=100))
            stack.enter_context(patch.object(support, "linux_source_worker_cpu_ticks", return_value={(1, "ctx-src-scan00"): 40}))
            stack.enter_context(patch.object(support, "linux_open_fd_count", return_value=4))
            stack.enter_context(patch.object(support, "linux_open_fd_summary", return_value=()))
            stack.enter_context(patch.object(support, "linux_peak_rss_bytes", return_value=100))
            result = support.start_cold_daemon(root, env)
            sample = result[3]
            self.assertEqual(sample.elapsed_seconds, 2.0)
            self.assertEqual(sample.cpu_seconds, 3.0)
            self.assertEqual(sample.source_workers[0].cpu_ticks, 40)
            self.assertEqual(sample.packet["request_id"], "startup-P")
            self.assertEqual(result[4], cold_status())
            for output in outputs:
                output.close()

    def test_command_sampler_keeps_existing_delta_baselines(self):
        with patch.object(support, "linux_process_cpu_ticks", side_effect=[100, 300]), patch.object(
            support, "linux_source_worker_cpu_ticks",
            side_effect=[{(1, "ctx-src-scan00"): 20}, {(1, "ctx-src-scan00"): 50}],
        ), patch.object(support, "linux_open_fd_count", return_value=4), patch.object(
            support, "linux_open_fd_summary", return_value=(),
        ), patch.object(support, "linux_peak_rss_bytes", return_value=100), patch.object(
            support.time, "monotonic", return_value=12.0,
        ), patch.object(support.os, "sysconf", return_value=100):
            sampler = support.DaemonSampler(123, 10.0)
            sampler.sample()
            result = sampler.finish({"command": "unchanged"})
            self.assertEqual(result.elapsed_seconds, 2.0)
            self.assertEqual(result.cpu_seconds, 2.0)
            self.assertEqual(result.source_workers[0].cpu_ticks, 30)

    def test_readiness_status_uses_only_remaining_launch_deadline(self):
        clock = [28.0]
        process = Mock()
        process.poll.return_value = None

        def status(*args, **kwargs):
            self.assertEqual(kwargs["timeout_seconds"], 2.0)
            clock[0] = 31.0
            return {"daemon": {"running": True, "core_refresh_endpoint": {"available": True}}}

        with patch.object(support.time, "monotonic", side_effect=lambda: clock[0]), patch.object(
            support, "run_json", side_effect=status,
        ), patch.object(support.time, "sleep"), self.assertRaises(TimeoutError):
            support.wait_daemon_ready(
                process, io.BytesIO(), io.BytesIO(), Path("."), {}, 30.0,
            )

    def test_existing_generation_or_journal_cannot_launch_a_cold_run(self):
        for relative in ("search/lexical/active-generation.json", "daemon/jobs/core-refresh.json"):
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                existing = root / relative
                existing.parent.mkdir(parents=True)
                existing.write_text("{}")
                with patch.object(support, "launch_daemon") as launch, self.assertRaises(RuntimeError):
                    support.start_cold_daemon(root, {"CTX_DATA_ROOT": str(root)})
                launch.assert_not_called()

    def test_failed_readiness_observation_and_timeout_close_owned_daemon(self):
        for phase in ("readiness", "missing", "failed", "exited"):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                process = Mock(pid=123)
                process.poll.return_value = 1 if phase == "exited" else None
                outputs = (io.BytesIO(), io.BytesIO())
                env = {"CTX_DATA_ROOT": str(root / "data")}

                def launch(*args):
                    if phase == "failed":
                        journal = root / "data/daemon/jobs/core-refresh.json"
                        journal.parent.mkdir(parents=True)
                        journal.write_text(json.dumps({**cold_job(), "status": "failed"}))
                    return process, *outputs

                with patch.object(support, "launch_daemon", side_effect=launch), patch.object(
                    support, "terminate_daemon_process"
                ) as stop, patch.object(support, "DaemonSampler"), patch.object(
                    support, "wait_daemon_ready", side_effect=TimeoutError("not ready")
                ):
                    with self.assertRaises((RuntimeError, TimeoutError)):
                        if phase == "readiness":
                            support.start_daemon(root, env)
                        else:
                            support.start_cold_daemon(root, env, timeout_seconds=0.01)
                    stop.assert_called_once_with(process)
                    self.assertTrue(all(output.closed for output in outputs))

    @unittest.skipUnless(sys.platform == "linux", "Linux caller-thread affinity")
    def test_affinity_is_inherited_before_exec_and_parent_restored(self):
        original = os.sched_getaffinity(0)
        selected = {min(original)}
        popen = subprocess.Popen
        with tempfile.TemporaryDirectory() as temporary:
            def child(argv, **kwargs):
                self.assertEqual(os.sched_getaffinity(0), selected)
                return popen([sys.executable, "-c", "import os; print(sorted(os.sched_getaffinity(0)))"], **kwargs)

            with patch.object(support.subprocess, "Popen", side_effect=child):
                process, stdout, stderr = support.launch_daemon(
                    Path(temporary), {support.TASK_BINARY_ENV: sys.executable}, selected,
                )
            try:
                self.assertEqual(os.sched_getaffinity(0), original)
                self.assertEqual(process.wait(timeout=5), 0)
                stdout.seek(0)
                self.assertEqual(json.loads(stdout.read()), sorted(selected))
            finally:
                support.close_daemon(process, stdout, stderr)

    @unittest.skipUnless(sys.platform == "linux", "Linux caller-thread affinity")
    def test_failed_launch_restores_affinity_and_closes_captures(self):
        original = os.sched_getaffinity(0)
        captures = []

        def fail(argv, **kwargs):
            captures.extend((kwargs["stdout"], kwargs["stderr"]))
            self.assertEqual(os.sched_getaffinity(0), {min(original)})
            raise OSError("inert launch failure")

        with tempfile.TemporaryDirectory() as temporary, patch.object(
            support.subprocess, "Popen", side_effect=fail,
        ), self.assertRaises(OSError):
            support.launch_daemon(
                Path(temporary), {support.TASK_BINARY_ENV: sys.executable}, {min(original)},
            )
        self.assertEqual(os.sched_getaffinity(0), original)
        self.assertTrue(all(output.closed for output in captures))
