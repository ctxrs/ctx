#!/usr/bin/env python3
"""Linux qualification for watcher-driven representative-family refreshes."""

from __future__ import annotations

import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest

from performance_family_fixtures import (
    FAMILY_CORPUS_WRITERS,
    SQLITE_WAL_FAMILY,
    FamilyCorpus,
    FamilyMutation,
)
from performance_sanity_support import (
    COMMAND_TIMEOUT_SECONDS,
    MAX_PEAK_RSS_BYTES,
    RefreshSnapshot,
    immutable_tree_snapshot,
    isolated_env,
    linux_open_fd_count,
    linux_peak_rss_bytes,
    refresh_snapshot,
    run_checked,
    run_json,
    start_daemon,
    stop_daemon,
)


CONTINUOUS_MUTATIONS_ENV = "CTX_PERFORMANCE_CONTINUOUS_MUTATIONS"
DEFAULT_CONTINUOUS_MUTATIONS = 5
MAX_OPEN_FD_DELTA = 96
MAX_INCREMENTAL_SEGMENT_OVERHEAD_BYTES = 256 * 1024
MUTATION_CADENCE_SECONDS = 0.1
STARTUP_REFRESH_QUIET_SECONDS = 2.5


def continuous_mutation_count() -> int:
    raw = os.environ.get(CONTINUOUS_MUTATIONS_ENV)
    if raw is None:
        return DEFAULT_CONTINUOUS_MUTATIONS
    try:
        count = int(raw)
    except ValueError as error:
        raise AssertionError(
            f"{CONTINUOUS_MUTATIONS_ENV} must be a positive integer, got {raw!r}"
        ) from error
    if count < 1:
        raise AssertionError(
            f"{CONTINUOUS_MUTATIONS_ENV} must be a positive integer, got {count}"
        )
    return count


