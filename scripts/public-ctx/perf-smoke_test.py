#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("perf-smoke.sh").resolve()
HELPER_ROOT = SCRIPT.with_name("perf-smoke")
HARNESS = HELPER_ROOT / "harness.sh"
HELPERS = (
    HELPER_ROOT / "arguments.sh",
    HELPER_ROOT / "entrypoint.sh",
    HELPER_ROOT / "fixtures.sh",
    HARNESS,
    HELPER_ROOT / "metrics.sh",
    HELPER_ROOT / "report.sh",
    HELPER_ROOT / "runner.sh",
    HELPER_ROOT / "validation.sh",
)
REPO_ROOT = SCRIPT.parents[2]
FROZEN_PYTHON_SOURCE_SHA256 = (
    "aa29a040f9e1ec113a85c9c53b3938d5e96d9b6cd0b7953d2ce9298d587b39bb"
)
PHASES = (
    "initial_source_refresh",
    "noop_source_refresh",
    "append_source_refresh",
    "replacement_source_refresh",
    "delete_source_refresh",
)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def embedded_python_source() -> str:
    return subprocess.run(
        [
            "bash",
            "-c",
            'source "$1"; perf_smoke_python_source',
            "perf-smoke-test",
            str(HARNESS),
        ],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout.removesuffix("\n")


def load_embedded_python() -> dict[str, object]:
    definitions = embedded_python_source().rsplit(
        "\ntry:\n    raise SystemExit(main())",
        1,
    )[0]
    namespace: dict[str, object] = {}
    original_argv = sys.argv
    try:
        sys.argv = [
            "perf-smoke-embedded.py",
            str(REPO_ROOT),
            str(REPO_ROOT),
            "single",
            "/bin/true",
            "test",
            "",
            "",
        ]
        exec(compile(definitions, "perf-smoke-embedded.py", "exec"), namespace)
    finally:
        sys.argv = original_argv
    return namespace


def resource(value: float) -> dict[str, float]:
    return {"p95": value, "max": value}


def fake_run(
    role: str,
    *,
    wall: float = 100.0,
    cpu: float = 100.0,
    rss: float = 100.0,
    read: float = 100.0,
    write: float = 100.0,
    total_io: float = 200.0,
) -> dict[str, object]:
    profiles: dict[str, object] = {}
    for phase in PHASES:
        profiles[phase] = {
            "timings": {"p95_ms": wall},
            "resources": {
                "cpu_total_ms": resource(cpu),
                "peak_rss_bytes": resource(rss),
                "filesystem_read_chars": resource(read),
                "filesystem_write_chars": resource(write),
                "device_read_bytes": resource(read),
                "device_write_bytes": resource(write),
                "device_total_io_bytes": resource(total_io),
                "source_backed_storage_high_water_bytes": resource(write),
                "source_backed_storage_growth_high_water_bytes": resource(write),
            },
        }
    return {"label": role, "role": role, "profiles": profiles}


MOCK_CTX = r"""
#!/usr/bin/env python3
import hashlib
import json
import os
import sys
import time
from pathlib import Path

ROLE = __ROLE__
VERSION = "ctx 0.26.0"


def emit(value):
    print(json.dumps(value, sort_keys=True))


def corpus_snapshot(root):
    snapshot = {}
    source_bytes = 0
    for path in sorted(Path(root).rglob("*.jsonl")):
        body = path.read_text(encoding="utf-8")
        lines = body.splitlines()
        source_bytes += len(body.encode("utf-8"))
        first = json.loads(lines[0])
        snapshot[str(path)] = {
            "events": len(lines) - 2,
            "lines": len(lines),
            "session_id": first["payload"]["id"],
        }
    return snapshot, source_bytes


def generation_id(snapshot):
    body = json.dumps(snapshot, separators=(",", ":"), sort_keys=True).encode()
    return hashlib.sha256(body).hexdigest()


def source_refresh_packet(generation, changed, source_bytes, source_count):
    return {
        "status": "completed",
        "jobs": {
            "source_backed_refresh": {
                "status": "completed",
                "request_state": "published",
                "generation_changed": changed,
                "published_generation": generation,
                "source_count": source_count,
                "certified_source_count": source_count,
                "certified_source_bytes": source_bytes,
                "scanned_routes": 1,
                "timings_us": {"commit": 10, "discovery": 10, "scan_stage": 10},
            }
        },
    }


args = sys.argv[1:]
if args == ["--version"]:
    print(VERSION)
    raise SystemExit(0)

data_root = Path(os.environ["CTX_DATA_ROOT"])
data_root.mkdir(parents=True, exist_ok=True)
codex_home = Path(os.environ["CODEX_HOME"])
corpus_root = codex_home / "sessions"
state_path = data_root / "mock-source-state.json"
running_path = data_root / "mock-daemon-running"
endpoint_path = data_root / "daemon" / "source-refresh-endpoint.json"

if args[:2] == ["daemon", "run"]:
    current, source_bytes = corpus_snapshot(corpus_root)
    previous = json.loads(state_path.read_text()) if state_path.exists() else {}
    generation = generation_id(current)
    changed = current != previous
    state_path.write_text(json.dumps(current, sort_keys=True), encoding="utf-8")
    lexical = data_root / "search" / "lexical"
    lexical.mkdir(parents=True, exist_ok=True)
    (lexical / "meta.json").write_text(
        json.dumps({"generation_id": generation}, sort_keys=True),
        encoding="utf-8",
    )
    (data_root / "relational.sqlite").write_bytes(
        f"source-backed relational {generation}".encode()
    )
    packet = source_refresh_packet(generation, changed, source_bytes, len(current))
    if ROLE == "baseline" and "--once" in args:
        padding = bytearray(2 * 1024 * 1024)
        digest = b"source-backed baseline"
        cpu_deadline = time.process_time() + 0.1
        while time.process_time() < cpu_deadline:
            digest = hashlib.sha256(digest).digest()
        with (data_root / "baseline-source-refresh-pad").open("ab") as handle:
            handle.write(digest + b"x" * (4096 - len(digest)))
            handle.flush()
            os.fsync(handle.fileno())
        time.sleep(0.3)
    if "--once" in args:
        emit(packet)
        raise SystemExit(0)
    endpoint_path.parent.mkdir(parents=True, exist_ok=True)
    endpoint_path.write_text(
        json.dumps({"pid": os.getpid(), "transport": "mock"}),
        encoding="utf-8",
    )
    running_path.write_text(str(os.getpid()), encoding="ascii")
    try:
        while True:
            time.sleep(1)
    finally:
        running_path.unlink(missing_ok=True)
        endpoint_path.unlink(missing_ok=True)

if args[:2] == ["daemon", "status"]:
    current, source_bytes = corpus_snapshot(corpus_root)
    generation = generation_id(current)
    running = running_path.is_file()
    emit({
        "daemon": {
            "running": running,
            "source_refresh_endpoint": {"available": running},
            "jobs": source_refresh_packet(
                generation,
                False,
                source_bytes,
                len(current),
            )["jobs"],
        }
    })
    raise SystemExit(0)

current = json.loads(state_path.read_text()) if state_path.exists() else {}
generation = generation_id(current)
event_count = sum(item["events"] for item in current.values())
session_count = len(current)

if args and args[0] == "status":
    lexical_path = data_root / "search" / "lexical"
    semantic_path = data_root / "search" / "semantic"
    relational_path = data_root / "relational.sqlite"
    emit({
        "schema_version": 2,
        "initialized": True,
        "indexed_items": event_count,
        "indexed_sessions": session_count,
        "indexed_sources": session_count,
        "history_epoch": {
            "name": "v0.26_source_backed",
            "status": "ready",
            "origin": "prior_epoch_preserved",
            "phase": "ready",
        },
        "lexical": {
            "status": "ready",
            "reason": None,
            "path": str(lexical_path),
            "generation_id": generation,
            "indexed_documents": event_count,
        },
        "catalog": {
            "status": "ready",
            "generation_matches": True,
            "generation_id": generation,
            "certified_sources": session_count,
        },
        "semantic": {
            "enabled": False,
            "status": "disabled",
            "flat_f32": {
                "status": "disabled",
                "path": str(semantic_path),
            },
        },
        "relational": {
            "status": "ready",
            "projection_status": "ready",
            "generation_matches": True,
            "active_core_generation_id": generation,
            "path": str(relational_path),
            "session_count": session_count,
            "event_count": event_count,
        },
        "prior_epoch": {
            "status": "preserved",
            "authority": "non_authoritative",
            "preserved": True,
            "active": False,
            "opened": False,
        },
    })
elif args and args[0] == "search":
    first = current[sorted(current)[0]]
    emit({
        "freshness": {"status": "existing_generation"},
        "results": [{
            "ctx_session_id": first["session_id"],
            "ctx_event_id": f"event-{first['session_id']}",
            "provider": "codex",
            "result_scope": "session",
            "source_exists": True,
        }],
    })
elif args and args[0] == "show":
    emit({"id": args[2], "events": []})
else:
    print(f"unsupported mock ctx args: {args}", file=sys.stderr)
    raise SystemExit(2)
"""


class PerfSmokePolicyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.module = load_embedded_python()

    def test_helpers_are_syntax_clean_bounded_and_source_backed(self) -> None:
        subprocess.run(
            ["bash", "-n", str(SCRIPT), *(str(helper) for helper in HELPERS)],
            check=True,
        )
        for path in (SCRIPT, *HELPERS):
            line_count = len(path.read_text(encoding="utf-8").splitlines())
            self.assertLessEqual(line_count, 1000, f"{path}: {line_count} lines")

        source = embedded_python_source()
        compile(source, "perf-smoke-embedded.py", "exec")
        self.assertEqual(
            hashlib.sha256(source.encode()).hexdigest(),
            FROZEN_PYTHON_SOURCE_SHA256,
        )
        for obsolete in (
            "database_path",
            "baseline-v0.25",
            "wal_high_water",
            "wal_growth_high_water",
            "import_command(",
        ):
            self.assertNotIn(obsolete, source)
        self.assertIn("source_refresh_command()", source)
        self.assertIn("prior_epoch_negative_assertion", source)

    def test_shell_argument_contract(self) -> None:
        help_result = subprocess.run(
            [str(SCRIPT), "--help"],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertEqual(help_result.returncode, 0)
        self.assertTrue(
            help_result.stdout.startswith("usage: scripts/public-ctx/perf-smoke.sh")
        )
        self.assertEqual(help_result.stderr, "")

        invalid_result = subprocess.run(
            [str(SCRIPT), "unexpected"],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertEqual(invalid_result.returncode, 2)
        self.assertEqual(invalid_result.stdout, "")

    def test_obsolete_store_qualification_executables_are_absent(self) -> None:
        for path in (
            REPO_ROOT / "scripts/public-ctx/import-upgrade-compat-smoke.sh",
            REPO_ROOT / "scripts/day1-acceptance/performance_runner.py",
        ):
            self.assertFalse(path.exists(), path)

    def test_roles_are_all_source_backed(self) -> None:
        effective_role = self.module["effective_role"]
        harness_error = self.module["HarnessError"]
        for role in ("baseline", "candidate", "single"):
            self.assertEqual(effective_role(role, "ctx 0.26.0"), role)
        with self.assertRaises(harness_error):
            effective_role("baseline-v0.25", "ctx 0.25.0")
        with self.assertRaises(harness_error):
            effective_role("candidate", "ctx 0.25.0")

    def test_storage_footprint_excludes_prior_epoch_sentinel(self) -> None:
        footprint = self.module["source_backed_storage_footprint"]
        with tempfile.TemporaryDirectory(prefix="ctx-perf-footprint-") as temp:
            root = Path(temp)
            (root / "catalogs/explicit-sources").mkdir(parents=True)
            (root / "search/lexical").mkdir(parents=True)
            (root / "search/semantic").mkdir(parents=True)
            (root / "catalogs/explicit-sources/catalog-1.json").write_bytes(b"cat")
            (root / "search/lexical/meta.json").write_bytes(b"lexical")
            (root / "search/semantic/vectors").write_bytes(b"semantic")
            (root / "relational.sqlite").write_bytes(b"relational")
            (root / "work.sqlite").write_bytes(b"x" * 10_000)
            sizes = footprint(root)
        self.assertEqual(
            sizes,
            {
                "catalogs/explicit-sources": 3,
                "search/lexical": 7,
                "search/semantic": 8,
                "relational": 10,
                "total": 28,
            },
        )

    def test_exact_hash_values_are_enforced(self) -> None:
        preflight = self.module["preflight_binary_hashes"]
        env_names = (
            "CTX_PERF_SMOKE_BASELINE_SHA256",
            "CTX_PERF_SMOKE_CANDIDATE_SHA256",
        )
        original = {name: os.environ.get(name) for name in env_names}
        try:
            with tempfile.TemporaryDirectory(prefix="ctx-perf-hash-unit-") as temp:
                root = Path(temp)
                baseline = root / "baseline"
                candidate = root / "candidate"
                baseline.write_bytes(b"exact baseline bytes")
                candidate.write_bytes(b"exact candidate bytes")
                os.environ[env_names[0]] = sha256(baseline)
                os.environ[env_names[1]] = sha256(candidate)
                receipt = preflight(
                    "comparison",
                    True,
                    [
                        (baseline, "baseline", "baseline"),
                        (candidate, "candidate", "candidate"),
                    ],
                )
        finally:
            for name, value in original.items():
                if value is None:
                    os.environ.pop(name, None)
                else:
                    os.environ[name] = value
        self.assertEqual(receipt["status"], "verified")
        self.assertTrue(all(item["matched"] for item in receipt["bindings"]))

    def test_comparison_policy_uses_source_refresh_phases(self) -> None:
        report = self.module["comparison_report"](
            "baseline-first",
            fake_run("baseline"),
            fake_run("candidate"),
            10.0,
            1024.0,
        )
        checks = {check["name"]: check for check in report["checks"]}
        self.assertEqual(report["status"], "passed")
        self.assertIn("delete_source_refresh_wall_parity", checks)
        self.assertEqual(
            checks["initial_source_refresh_device_total_io_amplification"][
                "max_ratio"
            ],
            1.73,
        )
        self.assertTrue(
            all(check["comparison_order"] == "baseline-first" for check in report["checks"])
        )


class PerfSmokeEndToEndTests(unittest.TestCase):
    def write_mock(self, root: Path, role: str) -> Path:
        path = root / f"ctx-{role}"
        source = textwrap.dedent(MOCK_CTX).replace("__ROLE__", repr(role)).lstrip()
        path.write_text(source, encoding="utf-8")
        path.chmod(0o700)
        return path

    def write_execution_sentinel(self, root: Path, role: str, marker: Path) -> Path:
        path = root / f"sentinel-{role}"
        path.write_text(
            "#!/usr/bin/env python3\n"
            "from pathlib import Path\n"
            f"Path({str(marker)!r}).write_text({role!r}, encoding='utf-8')\n"
            "raise SystemExit(97)\n",
            encoding="utf-8",
        )
        path.chmod(0o700)
        return path

    def test_enforced_hash_failure_precedes_binary_and_corpus_execution(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ctx-perf-hash-fail-") as temp:
            root = Path(temp)
            marker = root / "binary-executed"
            baseline = self.write_execution_sentinel(root, "baseline", marker)
            candidate = self.write_execution_sentinel(root, "candidate", marker)
            env = os.environ.copy()
            env.update(
                {
                    "CTX_PUBLIC_CTX_REPO": str(REPO_ROOT),
                    "CTX_PERF_SMOKE_BASELINE_BIN": str(baseline),
                    "CTX_PERF_SMOKE_CANDIDATE_BIN": str(candidate),
                    "CTX_PERF_SMOKE_BASELINE_SHA256": "0" * 64,
                    "CTX_PERF_SMOKE_CANDIDATE_SHA256": sha256(candidate),
                    "CTX_PERF_SMOKE_ENFORCE": "1",
                    "CTX_PERF_SMOKE_WORK_DIR": str(root / "work"),
                    "CTX_PERF_SMOKE_ARTIFACT": str(root / "artifact.json"),
                }
            )
            completed = subprocess.run(
                [str(SCRIPT)],
                cwd=REPO_ROOT,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=10,
            )
            self.assertEqual(completed.returncode, 1)
            self.assertIn("BASELINE_SHA256 does not match", completed.stderr)
            self.assertFalse(marker.exists())
            self.assertFalse((root / "work").exists())

    def test_mock_cli_exercises_both_source_backed_orders(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ctx-perf-smoke-test-") as temp:
            root = Path(temp)
            baseline = self.write_mock(root, "baseline")
            candidate = self.write_mock(root, "candidate")
            artifact_path = root / "artifact.json"
            env = os.environ.copy()
            env.update(
                {
                    "CTX_PUBLIC_CTX_REPO": str(REPO_ROOT),
                    "CTX_PERF_SMOKE_BASELINE_BIN": str(baseline),
                    "CTX_PERF_SMOKE_CANDIDATE_BIN": str(candidate),
                    "CTX_PERF_SMOKE_BASELINE_SHA256": sha256(baseline),
                    "CTX_PERF_SMOKE_CANDIDATE_SHA256": sha256(candidate),
                    "CTX_PERF_SMOKE_BASELINE_LABEL": "source-baseline",
                    "CTX_PERF_SMOKE_CANDIDATE_LABEL": "source-candidate",
                    "CTX_PERF_SMOKE_SESSIONS": "2",
                    "CTX_PERF_SMOKE_LARGE_SESSION_EVENTS": "65",
                    "CTX_PERF_SMOKE_INITIAL_REPEATS": "1",
                    "CTX_PERF_SMOKE_REPEATS": "1",
                    "CTX_PERF_SMOKE_CHANGED_FILES": "1",
                    "CTX_PERF_SMOKE_SAMPLING_INTERVAL_MS": "1",
                    "CTX_PERF_SMOKE_COMMAND_TIMEOUT_SECONDS": "5",
                    "CTX_PERF_SMOKE_TOTAL_TIMEOUT_SECONDS": "60",
                    "CTX_PERF_SMOKE_COMPARISON_ORDER": "both",
                    "CTX_PERF_SMOKE_ENFORCE": "1",
                    "CTX_PERF_SMOKE_WORK_DIR": str(root / "work"),
                    "CTX_PERF_SMOKE_ARTIFACT": str(artifact_path),
                }
            )
            completed = subprocess.run(
                [str(SCRIPT)],
                cwd=REPO_ROOT,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=60,
            )
            self.assertEqual(
                completed.returncode,
                0,
                f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}",
            )
            artifact = json.loads(artifact_path.read_text(encoding="utf-8"))

        self.assertEqual(artifact["schema_version"], 6)
        self.assertEqual(artifact["status"], "passed")
        self.assertEqual(
            artifact["configuration"]["comparison_orders"],
            ["baseline-first", "candidate-first"],
        )
        self.assertEqual(len(artifact["runs"]), 4)
        self.assertEqual(
            [order["execution_sequence"] for order in artifact["execution_orders"]],
            [["baseline", "candidate"], ["candidate", "baseline"]],
        )
        for run in artifact["runs"]:
            self.assertEqual(run["corpus"]["initial_sessions"], 2)
            self.assertEqual(run["corpus"]["final_sessions"], 1)
            self.assertEqual(run["corpus"]["appended_events"], 1)
            self.assertEqual(run["corpus"]["deleted_sessions"], 1)
            self.assertTrue(
                run["corpus"]["source_path"].endswith("/home/.codex/sessions")
            )
            self.assertTrue(run["prior_epoch_negative_assertion"]["untouched"])
            self.assertFalse(run["prior_epoch_negative_assertion"]["opened"])
            self.assertNotIn("work.sqlite", run["storage"]["files"])
            self.assertEqual(
                set(run["storage"]["files"]),
                {
                    "catalogs/explicit-sources",
                    "search/lexical",
                    "search/semantic",
                    "relational",
                    "total",
                },
            )
            for phase in PHASES:
                self.assertIn(phase, run["profiles"])
            self.assertFalse(
                run["profiles"]["noop_source_refresh"]["last_receipt"][
                    "generation_changed"
                ]
            )
            self.assertTrue(
                run["profiles"]["delete_source_refresh"]["receipts"][0][
                    "generation_changed"
                ]
            )
            initial = run["profiles"]["status"]["initial"]
            self.assertEqual(initial["lexical"]["status"], "ready")
            self.assertEqual(initial["catalog"]["status"], "ready")
            self.assertEqual(initial["semantic"]["status"], "disabled")
            self.assertEqual(initial["relational"]["status"], "ready")
            self.assertEqual(initial["prior_epoch"]["status"], "preserved")


if __name__ == "__main__":
    unittest.main()
