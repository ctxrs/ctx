#!/usr/bin/env python3
"""Contract tests for public CLI Bazel release target routing."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "scripts" / "public-cli-release-targets.py"
MATRIX = ROOT / "contracts" / "release-targets-v1.json"


def load_helper():
    spec = importlib.util.spec_from_file_location("public_cli_release_targets", HELPER)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


helper = load_helper()


class PublicCliReleaseTargetsTest(unittest.TestCase):
    def test_exact_six_target_names_and_raw_construction_names(self) -> None:
        value = helper.load_matrix(MATRIX)
        expected = {
            "freebsd-x64": ("ctx-freebsd-x64", "ctx-freebsd-x64"),
            "linux-arm64": ("ctx-linux-aarch64", "ctx-linux-aarch64"),
            "linux-x64": ("ctx", "ctx-linux-x64"),
            "macos-arm64": ("ctx-macos-arm64", "ctx-macos-arm64"),
            "macos-x64": ("ctx-macos-x64", "ctx-macos-x64"),
            "windows-x64": ("ctx.exe", "ctx-windows-x64.exe"),
        }
        actual = {}
        for target_id in sorted(expected):
            target = helper.find_target(value, target_id)
            actual[target_id] = (
                helper.RAW_BINARIES[target_id],
                target["public_artifact"],
            )
        self.assertEqual(actual, expected)

    def test_linux_arm64_adapts_only_the_existing_hook_vocabulary(self) -> None:
        value = helper.load_matrix(MATRIX)
        shell = helper.shell_values(helper.find_target(value, "linux-arm64"))
        self.assertIn("CTX_PUBLIC_TARGET_ID=linux-arm64", shell)
        self.assertIn("CTX_PUBLIC_TARGET_PLATFORM=linux-aarch64", shell)
        self.assertIn("CTX_PUBLIC_TARGET_TRIPLE=aarch64-unknown-linux-gnu", shell)

    def test_unknown_target_fails_closed(self) -> None:
        with self.assertRaisesRegex(helper.ContractError, "unsupported release target"):
            helper.find_target(helper.load_matrix(MATRIX), "linux-riscv64")

    def test_matrix_mutation_is_rejected_by_authoritative_checker(self) -> None:
        value = json.loads(MATRIX.read_text(encoding="utf-8"))
        value["targets"][0]["public_artifact"] = "ctx-wrong"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release-targets-v1.json"
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaisesRegex(helper.ContractError, "unexpected release contract"):
                helper.load_matrix(path)


if __name__ == "__main__":
    unittest.main()