@unittest.skipUnless(
    sys.platform.startswith("linux")
    and Path("/proc/self/stat").is_file()
    and Path("/proc/self/fd").is_dir(),
    "continuous watcher qualification requires Linux /proc",
)
class ProviderFamilyContinuousRefreshTest(unittest.TestCase):
    def search_off(self, query: str, corpus: FamilyCorpus, env: dict[str, str], root: Path) -> dict[str, object]:
        return run_json(
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

    def assert_result(self, search: dict[str, object], corpus: FamilyCorpus, body: str, env: dict[str, str], root: Path) -> str:
        self.assertEqual(
            search["freshness"],
            {"mode": "off", "source_count": 0, "status": "existing_generation"},
        )
        results = search["results"]
        self.assertIsInstance(results, list)
        self.assertEqual(len(results), 1)
        result = results[0]
        self.assertEqual(result["provider"], corpus.provider)
        self.assertEqual(result["source_format"], corpus.source_format)
        event_id = result["ctx_event_id"]
        show = run_json(["show", "event", event_id, "--format=json"], env, root)
        self.assertEqual(show["event"]["text"], body)
        return event_id

    def wait_for_searchable(self, query: str, corpus: FamilyCorpus, env: dict[str, str], root: Path, daemon_pid: int, resources: dict[str, int]) -> dict[str, object]:
        deadline = time.monotonic() + COMMAND_TIMEOUT_SECONDS
        last_error: Exception | None = None
        while time.monotonic() < deadline:
            resources["peak_open_fds"] = max(
                resources["peak_open_fds"], linux_open_fd_count(daemon_pid)
            )
            resources["peak_rss_bytes"] = max(
                resources["peak_rss_bytes"], linux_peak_rss_bytes(daemon_pid)
            )
            try:
                search = self.search_off(query, corpus, env, root)
            except RuntimeError as error:
                last_error = error
            else:
                results = search.get("results")
                if isinstance(results, list) and len(results) == 1:
                    return search
            time.sleep(0.025)
        raise AssertionError(
            "watcher did not publish a refresh-off-searchable mutation before "
            f"the deadline for provider={corpus.provider} query={query!r}; "
            f"last_error={last_error}"
        )

    def wait_for_isolated_continuous_baseline(
        self,
        corpus: FamilyCorpus,
        env: dict[str, str],
        root: Path,
        daemon_pid: int,
        resources: dict[str, int],
    ) -> tuple[dict[str, object], RefreshSnapshot]:
        """Drain watcher work before asserting mutation causality."""
        deadline = time.monotonic() + COMMAND_TIMEOUT_SECONDS
        stable_signature: tuple[object, ...] | None = None
        stable_since: float | None = None
        last_job: object = None
        while time.monotonic() < deadline:
            resources["peak_open_fds"] = max(
                resources["peak_open_fds"], linux_open_fd_count(daemon_pid)
            )
            resources["peak_rss_bytes"] = max(
                resources["peak_rss_bytes"], linux_peak_rss_bytes(daemon_pid)
            )
            status = run_json(["status", "--format=json"], env, root)
            daemon = status["daemon"]
            job = daemon["jobs"]["core_refresh"]
            last_job = job
            if job["status"] in {"failed", "retry_backoff"}:
                raise RuntimeError(
                    f"cold-start refresh failed before quiescence: {job!r}"
                )
            wakeup = daemon.get("wakeup", {})
            signature = (
                job.get("request_id"),
                job.get("request_state"),
                job.get("published_generation"),
                wakeup.get("raw_events"),
                wakeup.get("coalesced_wakeups"),
                wakeup.get("ingress_overflows"),
                wakeup.get("ingress_disconnects"),
                wakeup.get("rescan_notifications"),
            )
            terminal = (
                job["status"] == "completed"
                and job["request_state"] == "published"
            )
            now = time.monotonic()
            if terminal and signature == stable_signature:
                if (
                    stable_since is not None
                    and now - stable_since >= STARTUP_REFRESH_QUIET_SECONDS
                ):
                    search = self.search_off(corpus.cold_query, corpus, env, root)
                    snapshot = refresh_snapshot(search, root, env)
                    if snapshot.request_id == job["request_id"]:
                        return search, snapshot
                    stable_signature = None
                    stable_since = None
                    continue
            else:
                stable_signature = signature if terminal else None
                stable_since = now if terminal else None
            time.sleep(0.025)
        raise AssertionError(
            "cold-start watcher work did not become quiescent before the "
            f"controlled continuous phase: {last_job!r}"
        )

    def assert_counts(self, snapshot: RefreshSnapshot, corpus: FamilyCorpus, mutations: list[FamilyMutation]) -> None:
        expected = {
            "current_certified_source_bytes": corpus.certified_source_bytes
            + sum(item.certified_source_bytes_delta for item in mutations),
            "current_complete_records": corpus.complete_records
            + sum(item.complete_records_delta for item in mutations),
            "current_ignored_records": corpus.ignored_records,
            "current_indexed_documents": corpus.indexed_documents
            + sum(item.indexed_documents_delta for item in mutations),
            "current_rejected_records": corpus.rejected_records,
            "current_retained_records": corpus.retained_records
            + sum(item.retained_records_delta for item in mutations),
            "current_source_count": corpus.source_count,
            "current_sources_with_rejections": 0,
            "removed_source_count": 0,
        }
        self.assertEqual(snapshot.current, expected)
        self.assertEqual(snapshot.indexed_documents, expected["current_indexed_documents"])
        self.assertEqual(snapshot.status["indexed_events"], expected["current_indexed_documents"])
        self.assertEqual(snapshot.status["indexed_items"], expected["current_indexed_documents"])
        self.assertEqual(snapshot.status["indexed_sources"], corpus.source_count)

    def assert_incremental_watcher_job(
        self,
        snapshot: RefreshSnapshot,
        previous: RefreshSnapshot,
        env: dict[str, str],
        corpus: FamilyCorpus,
    ) -> None:
        job = snapshot.job
        self.assertEqual(job["owner"], "daemon")
        self.assertEqual(job["status"], "completed")
        self.assertEqual(job["request_state"], "published")
        self.assertEqual(job["trigger"], "periodic")
        self.assertEqual(job["trigger_provenance"], "daemon_scheduler")
        durable_job = json.loads(
            (
                Path(env["CTX_DATA_ROOT"])
                / "daemon"
                / "jobs"
                / "core-refresh.json"
            ).read_text(encoding="utf-8")
        )
        self.assertEqual(durable_job["request_id"], job["request_id"])
        self.assertEqual(durable_job["refresh_scope"]["kind"], "exact")
        self.assertEqual(len(durable_job["refresh_scope"]["routes"]), 1)
        self.assertEqual(job["source_count"], 1)
        self.assertNotEqual(snapshot.request_id, previous.request_id)
        self.assertNotEqual(snapshot.generation_id, previous.generation_id)
        self.assertEqual(snapshot.previous_generation, previous.generation_id)
        self.assertGreater(snapshot.opstamp, previous.opstamp)
        receipt = job["receipt"]
        self.assertEqual(receipt["selected_route_total"], 1)
        self.assertEqual(receipt["successful_route_total"], 1)
        route_results = receipt["route_results"]
        self.assertIsInstance(route_results, dict)
        self.assertEqual(
            set(route_results), set(durable_job["refresh_scope"]["routes"])
        )

    def assert_absent(self, query: str, corpus: FamilyCorpus, env: dict[str, str], root: Path) -> None:
        search = self.search_off(query, corpus, env, root)
        self.assertEqual(search["freshness"]["mode"], "off")
        self.assertEqual(search["freshness"]["status"], "existing_generation")
        self.assertEqual(search["results"], [])

    def test_watcher_refreshes_representative_provider_families_continuously(self) -> None:
        mutation_count = continuous_mutation_count()
        for writer in FAMILY_CORPUS_WRITERS:
            with self.subTest(writer=writer.__name__), tempfile.TemporaryDirectory(
                prefix="ctx-provider-family-continuous-"
            ) as temporary:
                root = Path(temporary)
                home = root / "home"
                home.mkdir()
                corpus = writer(home)
                try:
                    env = isolated_env(root, home)
                    run_checked(
                        ["setup", "--catalog-only", "--no-daemon", "--progress", "none"],
                        env,
                        root,
                    )
                    daemon, daemon_stdout, daemon_stderr = start_daemon(root, env)
                    try:
                        resources = {
                            "baseline_open_fds": linux_open_fd_count(daemon.pid),
                            "peak_open_fds": linux_open_fd_count(daemon.pid),
                            "peak_rss_bytes": linux_peak_rss_bytes(daemon.pid),
                        }
                        cold_search = self.wait_for_searchable(
                            corpus.cold_query, corpus, env, root, daemon.pid, resources
                        )
                        cold = refresh_snapshot(cold_search, root, env)
                        if corpus.family == SQLITE_WAL_FAMILY:
                            # SQLite online backup can produce delayed native
                            # watcher observations. A valid scheduler batch for
                            # those routes is unrelated to the next controlled
                            # fixture mutation, so establish a quiet terminal
                            # baseline before making the exact one-route causal
                            # assertion below.
                            cold_search, cold = (
                                self.wait_for_isolated_continuous_baseline(
                                    corpus, env, root, daemon.pid, resources
                                )
                            )
                        self.assert_counts(cold, corpus, [])
                        self.assert_result(cold_search, corpus, corpus.cold_body, env, root)
                        mutations: list[FamilyMutation] = []
                        previous = cold
                        for iteration in range(1, mutation_count + 1):
                            time.sleep(MUTATION_CADENCE_SECONDS)
                            if corpus.family == SQLITE_WAL_FAMILY:
                                # A completed SQLite online backup can emit a
                                # delayed native watcher event after the test
                                # samples its generation. Drain that work before
                                # each controlled mutation so the strict direct
                                # predecessor assertion remains causal.
                                _, previous = (
                                    self.wait_for_isolated_continuous_baseline(
                                        corpus, env, root, daemon.pid, resources
                                    )
                                )
                                self.assert_counts(previous, corpus, mutations)
                            source_before = immutable_tree_snapshot(corpus.source_root)
                            mutation = corpus.continuous_mutation(iteration)
                            mutations.append(mutation)
                            source_after_mutation = immutable_tree_snapshot(corpus.source_root)
                            self.assertNotEqual(source_after_mutation, source_before)

                            search = self.wait_for_searchable(
                                mutation.query,
                                corpus,
                                env,
                                root,
                                daemon.pid,
                                resources,
                            )
                            snapshot = refresh_snapshot(search, root, env)
                            self.assertEqual(
                                immutable_tree_snapshot(corpus.source_root),
                                source_after_mutation,
                                "the provider source changed after the controlled "
                                "harness mutation",
                            )
                            self.assert_counts(snapshot, corpus, mutations)
                            self.assert_incremental_watcher_job(
                                snapshot, previous, env, corpus
                            )
                            self.assert_result(search, corpus, mutation.body, env, root)
                            if mutation.previous_query is not None:
                                self.assert_absent(
                                    mutation.previous_query, corpus, env, root
                                )
                            self.assertGreaterEqual(len(snapshot.manifest_names), 1)
                            self.assertLessEqual(
                                len(snapshot.manifest_names),
                                len(cold.manifest_names) + iteration,
                            )
                            self.assertLessEqual(
                                len(snapshot.segments), len(cold.segments) + iteration
                            )
                            storage_limit = cold.index_bytes + sum(
                                item.storage_payload_bytes
                                + MAX_INCREMENTAL_SEGMENT_OVERHEAD_BYTES
                                for item in mutations
                            )
                            self.assertLessEqual(snapshot.index_bytes, storage_limit)
                            previous = snapshot

                        self.assertLessEqual(
                            resources["peak_open_fds"]
                            - resources["baseline_open_fds"],
                            MAX_OPEN_FD_DELTA,
                        )
                        self.assertLessEqual(
                            resources["peak_rss_bytes"], MAX_PEAK_RSS_BYTES
                        )
                        print(
                            "provider-family continuous refresh:"
                            f" provider={corpus.provider}"
                            f" mutations={mutation_count}"
                            f" peak_fd_delta="
                            f"{resources['peak_open_fds'] - resources['baseline_open_fds']}"
                            f" peak_rss_bytes={resources['peak_rss_bytes']}"
                            f" storage_growth={previous.index_bytes - cold.index_bytes}"
                        )
                    finally:
                        stop_daemon(daemon, daemon_stdout, daemon_stderr, root, env)
                finally:
                    corpus.close()


if __name__ == "__main__":
    unittest.main()
