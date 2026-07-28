#!/usr/bin/env python3
"""Validate the single public release-target vocabulary used by ctx and Pro."""

from __future__ import annotations

import json
from pathlib import Path
import sys


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = REPOSITORY_ROOT / "contracts" / "release-targets-v1.json"
EXPECTED = {
    "freebsd-x64": {
        "os": "freebsd",
        "arch": "x86_64",
        "public_rust_target": "x86_64-unknown-freebsd",
        "helper_rust_target": "x86_64-unknown-freebsd",
        "public_artifact": "ctx-freebsd-x64",
        "helper_artifact": "ctx-pro-freebsd-x64",
        "public_construction_authority": "bazel-release-route-v1",
        "public_construction_label": "//:ctx_release_freebsd_x64",
        "archive": "tar.gz",
        "vault": "secret-service",
        "runtime_authority": "native-freebsd-x86_64",
        "diagnostic_authorities": ["freebsd-15.1"],
        "platform_signature": "release-manifest",
        "linux_build": None,
    },
    "linux-arm64": {
        "os": "linux",
        "arch": "aarch64",
        "public_rust_target": "aarch64-unknown-linux-gnu",
        "helper_rust_target": "aarch64-unknown-linux-gnu",
        "public_artifact": "ctx-linux-aarch64",
        "helper_artifact": "ctx-pro-linux-arm64",
        "public_construction_authority": "bazel-release-route-v1",
        "public_construction_label": "//:ctx_release_linux_arm64",
        "archive": "tar.gz",
        "vault": "secret-service",
        "runtime_authority": "native-linux-aarch64",
        "diagnostic_authorities": ["qemu-user"],
        "platform_signature": "release-manifest",
        "linux_build": {
            "builder_image": "docker.io/library/ubuntu:22.04@sha256:"
            "0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982",
            "ubuntu_snapshot": "20260701T000000Z",
            "glibc_max": "2.35",
            "rust_toolchain": "1.97.1",
            "rust_commit": "8bab26f4f68e0e26f0bb7960be334d5b520ea452",
            "rust_sysroot": "/opt/rustup/toolchains/"
            "1.97.1-aarch64-unknown-linux-gnu",
        },
    },
    "linux-x64": {
        "os": "linux",
        "arch": "x86_64",
        "public_rust_target": "x86_64-unknown-linux-gnu",
        "helper_rust_target": "x86_64-unknown-linux-gnu",
        "public_artifact": "ctx-linux-x64",
        "helper_artifact": "ctx-pro-linux-x64",
        "public_construction_authority": "bazel-release-route-v1",
        "public_construction_label": "//:ctx_release_linux_x64",
        "archive": "tar.gz",
        "vault": "secret-service",
        "runtime_authority": "native-linux-x86_64",
        "diagnostic_authorities": [],
        "platform_signature": "release-manifest",
        "linux_build": {
            "builder_image": "docker.io/library/ubuntu:22.04@sha256:"
            "0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982",
            "ubuntu_snapshot": "20260701T000000Z",
            "glibc_max": "2.35",
            "rust_toolchain": "1.97.1",
            "rust_commit": "8bab26f4f68e0e26f0bb7960be334d5b520ea452",
            "rust_sysroot": "/opt/rustup/toolchains/"
            "1.97.1-x86_64-unknown-linux-gnu",
        },
    },
    "macos-arm64": {
        "os": "macos",
        "arch": "aarch64",
        "public_rust_target": "aarch64-apple-darwin",
        "helper_rust_target": "aarch64-apple-darwin",
        "public_artifact": "ctx-macos-arm64",
        "helper_artifact": "ctx-pro-macos-arm64",
        "public_construction_authority": "bazel-release-route-v1",
        "public_construction_label": "//:ctx_release_macos_arm64",
        "archive": "tar.gz",
        "vault": "keychain",
        "runtime_authority": "native-apple-arm64",
        "diagnostic_authorities": [],
        "platform_signature": "developer-id-notarized",
        "linux_build": None,
    },
    "macos-x64": {
        "os": "macos",
        "arch": "x86_64",
        "public_rust_target": "x86_64-apple-darwin",
        "helper_rust_target": "x86_64-apple-darwin",
        "public_artifact": "ctx-macos-x64",
        "helper_artifact": "ctx-pro-macos-x64",
        "public_construction_authority": "bazel-release-route-v1",
        "public_construction_label": "//:ctx_release_macos_x64",
        "archive": "tar.gz",
        "vault": "keychain",
        "runtime_authority": "native-macos-x86_64",
        "diagnostic_authorities": ["rosetta-2", "non-apple-qemu"],
        "platform_signature": "developer-id-notarized",
        "linux_build": None,
    },
    "windows-x64": {
        "os": "windows",
        "arch": "x86_64",
        "public_rust_target": "x86_64-pc-windows-gnu",
        "helper_rust_target": "x86_64-pc-windows-msvc",
        "public_artifact": "ctx-windows-x64.exe",
        "helper_artifact": "ctx-pro-windows-x64.exe",
        "public_construction_authority": "bazel-release-route-v1",
        "public_construction_label": "//:ctx_release_windows_x64",
        "archive": "zip",
        "vault": "credential-manager",
        "runtime_authority": "native-windows-x86_64",
        "diagnostic_authorities": [],
        "platform_signature": "unsigned",
        "linux_build": None,
    },
}
REQUIRED_STRING_FIELDS = {
    "id",
    "os",
    "arch",
    "public_rust_target",
    "helper_rust_target",
    "public_artifact",
    "helper_artifact",
    "public_construction_authority",
    "public_construction_label",
    "archive",
    "vault",
    "runtime_authority",
    "platform_signature",
}
PLATFORM_SIGNATURES = {
    "developer-id-notarized",
    "release-manifest",
    "unsigned",
}


