#!/usr/bin/env python3
"""Mutation tests for the pinned FreeBSD ORT shutdown-order repair."""

from __future__ import annotations

from pathlib import Path
import unittest

from check_freebsd_ort_exit_contract import (
    ContractError,
    ORT_COMMIT,
    validate_cargo_text,
    validate_module_text,
    validate_patch_bytes,
)


ROOT = Path(__file__).resolve().parents[2]
MODULE_TEXT = (ROOT / "MODULE.bazel").read_text(encoding="utf-8")
CARGO_TEXT = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
PATCH_BYTES = (
    ROOT / "tools/bazel/patches/ort-freebsd-release-env-order.patch"
).read_bytes()
class FreeBsdOrtExitContractTest(unittest.TestCase):
    def test_repository_contract_is_complete(self) -> None:
        validate_module_text(MODULE_TEXT)
        validate_cargo_text(CARGO_TEXT)
        validate_patch_bytes(PATCH_BYTES)

    def test_missing_bazel_patch_is_rejected(self) -> None:
        mutated = MODULE_TEXT.replace(
            '    patches = ["//tools/bazel/patches:ort-freebsd-release-env-order.patch"],\n',
            "",
        )
        self.assertNotEqual(mutated, MODULE_TEXT)
        with self.assertRaisesRegex(ContractError, "annotation must be exactly"):
            validate_module_text(mutated)

    def test_unreviewed_ort_pin_is_rejected(self) -> None:
        mutated = CARGO_TEXT.replace(ORT_COMMIT, "0" * 40)
        self.assertNotEqual(mutated, CARGO_TEXT)
        with self.assertRaisesRegex(ContractError, "Cargo pin must be exactly"):
            validate_cargo_text(mutated)

    def test_patch_mutation_is_rejected(self) -> None:
        mutated = PATCH_BYTES.replace(b"target_os = \"freebsd\"", b"target_os = \"openbsd\"", 1)
        self.assertNotEqual(mutated, PATCH_BYTES)
        with self.assertRaisesRegex(ContractError, "patch digest mismatch"):
            validate_patch_bytes(mutated)

if __name__ == "__main__":
    unittest.main()
