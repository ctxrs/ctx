#!/usr/bin/env python3
"""Real-product refresh, resource, and top-provider nightly sanity."""

from __future__ import annotations

from dataclasses import dataclass
import datetime as dt
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

from performance_family_fixtures import SourceFamilyTaxonomyTest
from performance_sanity_support import (
    COMMAND_TIMEOUT_SECONDS,
    FORCE_SINGLE_CPU_ENV,
    MAX_COMMAND_SECONDS,
    MAX_PEAK_RSS_BYTES,
    RefreshPerformanceSample,
    RefreshSnapshot,
    SourceWorkerCpu,
    command_failure,
    directory_bytes,
    isolated_env,
    published_file_state,
    require_parallel_source_workers,
    refresh_snapshot,
    run_checked,
    run_json,
    run_json_timed,
    run_refresh_measured,
    start_daemon,
    stop_daemon,
    task_binary,
)
from performance_family_runtime_test import SourceFamilyColdRefreshPerformanceTest


EVENT_COUNT = 64
QUERY = "nightly performance sentinel"
APPEND_QUERY = f"{QUERY} tiny append"
TOP_PROVIDER_QUERY = "ctxtopproviderperfsentinel"
SAMPLE_COUNT = 3
# The checked debug build measures about 23 KiB of immutable Tantivy segment,
# metadata, and manifest growth for one tiny append. Keep one fixed 32 KiB
# allowance instead of scaling retained storage with the existing corpus.
MAX_APPEND_SEGMENT_OVERHEAD_BYTES = 32 * 1024

# Normal CI keeps the small provider/scheduler contracts. Nightly and release
# add enough independent leaves to require multiple source workers while
# keeping the generated corpus bounded to tens of MiB.
TOP_PROVIDER_FILE_COUNT = 64
TOP_PROVIDER_EVENTS_PER_FILE = 64
TOP_PROVIDER_TEXT_BYTES = 1_536
TOP_PROVIDER_COUNT = 3

# Process CPU divided by wall time has a physical single-CPU ceiling of 1.0.
# This speed-independent margin rejects serialization while tolerating ordinary
# scheduler/accounting noise over the complete multi-second cold refresh.
MIN_COLD_CPU_PER_WALL = 1.10
MIN_COLD_SPEEDUP_OVER_SERIAL = 1.20


@dataclass(frozen=True)
class CommandSample:
    packet: dict[str, object]
    elapsed_seconds: float
    peak_rss_bytes: int | None


@dataclass(frozen=True)
class RepresentativeCorpus:
    codex_root: Path
    claude_root: Path
    cursor_root: Path
    fixture_bytes: int

    @property
    def source_count(self) -> int:
        return TOP_PROVIDER_COUNT * TOP_PROVIDER_FILE_COUNT

    @property
    def retained_records(self) -> int:
        return (
            TOP_PROVIDER_COUNT
            * TOP_PROVIDER_FILE_COUNT
            * TOP_PROVIDER_EVENTS_PER_FILE
        )

    @property
    def ignored_records(self) -> int:
        return TOP_PROVIDER_FILE_COUNT

    @property
    def complete_records(self) -> int:
        return self.retained_records + self.ignored_records

    def root(self, provider: str) -> Path:
        return {
            "codex": self.codex_root,
            "claude": self.claude_root,
            "cursor": self.cursor_root,
        }[provider]


def json_line(value: object) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True) + "\n"


