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
        "archive": "tar.gz",
        "vault": "secret-service",
        "runtime_authority": "native-freebsd-x86_64",
        "diagnostic_authorities": [],
        "platform_signature": "release-manifest",
    },
    "linux-arm64": {
        "os": "linux",
        "arch": "aarch64",
        "public_rust_target": "aarch64-unknown-linux-gnu",
        "helper_rust_target": "aarch64-unknown-linux-gnu",
        "public_artifact": "ctx-linux-aarch64",
        "helper_artifact": "ctx-pro-linux-arm64",
        "archive": "tar.gz",
        "vault": "secret-service",
        "runtime_authority": "native-linux-aarch64",
        "diagnostic_authorities": ["qemu-user"],
        "platform_signature": "release-manifest",
    },
    "linux-x64": {
        "os": "linux",
        "arch": "x86_64",
        "public_rust_target": "x86_64-unknown-linux-gnu",
        "helper_rust_target": "x86_64-unknown-linux-gnu",
        "public_artifact": "ctx-linux-x64",
        "helper_artifact": "ctx-pro-linux-x64",
        "archive": "tar.gz",
        "vault": "secret-service",
        "runtime_authority": "native-linux-x86_64",
        "diagnostic_authorities": [],
        "platform_signature": "release-manifest",
    },
    "macos-arm64": {
        "os": "macos",
        "arch": "aarch64",
        "public_rust_target": "aarch64-apple-darwin",
        "helper_rust_target": "aarch64-apple-darwin",
        "public_artifact": "ctx-macos-arm64",
        "helper_artifact": "ctx-pro-macos-arm64",
        "archive": "tar.gz",
        "vault": "keychain",
        "runtime_authority": "native-apple-arm64",
        "diagnostic_authorities": [],
        "platform_signature": "developer-id-notarized",
    },
    "macos-x64": {
        "os": "macos",
        "arch": "x86_64",
        "public_rust_target": "x86_64-apple-darwin",
        "helper_rust_target": "x86_64-apple-darwin",
        "public_artifact": "ctx-macos-x64",
        "helper_artifact": "ctx-pro-macos-x64",
        "archive": "tar.gz",
        "vault": "keychain",
        "runtime_authority": "native-macos-x86_64",
        "diagnostic_authorities": ["rosetta-2", "non-apple-qemu"],
        "platform_signature": "developer-id-notarized",
    },
    "windows-x64": {
        "os": "windows",
        "arch": "x86_64",
        "public_rust_target": "x86_64-pc-windows-gnu",
        "helper_rust_target": "x86_64-pc-windows-msvc",
        "public_artifact": "ctx-windows-x64.exe",
        "helper_artifact": "ctx-pro-windows-x64.exe",
        "archive": "zip",
        "vault": "credential-manager",
        "runtime_authority": "native-windows-x86_64",
        "diagnostic_authorities": [],
        "platform_signature": "unsigned",
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
        if set(target) != REQUIRED_STRING_FIELDS | {"diagnostic_authorities"}:
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
        target_id = target["id"]
        ids.append(target_id)
        if target["runtime_authority"] in diagnostics:
            raise ValueError(f"authoritative runner is also diagnostic for {target_id}")
        expected = EXPECTED.get(target_id)
        actual = {
            field: target[field]
            for field in REQUIRED_STRING_FIELDS | {"diagnostic_authorities"}
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
