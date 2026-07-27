#!/usr/bin/env python3
"""Pure validation for observed Linux release-builder properties."""

from __future__ import annotations

import argparse
from collections.abc import Mapping


EXPECTED_OS_ID = "ubuntu"
EXPECTED_OS_VERSION = "22.04"
EXPECTED_GLIBC = "glibc 2.35"
EXPECTED_RUST = "1.97.1"
EXPECTED_RUST_COMMIT = "8bab26f4f68e0e26f0bb7960be334d5b520ea452"
SYSROOT_ROOT = "/opt/rustup/toolchains"
TARGETS = {
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
}
FIELDS = {
    "cargo_version",
    "glibc",
    "os_id",
    "os_version",
    "rust_commit",
    "rust_host",
    "rust_release",
    "rust_sysroot",
    "rust_target_libdir",
    "target",
}


class ContractError(ValueError):
    pass


def validate(observed: Mapping[str, str]) -> None:
    if set(observed) != FIELDS:
        raise ContractError("Linux builder observation has an invalid shape")
    target = observed["target"]
    if target not in TARGETS:
        raise ContractError(f"unsupported Linux release target: {target}")
    expected_sysroot = f"{SYSROOT_ROOT}/{EXPECTED_RUST}-{target}"
    expected_target_libdir = f"{expected_sysroot}/lib/rustlib/{target}/lib"
    expected = {
        "os_id": EXPECTED_OS_ID,
        "os_version": EXPECTED_OS_VERSION,
        "glibc": EXPECTED_GLIBC,
        "rust_release": EXPECTED_RUST,
        "rust_commit": EXPECTED_RUST_COMMIT,
        "rust_host": target,
        "rust_sysroot": expected_sysroot,
        "rust_target_libdir": expected_target_libdir,
    }
    labels = {
        "os_id": "OS ID",
        "os_version": "Ubuntu version",
        "glibc": "GNU libc",
        "rust_release": "rustc release",
        "rust_commit": "rustc commit",
        "rust_host": "rustc host",
        "rust_sysroot": "Rust sysroot",
        "rust_target_libdir": "Rust target libdir",
    }
    for field, value in expected.items():
        if observed[field] != value:
            raise ContractError(
                f"expected {labels[field]} {value}, got {observed[field] or 'missing'}"
            )
    cargo_version = observed["cargo_version"]
    if (
        "\n" in cargo_version
        or not cargo_version.startswith(f"cargo {EXPECTED_RUST} ")
    ):
        raise ContractError(
            f"expected cargo {EXPECTED_RUST}, got {cargo_version or 'missing'}"
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    for field in sorted(FIELDS):
        parser.add_argument(f"--{field.replace('_', '-')}", required=True)
    args = parser.parse_args()
    try:
        validate(vars(args))
    except ContractError as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
