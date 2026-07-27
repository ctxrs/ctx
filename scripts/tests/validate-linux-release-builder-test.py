#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import unittest


if len(sys.argv) != 2:
    raise SystemExit(
        "usage: validate-linux-release-builder-test.py VALIDATOR"
    )
VALIDATOR = Path(sys.argv.pop(1)).resolve()
SPEC = importlib.util.spec_from_file_location(
    "validate_linux_release_builder", VALIDATOR
)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load Linux release-builder validator")
validator = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(validator)


def valid_observation(
    target: str = "x86_64-unknown-linux-gnu",
) -> dict[str, str]:
    sysroot = f"/opt/rustup/toolchains/1.97.1-{target}"
    return {
        "cargo_version": "cargo 1.97.1 (c980f4866 2026-06-30)",
        "glibc": "glibc 2.35",
        "os_id": "ubuntu",
        "os_version": "22.04",
        "rust_commit": "8bab26f4f68e0e26f0bb7960be334d5b520ea452",
        "rust_host": target,
        "rust_release": "1.97.1",
        "rust_sysroot": sysroot,
        "rust_target_libdir": f"{sysroot}/lib/rustlib/{target}/lib",
        "target": target,
    }


class LinuxReleaseBuilderValidatorTest(unittest.TestCase):
    def test_accepts_exact_release_builder(self) -> None:
        validator.validate(valid_observation())

    def test_accepts_exact_arm64_release_builder(self) -> None:
        validator.validate(valid_observation("aarch64-unknown-linux-gnu"))

    def assert_rejected(self, field: str, value: str, message: str) -> None:
        observed = valid_observation()
        observed[field] = value
        with self.assertRaisesRegex(validator.ContractError, message):
            validator.validate(observed)

    def test_rejects_newer_glibc(self) -> None:
        self.assert_rejected("glibc", "glibc 2.36", "GNU libc")

    def test_rejects_newer_ubuntu(self) -> None:
        self.assert_rejected("os_version", "24.04", "Ubuntu version")

    def test_rejects_newer_rust(self) -> None:
        self.assert_rejected("rust_release", "1.98.0", "rustc release")

    def test_rejects_wrong_rust_commit(self) -> None:
        self.assert_rejected("rust_commit", "0" * 40, "rustc commit")

    def test_rejects_wrong_rust_host(self) -> None:
        self.assert_rejected(
            "rust_host", "aarch64-unknown-linux-gnu", "rustc host"
        )

    def test_rejects_newer_cargo(self) -> None:
        self.assert_rejected(
            "cargo_version",
            "cargo 1.98.0 (fixture 2026-07-16)",
            "cargo 1.97.1",
        )

    def test_rejects_wrong_sysroot(self) -> None:
        self.assert_rejected(
            "rust_sysroot", "/tmp/forged-sysroot", "Rust sysroot"
        )

    def test_rejects_wrong_target_libdir(self) -> None:
        self.assert_rejected(
            "rust_target_libdir",
            "/tmp/forged-target-libdir",
            "Rust target libdir",
        )


if __name__ == "__main__":
    unittest.main()