def write_codex_fixture(home: Path) -> tuple[Path, int]:
    session_path = (
        home
        / ".codex"
        / "sessions"
        / "2026"
        / "07"
        / "30"
        / "nightly-performance.jsonl"
    )
    session_path.parent.mkdir(parents=True)
    session_id = "019fb4a0-1111-7777-8888-000000000001"
    base = dt.datetime(2026, 7, 30, 12, tzinfo=dt.timezone.utc)
    lines = [
        json_line(
            {
                "timestamp": base.isoformat().replace("+00:00", "Z"),
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "timestamp": base.isoformat().replace("+00:00", "Z"),
                    "cwd": "/workspace/ctx",
                    "originator": "codex-cli",
                    "cli_version": "1.0.0-test",
                    "source": "cli",
                    "model_provider": "openai",
                },
            }
        )
    ]
    for index in range(EVENT_COUNT):
        instant = base + dt.timedelta(milliseconds=index + 1)
        assistant = index % 2 == 1
        lines.append(
            json_line(
                {
                    "timestamp": instant.isoformat().replace("+00:00", "Z"),
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant" if assistant else "user",
                        "content": [
                            {
                                "type": "output_text" if assistant else "input_text",
                                "text": f"{QUERY} event {index:03d}",
                            }
                        ],
                        **({"phase": "commentary"} if assistant else {}),
                    },
                }
            )
        )
    body = "".join(lines).encode()
    session_path.write_bytes(body)
    return session_path, len(body)


def append_codex_event(session_path: Path) -> int:
    instant = dt.datetime(2026, 7, 30, 12, 0, 1, tzinfo=dt.timezone.utc)
    body = json_line(
        {
            "timestamp": instant.isoformat().replace("+00:00", "Z"),
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": APPEND_QUERY}],
                "phase": "commentary",
            },
        }
    ).encode()
    with session_path.open("ab") as fixture:
        fixture.write(body)
    return len(body)


def representative_timestamp(event_index: int) -> str:
    instant = dt.datetime(
        2026, 7, 30, 12, tzinfo=dt.timezone.utc
    ) + dt.timedelta(milliseconds=event_index)
    return instant.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def representative_text(label: str) -> str:
    prefix = f"{label} "
    if len(prefix) >= TOP_PROVIDER_TEXT_BYTES:
        raise ValueError("representative fixture label exceeds its fixed body size")
    filler = "0123456789abcdef"
    text = prefix + (
        filler
        * (
            (TOP_PROVIDER_TEXT_BYTES - len(prefix) + len(filler) - 1)
            // len(filler)
        )
    )[: TOP_PROVIDER_TEXT_BYTES - len(prefix)]
    if len(text.encode("ascii")) != TOP_PROVIDER_TEXT_BYTES:
        raise AssertionError("representative fixture text has the wrong byte count")
    return text


def write_json_lines(path: Path, records: list[object]) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as output:
        for record in records:
            output.write(json_line(record).encode("utf-8"))
    return path.stat().st_size


def codex_session_id(file_index: int) -> str:
    return f"019fb4a0-1111-7777-8888-{file_index:012x}"


def codex_message(file_index: int, event_index: int) -> object:
    assistant = event_index % 2 == 1
    return {
        "timestamp": representative_timestamp(event_index + 1),
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant" if assistant else "user",
            "content": [
                {
                    "type": "output_text" if assistant else "input_text",
                    "text": representative_text(
                        f"{TOP_PROVIDER_QUERY} provider=codex"
                        f" file={file_index:03d} event={event_index:03d}"
                    ),
                }
            ],
            **({"phase": "commentary"} if assistant else {}),
        },
    }


def claude_message(file_index: int, event_index: int) -> object:
    role = "assistant" if event_index % 2 == 1 else "user"
    return {
        "sessionId": f"claude-perf-{file_index:03d}",
        "timestamp": representative_timestamp(event_index + 1),
        "cwd": "/workspace/claude",
        "version": "test",
        "type": role,
        "message": {
            "role": role,
            "content": [
                {
                    "type": "text",
                    "text": representative_text(
                        f"{TOP_PROVIDER_QUERY} provider=claude"
                        f" file={file_index:03d} event={event_index:03d}"
                    ),
                }
            ],
        },
        "uuid": f"claude-perf-{file_index:03d}-{event_index:03d}",
    }


def cursor_message(file_index: int, event_index: int) -> object:
    role = "assistant" if event_index % 2 == 1 else "user"
    return {
        "timestamp": representative_timestamp(event_index + 1),
        "role": role,
        "message": {
            "role": role,
            "content": [
                {
                    "type": "text",
                    "text": representative_text(
                        f"{TOP_PROVIDER_QUERY} provider=cursor"
                        f" file={file_index:03d} event={event_index:03d}"
                    ),
                }
            ],
        },
    }


