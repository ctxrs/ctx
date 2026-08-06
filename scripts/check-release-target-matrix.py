#!/usr/bin/env python3
"""Validate the public release-target authority and its generated Bazel routes."""

from __future__ import annotations

import json
from pathlib import Path
import re
import sys
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = ROOT / "contracts" / "release-targets-v1.json"
ADVISORY_POLICY_PATH = ROOT / "security" / "release-advisory-policy-v1.json"
BAZEL_CONSUMER_PATH = ROOT / "tools" / "bazel" / "release_inventory.bzl"
GENERATED_BEGIN = "# BEGIN GENERATED: contracts/release-targets-v1.json"
GENERATED_END = "# END GENERATED: contracts/release-targets-v1.json"
SUPPORTED_TARGET_IDS = tuple(
    "freebsd-x64 linux-arm64 linux-x64 macos-arm64 macos-x64 windows-x64".split()
)
STRING_FIELDS = set(
    """
    arch archive bazel_platform helper_artifact helper_rust_target id os
    platform_signature public_artifact public_construction_authority
    public_construction_label public_rust_target runtime_authority vault
    """.split()
)
TARGET_FIELDS = STRING_FIELDS | {"diagnostic_authorities", "linux_build"}
LINUX_FIELDS = set(
    "builder_image glibc_max rust_commit rust_sysroot rust_toolchain "
    "ubuntu_snapshot".split()
)


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
    if (
        re.fullmatch(r"[^@\s]+@sha256:[0-9a-f]{64}", linux_build["builder_image"])
        is None
        or re.fullmatch(r"\d{8}T\d{6}Z", linux_build["ubuntu_snapshot"]) is None
        or re.fullmatch(r"\d+\.\d+", linux_build["glibc_max"]) is None
        or re.fullmatch(r"\d+\.\d+\.\d+", linux_build["rust_toolchain"]) is None
        or re.fullmatch(r"[0-9a-f]{40}", linux_build["rust_commit"]) is None
        or linux_build["rust_commit"] == "0" * 40
        or not linux_build["rust_sysroot"].startswith("/opt/rustup/toolchains/")
    ):
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
    if target["public_construction_authority"] != "bazel-release-route-v1":
        raise ValueError("public construction authority must be Bazel release V1")
    if target["public_construction_label"] != (
        f"//:ctx_release_{target['id'].replace('-', '_')}"
    ):
        raise ValueError("public construction label does not match target ID")
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
        target["platform_signature"]
        not in {"developer-id-notarized", "release-manifest", "unsigned"}
        or target["archive"] not in {"tar.gz", "zip"}
        or re.fullmatch(r"native-[a-z0-9_-]+", target["runtime_authority"]) is None
        or re.fullmatch(r"[A-Za-z0-9._-]+", target["public_artifact"]) is None
        or re.fullmatch(r"[A-Za-z0-9._-]+", target["helper_artifact"]) is None
        or re.fullmatch(
            r"//tools/bazel/platforms:release_[a-z0-9_]+",
            target["bazel_platform"],
        )
        is None
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
        raise ValueError("release target IDs must be the exact sorted Day One matrix")
    for target in value["targets"]:
        validate_target(target)
    return value


def validate_advisory_policy_coverage(
    value: dict[str, Any], path: Path = ADVISORY_POLICY_PATH
) -> None:
    policy = json.loads(path.read_text(encoding="utf-8"))
    scanner = policy.get("scanner")
    scanner_hashes = (
        scanner.get("sha256_by_target") if isinstance(scanner, dict) else None
    )
    if not isinstance(scanner_hashes, dict):
        raise ValueError("release advisory scanner target map is missing")
    release_targets = {target["id"] for target in value["targets"]}
    scanner_targets = set(scanner_hashes)
    if scanner_targets != release_targets:
        missing = sorted(release_targets - scanner_targets)
        unexpected = sorted(scanner_targets - release_targets)
        details = []
        if missing:
            details.append("missing: " + ", ".join(missing))
        if unexpected:
            details.append("unexpected: " + ", ".join(unexpected))
        raise ValueError(
            "release advisory scanner targets do not match release matrix ("
            + "; ".join(details)
            + ")"
        )
    malformed = sorted(
        target
        for target, digest in scanner_hashes.items()
        if not isinstance(digest, str)
        or re.fullmatch(r"[0-9a-f]{64}", digest) is None
    )
    if malformed:
        raise ValueError(
            "release advisory scanner has malformed SHA-256 for: "
            + ", ".join(malformed)
        )


def generated_bazel_consumer(value: dict[str, Any]) -> str:
    rows = [
        f'    "{target["id"]}": ("{target["bazel_platform"]}", '
        f'"{target["public_rust_target"]}"),'
        for target in value["targets"]
    ]
    return "\n".join(
        [GENERATED_BEGIN, "PUBLIC_RELEASE_ROUTES = {", *rows, "}", GENERATED_END]
    )


def updated_bazel_consumer(source: str, value: dict[str, Any]) -> str:
    if source.count(GENERATED_BEGIN) != 1 or source.count(GENERATED_END) != 1:
        raise ValueError("Bazel release consumer must contain one generated block")
    before, remainder = source.split(GENERATED_BEGIN, 1)
    _, after = remainder.split(GENERATED_END, 1)
    return before + generated_bazel_consumer(value) + after


def validate_bazel_consumer(path: Path, value: dict[str, Any]) -> None:
    source = path.read_text(encoding="utf-8")
    if updated_bazel_consumer(source, value) != source:
        raise ValueError(
            "Bazel release consumer is stale; run "
            "scripts/check-release-target-matrix.py --write-bazel-consumer"
        )


def main() -> int:
    try:
        if sys.argv[1:] not in ([], ["--write-bazel-consumer"]):
            raise ValueError("usage: check-release-target-matrix.py [--write-bazel-consumer]")
        value = load_and_validate()
        validate_advisory_policy_coverage(value)
        if BAZEL_CONSUMER_PATH.is_file():
            if sys.argv[1:]:
                source = BAZEL_CONSUMER_PATH.read_text(encoding="utf-8")
                BAZEL_CONSUMER_PATH.write_text(
                    updated_bazel_consumer(source, value),
                    encoding="utf-8",
                )
            validate_bazel_consumer(BAZEL_CONSUMER_PATH, value)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"release target matrix: {error}", file=sys.stderr)
        return 1
    print(f"release target matrix: OK ({len(value['targets'])} targets)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
