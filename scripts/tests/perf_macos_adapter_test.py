#!/usr/bin/env python3
"""Hermetic parser and contract tests for the native macOS perf adapter."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
ADAPTER_PATH = ROOT / "scripts/public-ctx/perf-macos-measure.py"
CONTRACT_PATH = ROOT / "scripts/public-ctx/perf-macos-contract.md"
FIXTURES = ROOT / "scripts/tests/perf_macos_fixtures"
SPEC = importlib.util.spec_from_file_location("perf_macos_measure", ADAPTER_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load macOS perf adapter")
ADAPTER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ADAPTER
SPEC.loader.exec_module(ADAPTER)


class PerfMacosParserTests(unittest.TestCase):
    def fixture(self, name: str) -> bytes:
        return (FIXTURES / name).read_bytes()

    def test_native_fixture_parses_normative_metrics_and_splits_stderr(self) -> None:
        parsed = ADAPTER.parse_time_l(self.fixture("success.time.txt"))
        self.assertEqual(parsed.command_stderr, b"ctx: fixture warning\n")
        self.assertTrue(parsed.timing_output.startswith(b"real 12.34\n"))
        self.assertEqual(
            parsed.metrics(),
            {
                "block_input_operations": 17,
                "block_output_operations": 29,
                "maximum_resident_set_size_bytes": 1_056_292_864,
                "system_cpu_seconds": 1.25,
                "user_cpu_seconds": 9.87,
                "wall_time_seconds": 12.34,
            },
        )
        self.assertFalse(parsed.command_terminated_abnormally)

    def test_abnormal_marker_is_native_status_not_command_stderr(self) -> None:
        parsed = ADAPTER.parse_time_l(self.fixture("signaled.time.txt"))
        self.assertEqual(parsed.command_stderr, b"ctx: began work\n")
        self.assertTrue(
            parsed.timing_output.startswith(b"Command terminated abnormally.\n")
        )
        receipt = ADAPTER.success_record(["ctx", "import"], parsed, 1)
        self.assertTrue(receipt["valid"])
        self.assertEqual(
            receipt["status"],
            {
                "exit_code": None,
                "kind": "signaled",
                "signal": None,
                "signal_unavailable_reason": "not_reported_by_darwin_time_l",
                "time_exit_code": 1,
            },
        )

    def test_signal_number_is_recorded_when_process_status_exposes_it(self) -> None:
        parsed = ADAPTER.parse_time_l(self.fixture("signaled.time.txt"))
        receipt = ADAPTER.success_record(["ctx"], parsed, -15)
        self.assertEqual(
            receipt["status"],
            {
                "exit_code": None,
                "kind": "signaled",
                "signal": 15,
                "signal_unavailable_reason": None,
                "time_exit_code": None,
            },
        )
        self.assertEqual(ADAPTER._return_code(-15, parsed), 143)

    def test_missing_normative_io_metric_fails_closed(self) -> None:
        with self.assertRaises(ADAPTER.MeasurementError) as raised:
            ADAPTER.parse_time_l(self.fixture("missing-io.time.txt"))
        self.assertEqual(raised.exception.code, "missing_required_metric")
        self.assertEqual(
            raised.exception.fields,
            ("block_output_operations",),
        )

    def test_zero_io_operation_counts_are_observed_values(self) -> None:
        raw = (
            self.fixture("success.time.txt")
            .replace(b"17  block input operations", b" 0  block input operations")
            .replace(b"29  block output operations", b" 0  block output operations")
        )
        parsed = ADAPTER.parse_time_l(raw)
        self.assertEqual(parsed.block_input_operations, 0)
        self.assertEqual(parsed.block_output_operations, 0)

    def test_duplicate_normative_metric_fails_closed(self) -> None:
        raw = self.fixture("success.time.txt") + b"1  block input operations\n"
        with self.assertRaises(ADAPTER.MeasurementError) as raised:
            ADAPTER.parse_time_l(raw)
        self.assertEqual(raised.exception.code, "duplicate_required_metric")
        self.assertEqual(raised.exception.fields, ("block_input_operations",))

    def test_gnu_time_output_is_not_accepted_as_macos(self) -> None:
        raw = (
            b"\tUser time (seconds): 1.00\n"
            b"\tSystem time (seconds): 0.25\n"
            b"\tElapsed (wall clock) time (h:mm:ss or m:ss): 0:01.50\n"
            b"\tMaximum resident set size (kbytes): 1234\n"
            b"\tFile system inputs: 4\n"
            b"\tFile system outputs: 9\n"
        )
        with self.assertRaises(ADAPTER.MeasurementError) as raised:
            ADAPTER.parse_time_l(raw)
        self.assertEqual(raised.exception.code, "missing_time_header")

    def test_receipt_json_is_canonical_and_units_are_explicit(self) -> None:
        parsed = ADAPTER.parse_time_l(self.fixture("success.time.txt"))
        receipt = ADAPTER.success_record(["/tmp/ctx", "status", "--json"], parsed, 0)
        first = ADAPTER.canonical_json(receipt)
        second = ADAPTER.canonical_json(receipt)
        self.assertEqual(first, second)
        self.assertTrue(first.endswith(b"\n"))
        self.assertNotIn(b": ", first)
        decoded = json.loads(first)
        self.assertEqual(decoded["schema"], "ctx.perf.macos.time_l.v1")
        self.assertEqual(decoded["schema_version"], 1)
        self.assertEqual(
            decoded["collector"]["io_contract"],
            "darwin_ru_inblock_ru_oublock_operations_v1",
        )
        self.assertEqual(decoded["units"], ADAPTER.UNITS)

    def test_invalid_receipt_omits_partial_metrics_and_units(self) -> None:
        error = ADAPTER.MeasurementError(
            "missing_required_metric",
            "native time output is missing normative metrics",
            fields=("block_output_operations",),
        )
        receipt = ADAPTER.invalid_record(["ctx"], error, time_exit_code=0)
        self.assertFalse(receipt["valid"])
        self.assertNotIn("metrics", receipt)
        self.assertNotIn("units", receipt)
        self.assertEqual(receipt["error"]["fields"], ["block_output_operations"])

    def test_linux_cli_fails_closed_without_running_the_command(self) -> None:
        if sys.platform == "darwin":
            self.skipTest("Linux-only platform guard test")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker = root / "command-ran"
            result = subprocess.run(
                [
                    sys.executable,
                    str(ADAPTER_PATH),
                    "--output",
                    str(root / "measurement.json"),
                    "--stdout",
                    str(root / "command.stdout"),
                    "--stderr",
                    str(root / "command.stderr"),
                    "--time-output",
                    str(root / "time-l.txt"),
                    "--",
                    "/bin/sh",
                    "-c",
                    f"touch {marker}",
                ],
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            self.assertEqual(result.returncode, ADAPTER.ADAPTER_ERROR_EXIT)
            self.assertFalse(marker.exists())
            receipt = json.loads((root / "measurement.json").read_bytes())
            self.assertFalse(receipt["valid"])
            self.assertEqual(receipt["error"]["code"], "unsupported_platform")
            self.assertFalse((root / "command.stdout").exists())
            self.assertFalse((root / "command.stderr").exists())
            self.assertFalse((root / "time-l.txt").exists())

    def test_public_contract_names_every_metric_and_forbids_linux_mapping(self) -> None:
        contract = CONTRACT_PATH.read_text(encoding="utf-8")
        for field in ADAPTER.UNITS:
            self.assertIn(f"`{field}`", contract)
        self.assertIn("darwin_ru_inblock_ru_oublock_operations_v1", contract)
        self.assertIn("They are not byte counts", contract)
        self.assertIn("Linux `/usr/bin/time -v`", contract)
        self.assertIn("return 125", contract)
        self.assertIn("status.signal_unavailable_reason", contract)
        self.assertIn("`perf-smoke` integration", contract)


if __name__ == "__main__":
    unittest.main()