def write_representative_corpus(home: Path) -> RepresentativeCorpus:
    codex_root = home / ".codex" / "sessions"
    claude_root = home / ".claude" / "projects"
    cursor_root = home / ".cursor" / "projects"
    fixture_bytes = 0
    for file_index in range(TOP_PROVIDER_FILE_COUNT):
        session_id = codex_session_id(file_index)
        codex_records = [
            {
                "timestamp": representative_timestamp(0),
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "timestamp": representative_timestamp(0),
                    "cwd": "/workspace/codex",
                    "originator": "codex-cli",
                    "cli_version": "1.0.0-test",
                    "source": "cli",
                    "model_provider": "openai",
                },
            }
        ]
        codex_records.extend(
            codex_message(file_index, event_index)
            for event_index in range(TOP_PROVIDER_EVENTS_PER_FILE)
        )
        fixture_bytes += write_json_lines(
            codex_root / "2026" / "07" / "30" / f"{session_id}.jsonl",
            codex_records,
        )

        fixture_bytes += write_json_lines(
            claude_root
            / "-workspace"
            / f"claude-perf-{file_index:03d}.jsonl",
            [
                claude_message(file_index, event_index)
                for event_index in range(TOP_PROVIDER_EVENTS_PER_FILE)
            ],
        )

        cursor_session = f"cursor-perf-{file_index:03d}"
        fixture_bytes += write_json_lines(
            cursor_root
            / "workspace"
            / "agent-transcripts"
            / cursor_session
            / f"{cursor_session}.jsonl",
            [
                cursor_message(file_index, event_index)
                for event_index in range(TOP_PROVIDER_EVENTS_PER_FILE)
            ],
        )
    return RepresentativeCorpus(
        codex_root=codex_root,
        claude_root=claude_root,
        cursor_root=cursor_root,
        fixture_bytes=fixture_bytes,
    )


def linux_peak_rss_bytes(pid: int) -> int | None:
    status_path = Path("/proc") / str(pid) / "status"
    try:
        fields = status_path.read_text(encoding="ascii").splitlines()
    except (FileNotFoundError, PermissionError, ProcessLookupError):
        return None
    values: dict[str, int] = {}
    for line in fields:
        name, separator, raw = line.partition(":")
        if separator and name in {"VmHWM", "VmRSS"}:
            parts = raw.split()
            if len(parts) == 2 and parts[1] == "kB":
                values[name] = int(parts[0]) * 1024
    return values.get("VmHWM", values.get("VmRSS"))


def run_measured(
    args: list[str], env: dict[str, str], cwd: Path
) -> CommandSample:
    started = time.monotonic()
    with tempfile.TemporaryFile(mode="w+b", dir=cwd) as stdout_file, (
        tempfile.TemporaryFile(mode="w+b", dir=cwd)
    ) as stderr_file:
        process = subprocess.Popen(
            [task_binary(env), *args],
            cwd=cwd,
            env=env,
            stdout=stdout_file,
            stderr=stderr_file,
        )
        peak_rss_bytes: int | None = None
        deadline = started + COMMAND_TIMEOUT_SECONDS
        while process.poll() is None:
            observed = linux_peak_rss_bytes(process.pid)
            if observed is not None:
                peak_rss_bytes = max(peak_rss_bytes or 0, observed)
            if time.monotonic() >= deadline:
                process.kill()
                process.wait()
                stdout_file.seek(0)
                stderr_file.seek(0)
                raise TimeoutError(
                    f"{' '.join(args)} exceeded {COMMAND_TIMEOUT_SECONDS}s\n"
                    f"stdout:\n{stdout_file.read().decode(errors='replace')}\n"
                    f"stderr:\n{stderr_file.read().decode(errors='replace')}"
                )
            time.sleep(0.002)
        stdout_file.seek(0)
        stderr_file.seek(0)
        stdout = stdout_file.read()
        stderr = stderr_file.read()
    elapsed_seconds = time.monotonic() - started
    if process.returncode != 0:
        raise command_failure(args, process.returncode, stdout, stderr)
    packet = json.loads(stdout)
    if not isinstance(packet, dict):
        raise RuntimeError(f"{' '.join(args)} did not return a JSON object")
    return CommandSample(packet, elapsed_seconds, peak_rss_bytes)


