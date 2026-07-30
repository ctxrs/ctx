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
            [target["id"] for target in value["targets"]],
            list(matrix.SUPPORTED_TARGET_IDS),
        )
        self.assertNotIn("windows-arm64", matrix.SUPPORTED_TARGET_IDS)
        self.assertNotIn("freebsd-arm64", matrix.SUPPORTED_TARGET_IDS)
        windows = next(
            target for target in value["targets"] if target["id"] == "windows-x64"
        )
        self.assertEqual(windows["platform_signature"], "unsigned")
        self.assertEqual(windows["archive"], "zip")
        self.assertEqual(windows["runtime_authority"], "native-windows-x86_64")
        self.assertEqual(
            windows["public_construction_label"],
            "//:ctx_release_windows_x64",
        )
        self.assertEqual(
            windows["public_construction_authority"],
            "bazel-release-route-v1",
        )
        self.assertEqual(
            windows["bazel_platform"],
            "//tools/bazel/platforms:release_windows_x64_gnu",
        )
        self.assertIsNone(windows["linux_build"])
        linux = next(
            target for target in value["targets"] if target["id"] == "linux-x64"
        )
        self.assertEqual(linux["linux_build"]["glibc_max"], "2.35")
        self.assertEqual(linux["linux_build"]["rust_toolchain"], "1.97.1")
        self.assertEqual(
            linux["linux_build"]["rust_sysroot"],
            "/opt/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu",
        )

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
        value["targets"].append(
            dict(
                value["targets"][-1],
                id="windows-arm64",
                public_construction_label="//:ctx_release_windows_arm64",
            )
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "exact sorted Day One matrix"):
                matrix.load_and_validate(path)

    def test_unsafe_artifact_or_signing_shape_is_rejected(self) -> None:
        value = matrix.load_and_validate()
        value["targets"][0]["helper_artifact"] = "../ctx-pro"
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
            with self.assertRaisesRegex(ValueError, "unsupported policy value or path"):
                matrix.load_and_validate(path)

    def test_linux_build_baseline_drift_is_rejected(self) -> None:
        value = matrix.load_and_validate()
        linux = next(
            target for target in value["targets"] if target["id"] == "linux-x64"
        )
        linux["linux_build"]["glibc_max"] = "latest"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "malformed immutable pins"):
                matrix.load_and_validate(path)

    def test_generated_bazel_consumer_fails_closed_on_drift(self) -> None:
        value = matrix.load_and_validate()
        source = f"{matrix.generated_bazel_consumer(value)}\n"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release_inventory.bzl"
            path.write_text(source, encoding="utf-8")
            matrix.validate_bazel_consumer(path, value)
            path.write_text(
                source.replace("release_linux_x64", "release_linux_wrong"),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "consumer is stale"):
                matrix.validate_bazel_consumer(path, value)


if __name__ == "__main__":
    unittest.main()
