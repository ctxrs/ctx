#!/usr/bin/env python3
"""Representative source-family cold-refresh performance gate."""

from __future__ import annotations

import os
from pathlib import Path
import sys
import tempfile
import unittest

from performance_family_fixtures import (
    FAMILY_CORPUS_WRITERS,
    FAMILY_MAX_FIXTURE_BYTES,
    FAMILY_MIN_FIXTURE_BYTES,
    FamilyCorpus,
)
from performance_sanity_support import (
    COMMAND_TIMEOUT_SECONDS,
    FORCE_SINGLE_CPU_ENV,
    MAX_COMMAND_SECONDS,
    MAX_PEAK_RSS_BYTES,
    RefreshSnapshot,
    isolated_env,
    require_parallel_source_workers,
    refresh_snapshot,
    run_refresh_measured,
    run_checked,
    run_json,
    run_json_timed,
    start_daemon,
    stop_daemon,
)


@unittest.skipUnless(
    sys.platform.startswith("linux")
    and hasattr(os, "sched_getaffinity")
    and Path("/proc/self/stat").is_file()
    and Path("/proc/self/fd").is_dir(),
    "source-family overlap and FD evidence requires Linux /proc and affinity",
)
class SourceFamilyColdRefreshPerformanceTest(unittest.TestCase):
    MIN_AVAILABLE_CPUS = 12
    MIN_CPU_PER_WALL = 1.10
    MAX_OPEN_FD_DELTA = 96

    @staticmethod
    def refresh_args(query: str) -> list[str]:
        return ["search", query, "--refresh", "wait", "--format=json", "--limit", "1"]

    def assert_family_state(
        self,
        search: dict[str, object],
        root: Path,
        env: dict[str, str],
        corpus: FamilyCorpus,
    ) -> RefreshSnapshot:
        self.assertEqual(
            search["freshness"],
            {"mode": "wait", "source_count": 1, "status": "completed"},
        )
        snapshot = refresh_snapshot(search, root, env)
        status = run_json(["status", "--format=json"], env, root)
        job = status["daemon"]["jobs"]["source_backed_refresh"]
        self.assertEqual(job["status"], "completed")
        self.assertEqual(job["request_state"], "published")
        self.assertEqual(job["source_count"], 1)
        self.assertEqual(job["scanned_routes"], 1)
        self.assertEqual(job["unsupported_routes"], 0)
        self.assertEqual(
            job["progress"],
            {
                "completed_sources": 1,
                "phase": "published",
                "total_sources": 1,
            },
        )
        self.assertEqual(job["certified_source_count"], corpus.source_count)
        self.assertEqual(
            job["certified_source_bytes"], corpus.certified_source_bytes
        )
        expected_current = {
            "current_certified_source_bytes": corpus.certified_source_bytes,
            "current_complete_records": corpus.complete_records,
            "current_ignored_records": corpus.ignored_records,
            "current_indexed_documents": corpus.indexed_documents,
            "current_rejected_records": corpus.rejected_records,
            "current_retained_records": corpus.retained_records,
            "current_source_count": corpus.source_count,
            "current_sources_with_rejections": 0,
            "removed_source_count": 0,
        }
        self.assertEqual(snapshot.current, expected_current)
        self.assertEqual(snapshot.indexed_documents, corpus.indexed_documents)
        self.assertEqual(status["indexed_events"], corpus.indexed_documents)
        self.assertEqual(status["indexed_items"], corpus.indexed_documents)
        self.assertEqual(status["indexed_sources"], corpus.source_count)
        self.assertEqual(
            status["lexical"]["indexed_documents"], corpus.indexed_documents
        )
        self.assertEqual(
            status["lexical"]["certified_sources"], corpus.source_count
        )
        self.assertEqual(
            status["lexical"]["certified_source_bytes"],
            corpus.certified_source_bytes,
        )
        self.assertGreater(job["timings_us"]["scan_stage"], 0)
        self.assertTrue(snapshot.segments)
        return snapshot

    def assert_complete_hydration(
        self,
        root: Path,
        env: dict[str, str],
        corpus: FamilyCorpus,
        query: str,
        expected_body: str,
    ) -> str:
        search = run_json(
            [
                "search",
                query,
                "--provider",
                corpus.provider,
                "--refresh",
                "off",
                "--format=json",
                "--limit",
                "1",
            ],
            env,
            root,
        )
        results = search.get("results")
        self.assertIsInstance(results, list)
        self.assertEqual(len(results), 1)
        result = results[0]
        self.assertEqual(result["provider"], corpus.provider)
        self.assertEqual(result["source_format"], corpus.source_format)
        self.assertTrue(
            str(result["source_path"]).endswith(corpus.source_path_suffix)
        )
        show = run_json(
            [
                "show",
                "event",
                result["ctx_event_id"],
                "--content",
                "complete",
                "--format=json",
            ],
            env,
            root,
        )
        self.assertEqual(show["payload_type"], "event_window")
        self.assertEqual(show["content_policy"], "complete")
        event = show["event"]
        self.assertEqual(event["provider"], corpus.provider)
        self.assertEqual(event["ctx_event_id"], result["ctx_event_id"])
        self.assertEqual(event["text"], expected_body)
        self.assertEqual(
            event["content"],
            {
                "complete": True,
                "complete_content_available": True,
                "origin": "provider_source",
                "requested": "complete",
                "source_verified": True,
                "stored_truncated": False,
            },
        )
        return result["ctx_event_id"]

    def test_representative_source_families_overlap_and_replace(self) -> None:
        available_cpus = set(os.sched_getaffinity(0))
        self.assertGreaterEqual(
            len(available_cpus),
            self.MIN_AVAILABLE_CPUS,
            "nightly family gate requires >=12 available CPUs: 8 Tantivy "
            "indexers + 2 runtime threads + 2 source scanners",
        )
        forced_single_cpu = os.environ.get(FORCE_SINGLE_CPU_ENV) == "1"
        daemon_affinity = {min(available_cpus)} if forced_single_cpu else None

        for writer in FAMILY_CORPUS_WRITERS:
            with self.subTest(writer=writer.__name__), tempfile.TemporaryDirectory(
                prefix="ctx-source-family-performance-"
            ) as temporary:
                root = Path(temporary)
                home = root / "home"
                home.mkdir()
                corpus = writer(home)
                try:
                    self.assertTrue(corpus.independent_leaves)
                    self.assertGreater(corpus.source_count, 16)
                    self.assertGreaterEqual(
                        corpus.fixture_bytes, FAMILY_MIN_FIXTURE_BYTES
                    )
                    self.assertLessEqual(
                        corpus.fixture_bytes, FAMILY_MAX_FIXTURE_BYTES
                    )
                    env = isolated_env(root, home)
                    run_checked(
                        [
                            "setup",
                            "--catalog-only",
                            "--no-daemon",
                            "--progress",
                            "none",
                        ],
                        env,
                        root,
                    )
                    daemon, daemon_stdout, daemon_stderr = start_daemon(
                        root, env, daemon_affinity
                    )
                    try:
                        cold = run_refresh_measured(
                            self.refresh_args(corpus.cold_query),
                            env,
                            root,
                            daemon.pid,
                            COMMAND_TIMEOUT_SECONDS,
                        )
                        cold_snapshot = self.assert_family_state(
                            cold.packet, root, env, corpus
                        )
                        cold_event_id = self.assert_complete_hydration(
                            root, env, corpus, corpus.cold_query, corpus.cold_body
                        )

                        noop_search, noop_seconds = run_json_timed(
                            self.refresh_args(corpus.cold_query),
                            env,
                            root,
                        )
                        noop = self.assert_family_state(
                            noop_search, root, env, corpus
                        )
                        self.assertFalse(noop.generation_changed)
                        self.assertEqual(
                            noop.previous_generation, cold_snapshot.generation_id
                        )
                        self.assertEqual(
                            noop.generation_id, cold_snapshot.generation_id
                        )
                        self.assertEqual(noop.current, cold_snapshot.current)
                        self.assertEqual(noop.opstamp, cold_snapshot.opstamp)
                        self.assertEqual(noop.segments, cold_snapshot.segments)
                        self.assertEqual(noop.meta, cold_snapshot.meta)
                        self.assertEqual(noop.manifest, cold_snapshot.manifest)
                        self.assertEqual(
                            noop.manifest_names, cold_snapshot.manifest_names
                        )
                        self.assertEqual(
                            noop.index_bytes, cold_snapshot.index_bytes
                        )

                        corpus.replace_leaf()
                        replacement_search, replacement_seconds = run_json_timed(
                            self.refresh_args(corpus.replacement_query),
                            env,
                            root,
                        )
                        replacement = self.assert_family_state(
                            replacement_search, root, env, corpus
                        )
                        self.assertNotEqual(
                            replacement.generation_id, noop.generation_id
                        )
                        self.assertEqual(replacement.current, noop.current)
                        self.assertGreater(replacement.opstamp, noop.opstamp)
                        self.assertEqual(
                            len(replacement.manifest_names),
                            len(noop.manifest_names) + 1,
                        )
                        replacement_event_id = self.assert_complete_hydration(
                            root,
                            env,
                            corpus,
                            corpus.replacement_query,
                            corpus.replacement_body,
                        )
                        self.assertEqual(replacement_event_id, cold_event_id)
                    finally:
                        stop_daemon(
                            daemon,
                            daemon_stdout,
                            daemon_stderr,
                            root,
                            env,
                        )

                    self.assertLessEqual(
                        cold.elapsed_seconds, MAX_COMMAND_SECONDS
                    )
                    self.assertLessEqual(noop_seconds, MAX_COMMAND_SECONDS)
                    self.assertLessEqual(
                        replacement_seconds, MAX_COMMAND_SECONDS
                    )
                    source_workers = require_parallel_source_workers(cold)
                    source_worker_ticks = ",".join(
                        f"{worker.name}:{worker.cpu_ticks}"
                        for worker in source_workers
                    )
                    self.assertGreaterEqual(
                        cold.cpu_per_wall,
                        self.MIN_CPU_PER_WALL,
                        f"{corpus.family} cold refresh did not overlap "
                        "independent source work; set "
                        f"{FORCE_SINGLE_CPU_ENV}=1 to exercise the control",
                    )
                    self.assertLessEqual(
                        cold.peak_open_fds - cold.baseline_open_fds,
                        self.MAX_OPEN_FD_DELTA,
                    )
                    self.assertLessEqual(
                        cold.peak_rss_bytes, MAX_PEAK_RSS_BYTES
                    )
                    print(
                        "source-family performance:"
                        f" family={corpus.family}"
                        f" provider={corpus.provider}"
                        f" fixture_sources={corpus.source_count}"
                        f" fixture_records={corpus.complete_records}"
                        f" fixture_bytes={corpus.fixture_bytes}"
                        f" certified_bytes={corpus.certified_source_bytes}"
                        f" cold_seconds={cold.elapsed_seconds:.3f}"
                        f" daemon_cpu_seconds={cold.cpu_seconds:.3f}"
                        f" cpu_per_wall={cold.cpu_per_wall:.3f}"
                        f" source_worker_slots="
                        f"{len({worker.name for worker in source_workers})}"
                        f" source_worker_cpu_ticks="
                        f"{source_worker_ticks}"
                        f" peak_fd_delta="
                        f"{cold.peak_open_fds - cold.baseline_open_fds}"
                        f" peak_rss_bytes={cold.peak_rss_bytes}"
                        f" noop_seconds={noop_seconds:.3f}"
                        f" replacement_seconds={replacement_seconds:.3f}"
                        f" forced_single_cpu={forced_single_cpu}"
                    )
                finally:
                    corpus.close()


if __name__ == "__main__":
    unittest.main()