class SourceWorkerParallelismOracleTest(unittest.TestCase):
    @staticmethod
    def sample(source_workers: tuple[SourceWorkerCpu, ...]) -> RefreshPerformanceSample:
        return RefreshPerformanceSample(
            packet={},
            elapsed_seconds=2.0,
            cpu_seconds=5.0,
            cpu_per_wall=2.5,
            baseline_open_fds=10,
            peak_open_fds=20,
            peak_rss_bytes=128 * 1024 * 1024,
            source_workers=source_workers,
        )

    def test_repeated_single_scanner_cannot_borrow_tantivy_cpu(self) -> None:
        sample = self.sample(
            (
                SourceWorkerCpu(101, "ctx-src-scan00", 12),
                SourceWorkerCpu(102, "ctx-src-scan00", 9),
                SourceWorkerCpu(103, "ctx-src-scan00", 7),
            )
        )

        with self.assertRaisesRegex(
            AssertionError, "at least two distinct named source-worker slots"
        ):
            require_parallel_source_workers(sample)

    def test_two_scanners_with_meaningful_cpu_satisfy_the_oracle(self) -> None:
        sample = self.sample(
            (
                SourceWorkerCpu(101, "ctx-src-scan00", 12),
                SourceWorkerCpu(102, "ctx-src-scan01", 9),
                SourceWorkerCpu(103, "ctx-src-scan02", 0),
            )
        )

        self.assertEqual(
            require_parallel_source_workers(sample),
            sample.source_workers[:2],
        )


class PhysicalStorageAccountingTest(unittest.TestCase):
    def test_hard_linked_generation_files_count_once(self) -> None:
        with tempfile.TemporaryDirectory(
            prefix="ctx-performance-storage-accounting-"
        ) as temporary:
            root = Path(temporary)
            original = root / "generation-a" / "segment"
            linked = root / "generation-b" / "segment"
            copied = root / "generation-c" / "segment"
            original.parent.mkdir()
            linked.parent.mkdir()
            copied.parent.mkdir()
            original.write_bytes(b"physical-segment")
            os.link(original, linked)
            copied.write_bytes(original.read_bytes())

            self.assertEqual(directory_bytes(root), 2 * len(b"physical-segment"))


