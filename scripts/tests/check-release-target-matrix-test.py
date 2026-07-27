#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "check-release-target-matrix.py"
SPEC = importlib.util.spec_from_file_location("release_target_matrix", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load release target matrix validator")
matrix = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(matrix)


class ReleaseTargetMatrixTest(unittest.TestCase):
    def test_repository_matrix_is_exact(self) -> None:
        value = matrix.load_and_validate()
        self.assertEqual(
            [target["id"] for target in value["targets"]], sorted(matrix.EXPECTED)
        )
        self.assertNotIn("windows-arm64", matrix.EXPECTED)
        self.assertNotIn("freebsd-arm64", matrix.EXPECTED)
        windows = next(
            target for target in value["targets"] if target["id"] == "windows-x64"
        )
        self.assertEqual(windows["platform_signature"], "unsigned")
        self.assertEqual(windows["archive"], "zip")
        self.assertEqual(windows["runtime_authority"], "native-windows-x86_64")

    def test_diagnostic_runner_cannot_be_authoritative(self) -> None:
        value = matrix.load_and_validate()
        value["targets"][0]["diagnostic_authorities"] = [
            value["targets"][0]["runtime_authority"]
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "also diagnostic"):
                matrix.load_and_validate(path)

    def test_extra_target_is_rejected(self) -> None:
        value = matrix.load_and_validate()
        value["targets"].append(dict(value["targets"][-1], id="windows-arm64"))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "unexpected release contract"):
                matrix.load_and_validate(path)

    def test_artifact_or_signing_drift_is_rejected(self) -> None:
        value = matrix.load_and_validate()
        value["targets"][0]["helper_artifact"] = "ctx-pro"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "unexpected release contract"):
                matrix.load_and_validate(path)

        value = matrix.load_and_validate()
        value["targets"][-1]["platform_signature"] = "authenticode"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "unsupported platform signature"):
                matrix.load_and_validate(path)

    def test_unknown_platform_signature_policy_is_rejected(self) -> None:
        value = matrix.load_and_validate()
        value["targets"][-1]["platform_signature"] = "not-reviewed"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "unsupported platform signature"):
                matrix.load_and_validate(path)


if __name__ == "__main__":
    unittest.main()
