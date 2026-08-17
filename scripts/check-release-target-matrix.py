#!/usr/bin/env python3
"""Validate the public release-target authority and its release matrix."""

from __future__ import annotations

import json
from pathlib import Path
import re
import sys
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = ROOT / "contracts" / "release-targets-v1.json"
ADVISORY_POLICY_PATH = ROOT / "security" / "release-advisory-policy-v1.json"
SUPPORTED_TARGET_IDS = tuple(
    "linux-arm64 linux-x64 macos-arm64 macos-x64 windows-x64".split()
)
HELPER_RUST_TARGETS = {
    "linux-arm64": "aarch64-unknown-linux-gnu",
    "linux-x64": "x86_64-unknown-linux-gnu",
    "macos-arm64": "aarch64-apple-darwin",
    "macos-x64": "x86_64-apple-darwin",
    "windows-x64": "x86_64-pc-windows-msvc",
}
HELPER_FACTORY_RUST_TARGETS = {
    **HELPER_RUST_TARGETS,
    "windows-x64": "x86_64-pc-windows-gnu",
}
STRING_FIELDS = set(
    """
    arch archive helper_artifact helper_factory_rust_target
    helper_rust_target id managed_pair_bin_dir managed_pair_companion_slot
    managed_pair_core_slot official_companion_rust_target os platform_signature
    public_artifact public_construction_authority public_construction_label
    public_rust_target runtime_authority vault
    """.split()
)
TARGET_FIELDS = STRING_FIELDS | {"diagnostic_authorities", "linux_build"}
LINUX_FIELDS = {"glibc_max"}


def require_string_fields(value: dict[str, Any], fields: set[str], label: str) -> None:
    if any(not isinstance(value[field], str) or not value[field] for field in fields):
        raise ValueError(f"{label} string fields must be non-empty")


def validate_linux_build(target: dict[str, Any]) -> None:
    linux_build = target["linux_build"]
    if target["os"] != "linux":
        if linux_build is not None:
            raise ValueError("non-Linux target has a Linux build contract")
        return
    if not isinstance(linux_build, dict):
        raise ValueError("Linux target must have a build contract")
    if set(linux_build) != LINUX_FIELDS:
        raise ValueError("Linux build contract has missing or unexpected fields")
    require_string_fields(linux_build, LINUX_FIELDS, "Linux build contract")
    if re.fullmatch(r"\d+\.\d+", linux_build["glibc_max"]) is None:
        raise ValueError("Linux build contract has malformed immutable pins")


def validate_target(target: dict[str, Any]) -> None:
    if set(target) != TARGET_FIELDS:
        raise ValueError("release target has missing or unexpected fields")
    require_string_fields(target, STRING_FIELDS, "release target")
    diagnostics = target["diagnostic_authorities"]
    if (
        not isinstance(diagnostics, list)
        or any(not isinstance(item, str) or not item for item in diagnostics)
        or len(diagnostics) != len(set(diagnostics))
    ):
        raise ValueError("diagnostic authorities must be unique non-empty strings")
    if target["runtime_authority"] in diagnostics:
        raise ValueError(f"authoritative runner is also diagnostic for {target['id']}")
    if target["public_construction_authority"] != "linux-cross-cargo-zigbuild-v1":
        raise ValueError("public construction authority must be Linux cross factory V1")
    if target["public_construction_label"] != (
        "scripts/release/build-public-candidate-on-linux.sh"
    ):
        raise ValueError("public construction label must name the Linux factory")
    if target["helper_rust_target"] != HELPER_RUST_TARGETS[target["id"]]:
        raise ValueError(f"unexpected native helper target for {target['id']}")
    if (
        target["helper_factory_rust_target"]
        != HELPER_FACTORY_RUST_TARGETS[target["id"]]
    ):
        raise ValueError(
            f"unexpected helper factory target for {target['id']}"
        )
    if target["official_companion_rust_target"] != target["helper_rust_target"]:
        raise ValueError(
            f"official companion target must match native helper target for {target['id']}"
        )
    suffix = ".exe" if target["os"] == "windows" else ""
    expected_public = f"ctx-{target['id']}{suffix}"
    if target["id"] == "linux-arm64":
        expected_public = "ctx-linux-aarch64"
    if (
        target["public_artifact"] != expected_public
        or target["helper_artifact"] != f"ctx-pro-{target['id']}{suffix}"
    ):
        raise ValueError(f"unexpected release contract for {target['id']}")
    if (
        target["managed_pair_bin_dir"] != "bin"
        or target["managed_pair_core_slot"] != f"bin/ctx{suffix}"
        or target["managed_pair_companion_slot"] != f"libexec/ctx-pro{suffix}"
    ):
        raise ValueError(f"unexpected managed pair installation geometry for {target['id']}")
    if (
        target["platform_signature"]
        not in {"authenticode", "developer-id-notarized", "release-manifest", "unsigned"}
        or target["archive"] not in {"tar.gz", "zip"}
        or re.fullmatch(r"native-[a-z0-9_-]+", target["runtime_authority"]) is None
        or re.fullmatch(r"[A-Za-z0-9._-]+", target["public_artifact"]) is None
        or re.fullmatch(r"[A-Za-z0-9._-]+", target["helper_artifact"]) is None
    ):
        raise ValueError("release target contains an unsupported policy value or path")
    validate_linux_build(target)


def load_and_validate(path: Path = MATRIX_PATH) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != 1
        or set(value) != {"schema_version", "targets"}
        or not isinstance(value["targets"], list)
    ):
        raise ValueError("release target matrix must use the exact V1 envelope")
    if any(not isinstance(target, dict) for target in value["targets"]):
        raise ValueError("release target entries must be objects")
    ids = [target.get("id") for target in value["targets"]]
    if ids != list(SUPPORTED_TARGET_IDS) or len(ids) != len(set(ids)):
        raise ValueError("release target IDs must be the exact sorted prebuilt matrix")
    for target in value["targets"]:
        validate_target(target)
    return value


def validate_advisory_policy_coverage(
    value: dict[str, Any], path: Path = ADVISORY_POLICY_PATH
) -> None:
    policy = json.loads(path.read_text(encoding="utf-8"))
    scanner = policy.get("scanner")
    if (
        not isinstance(scanner, dict)
        or scanner.get("authority") != "ctx-release-osv-linux-x64-v1"
        or scanner.get("name") != "osv-scanner"
        or scanner.get("platform") != "linux-x64"
        or not isinstance(scanner.get("version"), str)
        or not isinstance(scanner.get("sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", scanner["sha256"]) is None
    ):
        raise ValueError("release advisory scanner authority is not the pinned Linux-x64 input")
    return None


def main() -> int:
    try:
        if sys.argv[1:]:
            raise ValueError("usage: check-release-target-matrix.py")
        value = load_and_validate()
        validate_advisory_policy_coverage(value)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"release target matrix: {error}", file=sys.stderr)
        return 1
    print(f"release target matrix: OK ({len(value['targets'])} targets)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
