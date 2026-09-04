#!/usr/bin/env python3
"""Linux qualification for watcher-driven representative-family refreshes."""

from __future__ import annotations

from collections.abc import Callable
import json
import os
from pathlib import Path
import sys
import tempfile
import threading
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
FD_SAMPLE_INTERVAL_SECONDS = 0.005


class ContinuousFdSampler:
    def __init__(
        self,
        sample: Callable[[], int],
        interval_seconds: float = FD_SAMPLE_INTERVAL_SECONDS,
    ) -> None:
        self._sample = sample
        self._interval_seconds = interval_seconds
        self._stop = threading.Event()
        self._ready = threading.Event()
        self._peak: int | None = None
        self._failure: Exception | None = None
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        if self._thread is not None:
            raise RuntimeError("continuous FD sampler already started")
        self._thread = threading.Thread(
            target=self._sample_until_stopped,
            name="ctx-continuous-fd-sampler",
            daemon=True,
        )
        self._thread.start()
        if not self._ready.wait(COMMAND_TIMEOUT_SECONDS):
            self._stop.set()
            self._join()
            raise AssertionError("continuous FD sampler did not produce its first sample")
        if self._failure is not None:
            self._join()
            self._raise_failure()

    def stop(self) -> int:
        if self._thread is None:
            raise RuntimeError("continuous FD sampler was not started")
        self._stop.set()
        self._join()
        self._raise_failure()
        if self._peak is None:
            raise AssertionError("continuous FD sampler stopped without a sample")
        return self._peak

    def _join(self) -> None:
        assert self._thread is not None
        self._thread.join(COMMAND_TIMEOUT_SECONDS)
        if self._thread.is_alive():
            raise AssertionError("continuous FD sampler did not stop")

    def _raise_failure(self) -> None:
        if self._failure is not None:
            raise RuntimeError("continuous FD sampler failed") from self._failure

    def _sample_until_stopped(self) -> None:
        try:
            while not self._stop.is_set():
                observed = self._sample()
                self._peak = observed if self._peak is None else max(self._peak, observed)
                self._ready.set()
                self._stop.wait(self._interval_seconds)
        except Exception as error:  # sampler failures must fail the qualification
            self._failure = error
            self._ready.set()


class ContinuousFdSamplerTest(unittest.TestCase):
    def test_records_a_peak_before_search_polling_begins(self) -> None:
        sample_count = 0
        pre_search_peak_sampled = threading.Event()
        search_polling_started = threading.Event()

        def sample() -> int:
            nonlocal sample_count
            sample_count += 1
            if sample_count == 1:
                return 11
            if not search_polling_started.is_set():
                pre_search_peak_sampled.set()
                return 107
            return 11

        sampler = ContinuousFdSampler(sample)
        sampler.start()
        self.assertTrue(
            pre_search_peak_sampled.wait(COMMAND_TIMEOUT_SECONDS),
            "sampler did not observe the synchronized pre-search phase",
        )
        search_polling_started.set()

        self.assertEqual(sampler.stop(), 107)

    def test_propagates_sampling_failures_after_joining(self) -> None:
        sample_count = 0
        failure_sampled = threading.Event()

        def sample() -> int:
            nonlocal sample_count
            sample_count += 1
            if sample_count == 1:
                return 11
            failure_sampled.set()
            raise OSError("injected sampler failure")

        sampler = ContinuousFdSampler(sample)
        sampler.start()
        self.assertTrue(failure_sampled.wait(COMMAND_TIMEOUT_SECONDS))
        with self.assertRaisesRegex(RuntimeError, "continuous FD sampler failed"):
            sampler.stop()
        assert sampler._thread is not None
        self.assertFalse(sampler._thread.is_alive())


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
                        ["setup", "--no-daemon", "--progress", "none"],
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
                        cold_search, cold = (
                            self.wait_for_isolated_continuous_baseline(
                                corpus, env, root, daemon.pid, resources
                            )
                        )
                        self.assert_counts(cold, corpus, [])
                        self.assert_result(cold_search, corpus, corpus.cold_body, env, root)
                        mutations: list[FamilyMutation] = []
                        previous = cold
                        resources["baseline_open_fds"] = linux_open_fd_count(daemon.pid)
                        resources["peak_open_fds"] = resources["baseline_open_fds"]
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
                            fd_sampler = ContinuousFdSampler(
                                lambda: linux_open_fd_count(daemon.pid)
                            )
                            fd_sampler.start()
                            try:
                                source_before = immutable_tree_snapshot(corpus.source_root)
                                mutation = corpus.continuous_mutation(iteration)
                                mutations.append(mutation)
                                source_after_mutation = immutable_tree_snapshot(
                                    corpus.source_root
                                )
                                self.assertNotEqual(
                                    source_after_mutation, source_before
                                )

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
                                self.assert_result(
                                    search, corpus, mutation.body, env, root
                                )
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
                                    len(snapshot.segments),
                                    len(cold.segments) + iteration,
                                )
                                storage_limit = cold.index_bytes + sum(
                                    item.storage_payload_bytes
                                    + MAX_INCREMENTAL_SEGMENT_OVERHEAD_BYTES
                                    for item in mutations
                                )
                                self.assertLessEqual(snapshot.index_bytes, storage_limit)
                                previous = snapshot
                            finally:
                                resources["peak_open_fds"] = max(
                                    resources["peak_open_fds"], fd_sampler.stop()
                                )

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
                            f" baseline_open_fds={resources['baseline_open_fds']}"
                            f" peak_open_fds={resources['peak_open_fds']}"
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