class SmallQueryShowPerformanceTest(unittest.TestCase):
    def test_refresh_query_and_show_stay_within_sanity_bounds(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ctx-performance-sanity-") as temporary:
            root = Path(temporary)
            home = root / "home"
            home.mkdir()
            fixture_path, fixture_bytes = write_codex_fixture(home)
            env = isolated_env(root, home)
            run_checked(
                ["setup", "--catalog-only", "--no-daemon", "--progress", "none"],
                env,
                root,
            )
            daemon, daemon_stdout, daemon_stderr = start_daemon(root, env)
            try:
                initial_search, initial_refresh_seconds = run_json_timed(
                    [
                        "search",
                        QUERY,
                        "--refresh",
                        "wait",
                        "--format=json",
                        "--limit",
                        "1",
                    ],
                    env,
                    root,
                )
                self.assertTrue(initial_search.get("results"))
                initial = refresh_snapshot(initial_search, root, env)
                self.assertTrue(initial.segments)

                noop_search, noop_refresh_seconds = run_json_timed(
                    [
                        "search",
                        QUERY,
                        "--refresh",
                        "wait",
                        "--format=json",
                        "--limit",
                        "1",
                    ],
                    env,
                    root,
                )
                noop = refresh_snapshot(noop_search, root, env)
                self.assertNotEqual(noop.request_id, initial.request_id)
                self.assertFalse(noop.generation_changed)
                self.assertEqual(noop.previous_generation, initial.generation_id)
                self.assertEqual(noop.generation_id, initial.generation_id)
                self.assertEqual(noop.indexed_documents, initial.indexed_documents)
                # `receipt.current` is generation state, not per-command
                # attribution. Comparing the complete object with immutable
                # publication state keeps this no-work assertion truthful.
                self.assertEqual(noop.current, initial.current)
                self.assertEqual(noop.opstamp, initial.opstamp)
                self.assertEqual(noop.segments, initial.segments)
                self.assertEqual(noop.meta, initial.meta)
                self.assertEqual(noop.manifest, initial.manifest)
                self.assertEqual(noop.manifest_names, initial.manifest_names)
                self.assertEqual(noop.index_bytes, initial.index_bytes)

                append_bytes = append_codex_event(fixture_path)
                appended_search, append_refresh_seconds = run_json_timed(
                    [
                        "search",
                        APPEND_QUERY,
                        "--refresh",
                        "wait",
                        "--format=json",
                        "--limit",
                        "1",
                    ],
                    env,
                    root,
                )
                appended_results = appended_search.get("results")
                self.assertIsInstance(appended_results, list)
                self.assertEqual(len(appended_results), 1)
                self.assertIn(APPEND_QUERY, appended_results[0].get("snippet", ""))
                appended = refresh_snapshot(appended_search, root, env)
                for refresh_seconds in (
                    initial_refresh_seconds,
                    noop_refresh_seconds,
                    append_refresh_seconds,
                ):
                    self.assertLessEqual(refresh_seconds, MAX_COMMAND_SECONDS)
                self.assertNotEqual(appended.request_id, noop.request_id)
                self.assertNotEqual(appended.generation_id, noop.generation_id)
                # The persistent filesystem watcher can win the append race
                # before this explicit wait request. In that case the request
                # is truthfully a no-op over the already-published successor.
                self.assertEqual(
                    appended.previous_generation,
                    (
                        noop.generation_id
                        if appended.generation_changed
                        else appended.generation_id
                    ),
                )
                self.assertEqual(
                    appended.indexed_documents, noop.indexed_documents + 1
                )
                expected_current = dict(noop.current)
                for field, delta in {
                    "current_indexed_documents": 1,
                    "current_complete_records": 1,
                    "current_retained_records": 1,
                    "current_certified_source_bytes": append_bytes,
                }.items():
                    expected_current[field] = int(noop.current[field]) + delta
                self.assertEqual(appended.current, expected_current)
                self.assertGreater(appended.opstamp, noop.opstamp)
                self.assertLessEqual(
                    len(appended.segments),
                    len(noop.segments) + 1,
                    "one tiny append exposed more than one additional active segment",
                )
                append_storage_delta = appended.index_bytes - noop.index_bytes
                self.assertGreaterEqual(append_storage_delta, 0)
                self.assertLessEqual(
                    append_storage_delta,
                    append_bytes + MAX_APPEND_SEGMENT_OVERHEAD_BYTES,
                    "one tiny append exceeded its payload plus the fixed "
                    "append-segment storage allowance",
                )
                self.assertEqual(
                    len(appended.manifest_names), len(noop.manifest_names) + 1
                )
                self.assertEqual(
                    published_file_state(
                        Path(env["CTX_DATA_ROOT"])
                        / "search"
                        / "lexical"
                        / "ctx-generations"
                        / f"{initial.generation_id}.json"
                    ),
                    initial.manifest,
                )

                search_samples = [
                    run_measured(
                        [
                            "search",
                            QUERY,
                            "--refresh",
                            "off",
                            "--format=json",
                            "--limit",
                            "10",
                        ],
                        env,
                        root,
                    )
                    for _ in range(SAMPLE_COUNT)
                ]
                results = search_samples[-1].packet.get("results")
                self.assertIsInstance(results, list)
                self.assertTrue(results)
                session_id = results[0].get("ctx_session_id")
                self.assertIsInstance(session_id, str)
                self.assertTrue(session_id)
                show_samples = [
                    run_measured(
                        [
                            "show",
                            "session",
                            session_id,
                            "--mode",
                            "lite",
                            "--format",
                            "json",
                        ],
                        env,
                        root,
                    )
                    for _ in range(SAMPLE_COUNT)
                ]
            finally:
                stop_daemon(
                    daemon,
                    daemon_stdout,
                    daemon_stderr,
                    root,
                    env,
                )

        shown_id = show_samples[-1].packet.get(
            "ctx_session_id", show_samples[-1].packet.get("id")
        )
        self.assertEqual(shown_id, session_id)
        self.assertLessEqual(
            max(sample.elapsed_seconds for sample in search_samples),
            MAX_COMMAND_SECONDS,
        )
        self.assertLessEqual(
            max(sample.elapsed_seconds for sample in show_samples),
            MAX_COMMAND_SECONDS,
        )
        measured_rss = [
            sample.peak_rss_bytes
            for sample in (*search_samples, *show_samples)
            if sample.peak_rss_bytes is not None
        ]
        if sys.platform.startswith("linux"):
            self.assertEqual(len(measured_rss), SAMPLE_COUNT * 2)
        for peak_rss_bytes in measured_rss:
            self.assertLessEqual(peak_rss_bytes, MAX_PEAK_RSS_BYTES)

        search_max = max(sample.elapsed_seconds for sample in search_samples)
        show_max = max(sample.elapsed_seconds for sample in show_samples)
        rss_max = max(measured_rss, default=0)
        append_complete_delta = int(
            appended.current["current_complete_records"]
        ) - int(noop.current["current_complete_records"])
        append_retained_delta = int(
            appended.current["current_retained_records"]
        ) - int(noop.current["current_retained_records"])
        append_source_bytes_delta = int(
            appended.current["current_certified_source_bytes"]
        ) - int(noop.current["current_certified_source_bytes"])
        print(
            "performance sanity:"
            f" fixture_events={EVENT_COUNT + 1}"
            f" initial_fixture_bytes={fixture_bytes}"
            f" append_bytes={append_bytes}"
            f" append_request_generation_changed="
            f"{str(appended.generation_changed).lower()}"
            f" noop_generation_changed={str(noop.generation_changed).lower()}"
            f" noop_current_unchanged=true"
            f" noop_publication_unchanged=true"
            f" noop_opstamp={noop.opstamp}"
            f" append_document_delta="
            f"{appended.indexed_documents - noop.indexed_documents}"
            f" append_complete_record_delta={append_complete_delta}"
            f" append_retained_record_delta={append_retained_delta}"
            f" append_source_bytes_delta={append_source_bytes_delta}"
            f" append_opstamp={appended.opstamp}"
            f" initial_refresh_seconds={initial_refresh_seconds:.3f}"
            f" noop_refresh_seconds={noop_refresh_seconds:.3f}"
            f" append_refresh_seconds={append_refresh_seconds:.3f}"
            f" segments_before={len(noop.segments)}"
            f" segments_after={len(appended.segments)}"
            f" index_bytes_before={initial.index_bytes}"
            f" index_bytes_after={appended.index_bytes}"
            f" append_storage_delta={append_storage_delta}"
            f" append_segment_overhead_bytes="
            f"{append_storage_delta - append_bytes}"
            f" search_max_seconds={search_max:.3f}"
            f" show_max_seconds={show_max:.3f}"
            f" peak_rss_bytes={rss_max}"
        )


@unittest.skipUnless(
    sys.platform.startswith("linux")
    and hasattr(os, "sched_getaffinity")
    and Path("/proc/self/stat").is_file(),
    "top-provider CPU overlap evidence requires Linux /proc and affinity",
)
class TopProviderColdRefreshPerformanceTest(unittest.TestCase):
    MIN_AVAILABLE_CPUS = 12

    def assert_representative_refresh(
        self,
        search: dict[str, object],
        root: Path,
        env: dict[str, str],
        corpus: RepresentativeCorpus,
    ) -> RefreshSnapshot:
        self.assertEqual(
            search["freshness"],
            {
                "mode": "wait",
                "source_count": TOP_PROVIDER_COUNT,
                "status": "completed",
            },
        )
        snapshot = refresh_snapshot(search, root, env)
        status = snapshot.status
        job = snapshot.job
        self.assertEqual(job["status"], "completed")
        self.assertEqual(job["request_state"], "published")
        self.assertEqual(job["source_count"], TOP_PROVIDER_COUNT)
        self.assertEqual(
            job["progress"],
            {
                "completed_sources": TOP_PROVIDER_COUNT,
                "phase": "published",
                "total_sources": TOP_PROVIDER_COUNT,
            },
        )
        self.assertTrue(job["generation_changed"])
        self.assertEqual(job["certified_source_count"], corpus.source_count)
        self.assertEqual(job["certified_source_bytes"], corpus.fixture_bytes)
        expected_current = {
            "current_certified_source_bytes": corpus.fixture_bytes,
            "current_complete_records": corpus.complete_records,
            "current_ignored_records": corpus.ignored_records,
            "current_indexed_documents": corpus.retained_records,
            "current_rejected_records": 0,
            "current_retained_records": corpus.retained_records,
            "current_source_count": corpus.source_count,
            "current_sources_with_rejections": 0,
            "removed_source_count": 0,
        }
        self.assertEqual(snapshot.current, expected_current)
        self.assertEqual(snapshot.indexed_documents, corpus.retained_records)
        self.assertEqual(status["indexed_events"], corpus.retained_records)
        self.assertEqual(status["indexed_items"], corpus.retained_records)
        self.assertEqual(status["indexed_sources"], corpus.source_count)
        self.assertEqual(
            status["lexical"]["indexed_documents"], corpus.retained_records
        )
        self.assertEqual(
            status["lexical"]["certified_sources"], corpus.source_count
        )
        self.assertEqual(
            status["lexical"]["certified_source_bytes"],
            corpus.fixture_bytes,
        )
        self.assertEqual(
            status["lexical"]["generation_id"], snapshot.generation_id
        )
        self.assertGreater(job["timings_us"]["scan_stage"], 0)
        self.assertTrue(snapshot.segments)
        return snapshot

    def assert_complete_core_content(
        self,
        root: Path,
        env: dict[str, str],
        corpus: RepresentativeCorpus,
    ) -> None:
        source_formats = {
            "codex": "codex_session_jsonl",
            "claude": "claude_projects_jsonl_tree",
            "cursor": "cursor_agent_transcript_jsonl_tree",
        }
        for provider in ("codex", "claude", "cursor"):
            search = run_json(
                [
                    "search",
                    TOP_PROVIDER_QUERY,
                    "--provider",
                    provider,
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
            self.assertEqual(result["provider"], provider)
            self.assertEqual(result["source_format"], source_formats[provider])
            self.assertNotIn("source_path", result)
            show = run_json(
                [
                    "show",
                    "event",
                    result["ctx_event_id"],
                    "--format=json",
                ],
                env,
                root,
            )
            self.assertEqual(show["payload_type"], "event_window")
            event = show["event"]
            self.assertEqual(event["provider"], provider)
            self.assertEqual(event["ctx_event_id"], result["ctx_event_id"])
            self.assertEqual(
                len(event["text"].encode("ascii")),
                TOP_PROVIDER_TEXT_BYTES,
            )
            self.assertIn(TOP_PROVIDER_QUERY, event["text"])
            self.assertIn(f"provider={provider}", event["text"])
            self.assertEqual(
                event["content"],
                {
                    "complete": True,
                    "policy_status": "selected",
                },
            )

    def test_representative_top_provider_cold_refresh_overlaps_work(self) -> None:
        available_cpus = set(os.sched_getaffinity(0))
        self.assertGreaterEqual(
            len(available_cpus),
            self.MIN_AVAILABLE_CPUS,
            "nightly top-provider gate requires >=12 available CPUs: 8 Tantivy "
            "indexers + 2 runtime threads + 2 source scanners",
        )
        forced_single_cpu = os.environ.get(FORCE_SINGLE_CPU_ENV) == "1"
        cold, snapshot, corpus = self.run_representative_top_provider_refresh(
            available_cpus,
            force_single_cpu=forced_single_cpu,
            verify_core_content=True,
        )

        source_workers = require_parallel_source_workers(cold)
        source_worker_ticks = ",".join(
            f"{worker.name}:{worker.cpu_ticks}" for worker in source_workers
        )
        self.assertGreaterEqual(
            cold.cpu_per_wall,
            MIN_COLD_CPU_PER_WALL,
            "cold refresh did not use more than one CPU; "
            f"set {FORCE_SINGLE_CPU_ENV}=1 to exercise the serialization control",
        )
        serial_seconds = None
        speedup = None
        if not forced_single_cpu:
            serial, _, _ = self.run_representative_top_provider_refresh(
                available_cpus,
                force_single_cpu=True,
                verify_core_content=False,
            )
            serial_seconds = serial.elapsed_seconds
            speedup = serial.elapsed_seconds / cold.elapsed_seconds
            self.assertGreaterEqual(
                speedup,
                MIN_COLD_SPEEDUP_OVER_SERIAL,
                "parallel cold refresh did not improve wall time over the "
                "same workload pinned to one CPU",
            )
        print(
            "top-provider performance:"
            f" fixture_files={corpus.source_count}"
            f" fixture_events={corpus.retained_records}"
            f" fixture_bytes={corpus.fixture_bytes}"
            f" generation={snapshot.generation_id}"
            f" refresh_seconds={cold.elapsed_seconds:.3f}"
            f" serial_seconds={serial_seconds}"
            f" speedup_over_serial={speedup}"
            f" daemon_cpu_seconds={cold.cpu_seconds:.3f}"
            f" cpu_per_wall={cold.cpu_per_wall:.3f}"
            f" source_worker_slots="
            f"{len({worker.name for worker in source_workers})}"
            f" source_worker_cpu_ticks="
            f"{source_worker_ticks}"
            f" forced_single_cpu={forced_single_cpu}"
        )

    def run_representative_top_provider_refresh(
        self,
        available_cpus: set[int],
        *,
        force_single_cpu: bool,
        verify_core_content: bool,
    ) -> tuple[RefreshPerformanceSample, RefreshSnapshot, RepresentativeCorpus]:
        daemon_affinity = {min(available_cpus)} if force_single_cpu else None
        with tempfile.TemporaryDirectory(
            prefix="ctx-top-provider-performance-"
        ) as temporary:
            root = Path(temporary)
            home = root / "home"
            home.mkdir()
            corpus = write_representative_corpus(home)
            self.assertGreaterEqual(corpus.fixture_bytes, 20 * 1024 * 1024)
            self.assertLessEqual(corpus.fixture_bytes, 64 * 1024 * 1024)
            env = isolated_env(root, home)
            run_checked(
                ["setup", "--catalog-only", "--no-daemon", "--progress", "none"],
                env,
                root,
            )
            daemon, daemon_stdout, daemon_stderr = start_daemon(
                root, env, daemon_affinity
            )
            try:
                cold = run_refresh_measured(
                    [
                        "search",
                        TOP_PROVIDER_QUERY,
                        "--refresh",
                        "wait",
                        "--format=json",
                        "--limit",
                        "3",
                    ],
                    env,
                    root,
                    daemon.pid,
                )
                snapshot = self.assert_representative_refresh(
                    cold.packet, root, env, corpus
                )
                if verify_core_content:
                    self.assert_complete_core_content(root, env, corpus)
            finally:
                stop_daemon(
                    daemon,
                    daemon_stdout,
                    daemon_stderr,
                    root,
                    env,
                )
        return cold, snapshot, corpus


if __name__ == "__main__":
    unittest.main()
