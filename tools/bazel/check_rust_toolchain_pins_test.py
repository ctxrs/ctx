#!/usr/bin/env python3
"""Mutation tests for the native Bazel Rust toolchain checksum contract."""

from __future__ import annotations

import json
from pathlib import Path
import unittest

from check_rust_toolchain_pins import (
    PinContractError,
    validate_module_text,
    validate_release_matrix_text,
)


ROOT = Path(__file__).resolve().parents[2]
MODULE_TEXT = (ROOT / "MODULE.bazel").read_text(encoding="utf-8")
MATRIX_TEXT = (ROOT / "contracts/release-targets-v1.json").read_text(
    encoding="utf-8"
)
ARM_ARCHIVE = "rustc-1.97.1-aarch64-unknown-linux-gnu.tar.xz"
ARM_CHECKSUM = "b344b81f0cd4c2246c7da8b197fe7a339d7dd02bb15cb69b2524115d9c75224c"


class RustToolchainPinContractTest(unittest.TestCase):
    def test_repository_contract_is_complete(self) -> None:
        validate_module_text(MODULE_TEXT)
        validate_release_matrix_text(MATRIX_TEXT)

    def test_missing_arm_checksum_is_rejected(self) -> None:
        line = f'        "{ARM_ARCHIVE}": "{ARM_CHECKSUM}",\n'
        mutated = MODULE_TEXT.replace(line, "")
        self.assertNotEqual(mutated, MODULE_TEXT)
        with self.assertRaisesRegex(PinContractError, "missing checksum"):
            validate_module_text(mutated)

    def test_wrong_arm_checksum_is_rejected(self) -> None:
        mutated = MODULE_TEXT.replace(ARM_CHECKSUM, "0" * 64)
        self.assertNotEqual(mutated, MODULE_TEXT)
        with self.assertRaisesRegex(PinContractError, "wrong checksum"):
            validate_module_text(mutated)

    def test_new_release_platform_without_mapping_is_rejected(self) -> None:
        matrix = json.loads(MATRIX_TEXT)
        matrix["targets"].append({"os": "plan9", "arch": "x86_64"})
        with self.assertRaisesRegex(PinContractError, "no native Bazel host mapping"):
            validate_release_matrix_text(json.dumps(matrix))


if __name__ == "__main__":
    unittest.main()
