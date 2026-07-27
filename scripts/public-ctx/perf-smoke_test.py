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
# This is the embedded Python body from base 8f5cf123. Update it only alongside
# an intentional harness behavior or artifact-schema change.
FROZEN_PYTHON_SOURCE_SHA256 = (
    "70aafda14c48d6d8b1d2eccae6ad193f90e87e822b53fff03da0b0257d214689"
)
PHASES = (
    "initial_import",
    "noop_incremental_import",
    "append_incremental_import",
    "replacement_import",
)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_embedded_python() -> dict[str, object]:
    python_source = subprocess.run(
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
    definitions = python_source.rsplit("\ntry:\n    raise SystemExit(main())", 1)[0]
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
    wal: float = 10.0,
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
                "wal_high_water_bytes": resource(wal),
                "wal_growth_high_water_bytes": resource(wal),
            },
        }
    profiles["concurrent_refresh_off_search"] = {
        "query": {"timings": {"p95_ms": wall}}
    }
    return {
        "label": role,
        "role": role,
        "profiles": profiles,
    }


MOCK_CTX = r"""
#!/usr/bin/env python3
import json
import os
import sys
import time
from pathlib import Path

ROLE = __ROLE__
VERSION = "ctx 0.25.0" if ROLE == "baseline-v0.25" else "ctx 0.26.0"


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


args = sys.argv[1:]
if args == ["--version"]:
    print(VERSION)
    raise SystemExit(0)

data_root = Path(os.environ["CTX_DATA_ROOT"])
data_root.mkdir(parents=True, exist_ok=True)
state_path = data_root / "mock-import-state.json"
if ROLE == "baseline-v0.25":
    baseline_rss_pad = bytearray(16 * 1024 * 1024)
    baseline_cpu_pad = sum(index * index for index in range(500_000))
    with (data_root / "baseline-io-pad").open("ab") as handle:
        handle.write(b"x" * (1024 * 1024))
        handle.flush()
        os.fsync(handle.fileno())
    time.sleep(0.25)

if args and args[0] == "import":
    corpus_root = args[args.index("--path") + 1]
    current, source_bytes = corpus_snapshot(corpus_root)
    previous = json.loads(state_path.read_text()) if state_path.exists() else {}
    imported_sessions = 0
    imported_events = 0
    for path, item in current.items():
        old = previous.get(path)
        if old is None:
            imported_sessions += 1
            imported_events += item["events"]
        elif old["session_id"] != item["session_id"]:
            imported_sessions += 1
            imported_events += item["events"]
        elif item["lines"] > old["lines"]:
            if ROLE == "baseline-v0.25":
                imported_sessions += 1
            imported_events += item["lines"] - old["lines"]
    if "--resume" in args:
        time.sleep(0.08)
    state_path.write_text(json.dumps(current, sort_keys=True), encoding="utf-8")
    emit({
        "totals": {
            "failed": 0,
            "failed_sources": 0,
            "imported_edges": 0,
            "imported_events": imported_events,
            "imported_sessions": imported_sessions,
            "skipped": 0,
            "source_bytes": source_bytes,
            "source_files": len(current),
        }
    })
elif args and args[0] == "status":
    emit({
        "initialized": True,
        "indexed_items": 1,
        "indexed_catalog_sessions": 1,
        "database_path": str(data_root / "work.sqlite"),
    })
elif args and args[0] == "search":
    emit({
        "freshness": "current",
        "results": [{
            "ctx_session_id": "mock-session",
            "ctx_event_id": "mock-event",
            "provider": "codex",
            "result_scope": "event",
            "source_exists": True,
        }],
    })
elif args and args[0] == "show":
    emit({"id": "mock-session", "events": []})
else:
    print(f"unsupported mock ctx args: {args}", file=sys.stderr)
    raise SystemExit(2)
"""


class PerfSmokePolicyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.module = load_embedded_python()

    def test_helpers_are_shipped_syntax_clean_bounded_and_source_exact(self) -> None:
        self.assertTrue(SCRIPT.is_file())
        for helper in HELPERS:
            self.assertTrue(helper.is_file(), helper)
        subprocess.run(
            ["bash", "-n", str(SCRIPT), *(str(helper) for helper in HELPERS)],
            check=True,
        )
        for path in (SCRIPT, *HELPERS):
            line_count = len(path.read_text(encoding="utf-8").splitlines())
            self.assertLessEqual(line_count, 1000, f"{path}: {line_count} lines")

        python_source = subprocess.run(
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
        self.assertEqual(
            hashlib.sha256(python_source.encode("utf-8")).hexdigest(),
            FROZEN_PYTHON_SOURCE_SHA256,
        )

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
        self.assertTrue(
            invalid_result.stderr.startswith("usage: scripts/public-ctx/perf-smoke.sh")
        )

    def test_append_oracles_are_version_specific(self) -> None:
        append_expectation = self.module["append_expectation"]
        baseline = append_expectation("baseline-v0.25", 5)
        candidate = append_expectation("candidate", 5)
        self.assertEqual(
            (baseline["imported_sessions"], baseline["imported_events"]),
            (5, 5),
        )
        self.assertEqual(
            (candidate["imported_sessions"], candidate["imported_events"]),
            (0, 5),
        )
        self.assertNotEqual(baseline["shape"], candidate["shape"])

    def test_exact_baseline_identity_and_candidate_identity_are_enforced(self) -> None:
        effective_role = self.module["effective_role"]
        harness_error = self.module["HarnessError"]
        self.assertEqual(
            effective_role("baseline-v0.25", "ctx 0.25.0"),
            "baseline-v0.25",
        )
        with self.assertRaises(harness_error):
            effective_role("baseline-v0.25", "ctx 0.25.1")
        with self.assertRaises(harness_error):
            effective_role("candidate", "ctx 0.25.0")

    def test_exact_hash_values_verify_without_executing_binaries(self) -> None:
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
                        (baseline, "v0.25", "baseline-v0.25"),
                        (candidate, "candidate", "candidate"),
                    ],
                )
                single_receipt = preflight(
                    "single",
                    True,
                    [(candidate, "diagnostic", "single")],
                )
        finally:
            for name, value in original.items():
                if value is None:
                    os.environ.pop(name, None)
                else:
                    os.environ[name] = value

        self.assertEqual(receipt["status"], "verified")
        self.assertTrue(receipt["required"])
        for binding in receipt["bindings"]:
            self.assertTrue(binding["matched"])
            self.assertEqual(
                binding["expected_sha256"],
                binding["observed_sha256"],
            )
        self.assertEqual(single_receipt["status"], "observed-only")
        self.assertFalse(single_receipt["required"])
        self.assertIsNone(single_receipt["bindings"][0]["expected_sha256"])

    def test_enforced_comparison_requires_both_orders(self) -> None:
        selected = self.module["selected_comparison_orders"]
        harness_error = self.module["HarnessError"]
        self.assertEqual(
            selected("both", True),
            ["baseline-first", "candidate-first"],
        )
        self.assertEqual(selected("head-first", False), ["candidate-first"])
        with self.assertRaises(harness_error):
            selected("baseline-first", True)

    def test_relative_and_absolute_policy_boundaries_are_machine_readable(self) -> None:
        report = self.module["comparison_report"](
            "baseline-first",
            fake_run("baseline-v0.25"),
            fake_run("candidate"),
            10.0,
            1024.0,
            64.0,
        )
        checks = {check["name"]: check for check in report["checks"]}
        self.assertEqual(report["status"], "passed")
        self.assertEqual(checks["initial_import_wall_parity"]["max_ratio"], 1.0)
        self.assertEqual(
            checks["initial_import_device_write_parity"]["max_ratio"],
            1.0,
        )
        self.assertEqual(
            checks["initial_import_device_total_io_amplification"]["comparator"],
            "<",
        )
        self.assertEqual(
            checks["initial_import_device_total_io_amplification"]["max_ratio"],
            1.73,
        )
        self.assertIn("candidate", checks["initial_import_cpu_total_relative"])
        self.assertEqual(
            checks["initial_import_peak_rss_max"]["threshold"],
            1024.0,
        )
        self.assertEqual(
            checks["initial_import_wal_high_water_max"]["threshold"],
            64.0,
        )
        self.assertTrue(
            all(check["comparison_order"] == "baseline-first" for check in report["checks"])
        )

    def test_hard_ratio_boundaries_fail_at_the_required_edges(self) -> None:
        ratio_check = self.module["ratio_check"]
        self.assertTrue(
            ratio_check("wall", 100, 100, 1.0, "ms", "wall", inclusive=True)["passed"]
        )
        self.assertFalse(
            ratio_check("wall", 100, 101, 1.0, "ms", "wall", inclusive=True)["passed"]
        )
        self.assertFalse(
            ratio_check("io", 100, 173, 1.73, "bytes", "io", inclusive=False)["passed"]
        )
        self.assertTrue(
            ratio_check("io", 100, 172, 1.73, "bytes", "io", inclusive=False)["passed"]
        )
        self.assertTrue(
            ratio_check("zero", 0, 0, 1.0, "bytes", "zero", inclusive=True)["passed"]
        )
        self.assertFalse(
            ratio_check("zero", 0, 1, 1.0, "bytes", "zero", inclusive=True)["passed"]
        )

    def test_policy_rejects_cpu_rss_io_and_absolute_amplification(self) -> None:
        report = self.module["comparison_report"](
            "candidate-first",
            fake_run("baseline-v0.25"),
            fake_run(
                "candidate",
                wall=101,
                cpu=111,
                rss=2048,
                read=173,
                write=101,
                total_io=346,
                wal=65,
            ),
            10.0,
            1024.0,
            64.0,
        )
        checks = {check["name"]: check for check in report["checks"]}
        for name in (
            "initial_import_wall_parity",
            "initial_import_cpu_total_relative",
            "initial_import_peak_rss_relative",
            "initial_import_device_read_amplification",
            "initial_import_device_write_parity",
            "initial_import_device_total_io_amplification",
            "initial_import_peak_rss_max",
            "initial_import_wal_high_water_max",
        ):
            self.assertFalse(checks[name]["passed"], name)