def load_and_validate(path: Path = MATRIX_PATH) -> dict[str, object]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if value.get("schema_version") != 1 or set(value) != {"schema_version", "targets"}:
        raise ValueError("release target matrix must use the exact V1 envelope")
    targets = value.get("targets")
    if not isinstance(targets, list):
        raise ValueError("release target matrix targets must be a list")
    ids: list[str] = []
    for target in targets:
        if not isinstance(target, dict):
            raise ValueError("release target entries must be objects")
        if set(target) != REQUIRED_STRING_FIELDS | {
            "diagnostic_authorities",
            "linux_build",
        }:
            raise ValueError("release target entry has missing or unexpected fields")
        if any(not isinstance(target[field], str) or not target[field] for field in REQUIRED_STRING_FIELDS):
            raise ValueError("release target string fields must be non-empty")
        diagnostics = target["diagnostic_authorities"]
        if not isinstance(diagnostics, list) or any(
            not isinstance(authority, str) or not authority for authority in diagnostics
        ):
            raise ValueError("diagnostic authorities must be non-empty strings")
        if target["platform_signature"] not in PLATFORM_SIGNATURES:
            raise ValueError("unsupported platform signature policy")
        if target["public_construction_authority"] != "bazel-release-route-v1":
            raise ValueError("public construction authority must be Bazel release V1")
        if target["public_construction_label"] != (
            f"//:ctx_release_{target['id'].replace('-', '_')}"
        ):
            raise ValueError("public construction label does not match target ID")
        target_id = target["id"]
        ids.append(target_id)
        if target["runtime_authority"] in diagnostics:
            raise ValueError(f"authoritative runner is also diagnostic for {target_id}")
        expected = EXPECTED.get(target_id)
        actual = {
            field: target[field]
            for field in REQUIRED_STRING_FIELDS
            | {"diagnostic_authorities", "linux_build"}
            if field != "id"
        }
        if expected != actual:
            raise ValueError(f"unexpected release contract for {target_id}")
    if ids != sorted(EXPECTED) or len(ids) != len(set(ids)):
        raise ValueError("release target IDs must be the exact sorted Day One matrix")
    return value


def main() -> int:
    try:
        value = load_and_validate()
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"release target matrix: {error}", file=sys.stderr)
        return 1
    print(f"release target matrix: OK ({len(value['targets'])} targets)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
