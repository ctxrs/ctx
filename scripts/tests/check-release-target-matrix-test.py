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
        matrix.validate_advisory_policy_coverage(value)
        self.assertEqual(
            [target["id"] for target in value["targets"]],
            list(matrix.SUPPORTED_TARGET_IDS),
        )
        self.assertNotIn("windows-arm64", matrix.SUPPORTED_TARGET_IDS)
        self.assertNotIn("freebsd-x64", matrix.SUPPORTED_TARGET_IDS)
        self.assertNotIn("freebsd-arm64", matrix.SUPPORTED_TARGET_IDS)
        windows = next(
            target for target in value["targets"] if target["id"] == "windows-x64"
        )
        self.assertEqual(windows["platform_signature"], "authenticode")
        self.assertEqual(windows["archive"], "zip")
        self.assertEqual(windows["runtime_authority"], "native-windows-x86_64")
        self.assertEqual(
            windows["public_construction_label"],
            "scripts/release/build-public-candidate-on-linux.sh",
        )
        self.assertEqual(
            windows["public_construction_authority"],
            "linux-cross-cargo-zigbuild-v1",
        )
        self.assertNotIn("bazel_platform", windows)
        self.assertIsNone(windows["linux_build"])
        linux = next(
            target for target in value["targets"] if target["id"] == "linux-x64"
        )
        self.assertEqual(linux["linux_build"], {"glibc_max": "2.35"})
        serialized = json.dumps(value)
        for stale_field in (
            "builder_image",
            "rust_commit",
            "rust_sysroot",
            "rust_toolchain",
            "ubuntu_snapshot",
        ):
            self.assertNotIn(stale_field, serialized)

    def test_advisory_scanner_must_cover_every_release_target(self) -> None:
        value = matrix.load_and_validate()
        policy = json.loads(matrix.ADVISORY_POLICY_PATH.read_text(encoding="utf-8"))
        policy["scanner"]["sha256"] = None
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release-advisory-policy-v1.json"
            path.write_text(json.dumps(policy), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "pinned Linux-x64 input"):
                matrix.validate_advisory_policy_coverage(value, path)

    def test_advisory_scanner_rejects_unexpected_targets(self) -> None:
        value = matrix.load_and_validate()
        policy = json.loads(matrix.ADVISORY_POLICY_PATH.read_text(encoding="utf-8"))
        policy["scanner"]["platform"] = "freebsd-x64"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release-advisory-policy-v1.json"
            path.write_text(json.dumps(policy), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "pinned Linux-x64 input"):
                matrix.validate_advisory_policy_coverage(value, path)

    def test_advisory_scanner_rejects_malformed_digest(self) -> None:
        value = matrix.load_and_validate()
        policy = json.loads(matrix.ADVISORY_POLICY_PATH.read_text(encoding="utf-8"))
        policy["scanner"]["sha256"] = None
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release-advisory-policy-v1.json"
            path.write_text(json.dumps(policy), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "pinned Linux-x64 input"):
                matrix.validate_advisory_policy_coverage(value, path)

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
                public_construction_label="scripts/release/build-public-candidate-on-linux.sh",
            )
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "exact sorted prebuilt matrix"):
                matrix.load_and_validate(path)

    def test_freebsd_cannot_reenter_the_prebuilt_matrix(self) -> None:
        value = matrix.load_and_validate()
        freebsd = dict(
            value["targets"][0],
            id="freebsd-x64",
            os="freebsd",
            public_rust_target="x86_64-unknown-freebsd",
            helper_rust_target="x86_64-unknown-freebsd",
            public_artifact="ctx-freebsd-x64",
            helper_artifact="ctx-pro-freebsd-x64",
            public_construction_label="scripts/release/build-public-candidate-on-linux.sh",
            runtime_authority="native-freebsd-x86_64",
            linux_build=None,
        )
        value["targets"].insert(0, freebsd)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "exact sorted prebuilt matrix"):
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
        value["targets"][-1]["platform_signature"] = "self-signed"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "unsupported policy value or path"):
                matrix.load_and_validate(path)

    def test_linux_abi_baseline_drift_is_rejected(self) -> None:
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

    def test_bazel_platform_cannot_reenter_the_target_shape(self) -> None:
        value = matrix.load_and_validate()
        value["targets"][0]["bazel_platform"] = (
            "//tools/bazel/platforms:release_linux_arm64"
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "missing or unexpected fields"):
                matrix.load_and_validate(path)

    def test_stale_linux_builder_metadata_is_rejected(self) -> None:
        value = matrix.load_and_validate()
        linux = next(
            target for target in value["targets"] if target["id"] == "linux-x64"
        )
        linux["linux_build"]["builder_image"] = "ubuntu:22.04"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "matrix.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "missing or unexpected fields"):
                matrix.load_and_validate(path)

if __name__ == "__main__":
    unittest.main()
