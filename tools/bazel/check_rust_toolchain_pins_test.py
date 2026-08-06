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
    validate_windows_cargo_runfiles_patch_text,
    validate_windows_gnu_dlltool_patch_text,
)


ROOT = Path(__file__).resolve().parents[2]
MODULE_TEXT = (ROOT / "MODULE.bazel").read_text(encoding="utf-8")
MATRIX_TEXT = (ROOT / "contracts/release-targets-v1.json").read_text(
    encoding="utf-8"
)
ARM_ARCHIVE = "rustc-1.97.1-aarch64-unknown-linux-gnu.tar.xz"
ARM_CHECKSUM = "b344b81f0cd4c2246c7da8b197fe7a339d7dd02bb15cb69b2524115d9c75224c"
FREEBSD_CRATE_UNIVERSE_LINE = '        "x86_64-unknown-freebsd",\n'
WINDOWS_GNU_PATCH_LINE = (
    '        "//tools/bazel/patches:rules-rust-windows-gnu-dlltool-path.patch",\n'
)
WINDOWS_CARGO_RUNFILES_PATCH_LINE = (
    '        "//tools/bazel/patches:rules-rust-windows-cargo-runfiles.patch",\n'
)
WINDOWS_GNU_PATCH = (
    ROOT / "tools/bazel/patches/rules-rust-windows-gnu-dlltool-path.patch"
).read_text(encoding="utf-8")
WINDOWS_CARGO_RUNFILES_PATCH = (
    ROOT / "tools/bazel/patches/rules-rust-windows-cargo-runfiles.patch"
).read_text(encoding="utf-8")


class RustToolchainPinContractTest(unittest.TestCase):
    def test_repository_contract_is_complete(self) -> None:
        validate_module_text(MODULE_TEXT)
        validate_release_matrix_text(MATRIX_TEXT)
        validate_windows_cargo_runfiles_patch_text(WINDOWS_CARGO_RUNFILES_PATCH)
        validate_windows_gnu_dlltool_patch_text(WINDOWS_GNU_PATCH)

    def test_missing_windows_gnu_override_patch_is_rejected(self) -> None:
        mutated = MODULE_TEXT.replace(WINDOWS_GNU_PATCH_LINE, "", 1)
        self.assertNotEqual(mutated, MODULE_TEXT)
        with self.assertRaisesRegex(PinContractError, "incomplete release patches"):
            validate_module_text(mutated)

    def test_incomplete_windows_gnu_patch_is_rejected(self) -> None:
        mutated = WINDOWS_GNU_PATCH.replace(
            '+        tool_dir = paths.dirname(linker)\n',
            "",
            1,
        )
        self.assertNotEqual(mutated, WINDOWS_GNU_PATCH)
        with self.assertRaisesRegex(PinContractError, "dlltool discovery patch is missing"):
            validate_windows_gnu_dlltool_patch_text(mutated)

    def test_missing_windows_cargo_runfiles_patch_is_rejected(self) -> None:
        mutated = MODULE_TEXT.replace(WINDOWS_CARGO_RUNFILES_PATCH_LINE, "", 1)
        self.assertNotEqual(mutated, MODULE_TEXT)
        with self.assertRaisesRegex(PinContractError, "incomplete release patches"):
            validate_module_text(mutated)

    def test_incomplete_windows_cargo_runfiles_patch_is_rejected(self) -> None:
        mutated = WINDOWS_CARGO_RUNFILES_PATCH.replace(
            "+            if self\n",
            "+            if !self\n",
            1,
        )
        self.assertNotEqual(mutated, WINDOWS_CARGO_RUNFILES_PATCH)
        with self.assertRaisesRegex(PinContractError, "retention patch is missing"):
            validate_windows_cargo_runfiles_patch_text(mutated)

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

    def test_missing_freebsd_crate_universe_target_is_rejected(self) -> None:
        mutated = MODULE_TEXT.replace(FREEBSD_CRATE_UNIVERSE_LINE, "", 1)
        self.assertNotEqual(mutated, MODULE_TEXT)
        with self.assertRaisesRegex(
            PinContractError,
            "crate_universe is missing release host triple x86_64-unknown-freebsd",
        ):
            validate_module_text(mutated)


if __name__ == "__main__":
    unittest.main()