class PerfSmokeEndToEndTests(unittest.TestCase):
    def write_mock(self, root: Path, role: str) -> Path:
        path = root / f"ctx-{role}"
        source = textwrap.dedent(MOCK_CTX).replace("__ROLE__", repr(role)).lstrip()
        path.write_text(source, encoding="utf-8")
        path.chmod(0o755)
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
        path.chmod(0o755)
        return path

    def test_enforced_hash_failures_precede_binary_and_corpus_execution(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ctx-perf-hash-fail-") as temp:
            root = Path(temp)
            marker = root / "binary-executed"
            baseline = self.write_execution_sentinel(root, "baseline-v0.25", marker)
            candidate = self.write_execution_sentinel(root, "candidate", marker)
            baseline_hash = sha256(baseline)
            candidate_hash = sha256(candidate)
            cases = [
                ("missing-baseline", None, candidate_hash, "BASELINE_SHA256 is required"),
                ("missing-candidate", baseline_hash, None, "CANDIDATE_SHA256 is required"),
                (
                    "malformed-baseline",
                    baseline_hash.upper(),
                    candidate_hash,
                    "BASELINE_SHA256 must be exactly",
                ),
                (
                    "malformed-candidate",
                    baseline_hash,
                    "g" * 64,
                    "CANDIDATE_SHA256 must be exactly",
                ),
                (
                    "mismatch-baseline",
                    "0" * 64,
                    candidate_hash,
                    "BASELINE_SHA256 does not match",
                ),
                (
                    "mismatch-candidate",
                    baseline_hash,
                    "0" * 64,
                    "CANDIDATE_SHA256 does not match",
                ),
            ]
            for name, expected_baseline, expected_candidate, error_fragment in cases:
                with self.subTest(name=name):
                    marker.unlink(missing_ok=True)
                    work_dir = root / f"{name}-work"
                    artifact = root / f"{name}.json"
                    env = os.environ.copy()
                    env.pop("CTX_PERF_SMOKE_BASELINE_SHA256", None)
                    env.pop("CTX_PERF_SMOKE_CANDIDATE_SHA256", None)
                    env.update(
                        {
                            "CTX_PUBLIC_CTX_REPO": str(REPO_ROOT),
                            "CTX_PERF_SMOKE_BASELINE_BIN": str(baseline),
                            "CTX_PERF_SMOKE_CANDIDATE_BIN": str(candidate),
                            "CTX_PERF_SMOKE_ENFORCE": "1",
                            "CTX_PERF_SMOKE_COMPARISON_ORDER": "both",
                            "CTX_PERF_SMOKE_WORK_DIR": str(work_dir),
                            "CTX_PERF_SMOKE_ARTIFACT": str(artifact),
                        }
                    )
                    if expected_baseline is not None:
                        env["CTX_PERF_SMOKE_BASELINE_SHA256"] = expected_baseline
                    if expected_candidate is not None:
                        env["CTX_PERF_SMOKE_CANDIDATE_SHA256"] = expected_candidate
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
                    self.assertIn(error_fragment, completed.stderr)
                    self.assertFalse(marker.exists(), "a binary executed before hash rejection")
                    self.assertFalse(work_dir.exists(), "corpus work began before hash rejection")
                    self.assertFalse(artifact.exists(), "a failure emitted a success receipt")

    def test_mock_cli_exercises_both_orders_and_both_append_shapes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="ctx-perf-smoke-test-") as temp:
            root = Path(temp)
            baseline = self.write_mock(root, "baseline-v0.25")
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
                    "CTX_PERF_SMOKE_BASELINE_LABEL": "v0.25-test",
                    "CTX_PERF_SMOKE_CANDIDATE_LABEL": "candidate-test",
                    "CTX_PERF_SMOKE_SESSIONS": "2",
                    "CTX_PERF_SMOKE_LARGE_SESSION_EVENTS": "65",
                    "CTX_PERF_SMOKE_INITIAL_REPEATS": "1",
                    "CTX_PERF_SMOKE_REPEATS": "1",
                    "CTX_PERF_SMOKE_CHANGED_FILES": "1",
                    "CTX_PERF_SMOKE_CONCURRENT_QUERIES": "1",
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

        self.assertEqual(artifact["schema_version"], 4)
        self.assertTrue(artifact["enforced"])
        self.assertEqual(artifact["status"], "passed")
        self.assertEqual(artifact["binary_hash_binding"]["status"], "verified")
        for binding in artifact["binary_hash_binding"]["bindings"]:
            self.assertTrue(binding["matched"])
            self.assertEqual(
                binding["expected_sha256"],
                binding["observed_sha256"],
            )
        for run in artifact["runs"]:
            self.assertTrue(run["binary"]["sha256_matched"])
            self.assertEqual(
                run["binary"]["expected_sha256"],
                run["binary"]["sha256"],
            )
        self.assertEqual(
            artifact["configuration"]["comparison_orders"],
            ["baseline-first", "candidate-first"],
        )
        orders = artifact["execution_orders"]
        self.assertEqual(
            [order["execution_sequence"] for order in orders],
            [
                ["baseline-v0.25", "candidate"],
                ["candidate", "baseline-v0.25"],
            ],
        )
        self.assertEqual(len(artifact["runs"]), 4)
        runs_by_id = {run["run_id"]: run for run in artifact["runs"]}
        for order in orders:
            by_role = {
                runs_by_id[run_id]["role"]: runs_by_id[run_id]
                for run_id in order["run_ids"]
            }
            baseline_summary = by_role["baseline-v0.25"]["profiles"][
                "append_incremental_import"
            ]["sample_summaries"][0]
            candidate_summary = by_role["candidate"]["profiles"][
                "append_incremental_import"
            ]["sample_summaries"][0]
            self.assertEqual(baseline_summary["imported_sessions"], 1)
            self.assertEqual(candidate_summary["imported_sessions"], 0)
            self.assertEqual(baseline_summary["imported_events"], 1)
            self.assertEqual(candidate_summary["imported_events"], 1)

        comparison = artifact["comparison"]
        self.assertEqual(
            comparison["required_orders"],
            ["baseline-first", "candidate-first"],
        )
        self.assertEqual(
            comparison["hard_policy"]["wall_ratio_max_inclusive"],
            1.0,
        )
        self.assertEqual(
            comparison["hard_policy"]["device_write_ratio_max_inclusive"],
            1.0,
        )
        self.assertEqual(
            comparison["hard_policy"]["io_amplification_ratio_max_exclusive"],
            1.73,
        )
        self.assertEqual(
            comparison["hard_policy"]["cpu_rss_ratio_max_inclusive"],
            1.10,
        )
        self.assertEqual(
            {check["comparison_order"] for check in comparison["checks"]},
            {"baseline-first", "candidate-first"},
        )


if __name__ == "__main__":
    unittest.main()
