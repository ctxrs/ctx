#!/usr/bin/env python3
"""Validate an exact release artifact and its matrix-bound build evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import stat
from typing import Any


VERSION = re.compile(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
)
RECIPE_PIN = re.compile(
    r'^readonly (RUST_VERSION|RUST_COMMIT|ZIG_VERSION|CARGO_ZIGBUILD_VERSION)="([^"\n]+)"$',
    re.MULTILINE,
)


def regular(path: Path, label: str, maximum: int) -> bytes:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise ValueError(f"{label} is unavailable: {path}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise ValueError(f"{label} is not a regular file: {path}")
    if metadata.st_size <= 0 or metadata.st_size > maximum:
        raise ValueError(f"{label} has an invalid size: {path}")
    try:
        return path.read_bytes()
    except OSError as error:
        raise ValueError(f"{label} could not be read: {path}") from error


def lower_hex(value: object, length: int) -> bool:
    return isinstance(value, str) and re.fullmatch(
        rf"[0-9a-f]{{{length}}}", value
    ) is not None


def target_by_id(matrix: object, platform: str) -> dict[str, Any]:
    if not isinstance(matrix, dict) or matrix.get("schema_version") != 1:
        raise ValueError("release-target matrix schema is invalid")
    targets = matrix.get("targets")
    if not isinstance(targets, list):
        raise ValueError("release-target matrix targets are invalid")
    target_id = "linux-arm64" if platform == "linux-aarch64" else platform
    matches = [
        target
        for target in targets
        if isinstance(target, dict) and target.get("id") == target_id
    ]
    if len(matches) != 1:
        raise ValueError("release-target matrix does not contain the exact target")
    return matches[0]


def factory_recipe(
    matrix_path: Path, target: dict[str, Any]
) -> tuple[bytes, dict[str, str]]:
    label = target.get("public_construction_label")
    if not isinstance(label, str) or not label or Path(label).is_absolute():
        raise ValueError("release target construction label is malformed")
    root = matrix_path.resolve(strict=True).parent.parent
    recipe_path = root / label
    try:
        if not recipe_path.resolve(strict=True).is_relative_to(root):
            raise ValueError("release target construction label escapes the repository")
    except OSError as error:
        raise ValueError("release factory recipe is unavailable") from error
    recipe_bytes = regular(recipe_path, "release factory recipe", 2 * 1024 * 1024)
    try:
        source = recipe_bytes.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ValueError("release factory recipe is not UTF-8") from error
    pins: dict[str, str] = {}
    for name, value in RECIPE_PIN.findall(source):
        if name in pins:
            raise ValueError(f"release factory recipe repeats {name}")
        pins[name] = value
    required = {
        "RUST_VERSION",
        "RUST_COMMIT",
        "ZIG_VERSION",
        "CARGO_ZIGBUILD_VERSION",
    }
    if set(pins) != required:
        raise ValueError("release factory recipe toolchain pins are incomplete")
    if (
        VERSION.fullmatch(pins["RUST_VERSION"]) is None
        or not lower_hex(pins["RUST_COMMIT"], 40)
        or pins["RUST_COMMIT"] == "0" * 40
        or VERSION.fullmatch(pins["ZIG_VERSION"]) is None
        or VERSION.fullmatch(pins["CARGO_ZIGBUILD_VERSION"]) is None
    ):
        raise ValueError("release factory recipe toolchain pins are malformed")
    return recipe_bytes, pins


def factory_inputs(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(regular(path, "release factory inputs", 256 * 1024))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("release factory input contract is malformed") from error
    linux_host = value.get("linux_host") if isinstance(value, dict) else None
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != 1
        or value.get("kind") != "ctx-release-factory-inputs"
        or not isinstance(linux_host, dict)
        or set(linux_host) != {"arch", "authority", "os_id", "os_version"}
        or any(not isinstance(item, str) or not item for item in linux_host.values())
    ):
        raise ValueError("release factory input contract is malformed")
    return value


def rust_version_matches(observed: object, pins: dict[str, str]) -> bool:
    if not isinstance(observed, str):
        return False
    version = re.escape(pins["RUST_VERSION"])
    commit = re.escape(pins["RUST_COMMIT"][:9])
    return re.fullmatch(
        rf"rustc {version} \({commit} [0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}\)",
        observed,
    ) is not None


def validate(
    artifact: Path,
    build_info_path: Path,
    matrix_path: Path,
    platform: str,
    expected_source_commit: str | None = None,
    cargo_lock_path: Path | None = None,
    factory_inputs_path: Path | None = None,
) -> str:
    release_label = f"{platform} release"
    artifact_bytes = regular(
        artifact, f"{release_label} artifact", 256 * 1024 * 1024
    )
    build_info_bytes = regular(
        build_info_path, f"{release_label} build-info", 64 * 1024
    )
    matrix_bytes = regular(matrix_path, "release-target matrix", 256 * 1024)
    try:
        value = json.loads(build_info_bytes)
        matrix = json.loads(matrix_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("release build-info or target matrix is malformed") from error
    target = target_by_id(matrix, platform)
    linux_build = target.get("linux_build")
    if not isinstance(value, dict):
        raise ValueError(f"{release_label} build-info is malformed")

    artifact_sha256 = hashlib.sha256(artifact_bytes).hexdigest()
    cargo_lock_sha256 = (
        hashlib.sha256(
            regular(cargo_lock_path, "Cargo.lock", 32 * 1024 * 1024)
        ).hexdigest()
        if cargo_lock_path is not None
        else None
    )
    source = value.get("source")
    gates = value.get("gates")
    builder = value.get("builder")
    runtime = value.get("runtime")
    inspector = value.get("inspector")
    release_factory = value.get("release_factory")
    expected_top = {
        "artifact_sha256",
        "build_system",
        "builder",
        "cargo_lock_sha256",
        "gates",
        "inspector",
        "linux_build",
        "platform",
        "release_factory",
        "representative_cpu_proof",
        "runtime",
        "rust_version",
        "schema_version",
        "source",
        "target",
    }
    if (
        set(value) != expected_top
        or value.get("schema_version") != 1
        or value.get("platform") != platform
        or value.get("target") != target.get("public_rust_target")
        or value.get("build_system") != "cargo-zigbuild"
        or value.get("artifact_sha256") != artifact_sha256
        or not lower_hex(value.get("cargo_lock_sha256"), 64)
        or (
            cargo_lock_sha256 is not None
            and value.get("cargo_lock_sha256") != cargo_lock_sha256
        )
        or not isinstance(source, dict)
        or source.get("clean") is not True
        or not lower_hex(source.get("commit"), 40)
        or source.get("commit") == "0" * 40
        or (
            expected_source_commit is not None
            and source.get("commit") != expected_source_commit
        )
    ):
        raise ValueError(
            f"{release_label} build-info does not bind the clean exact artifact"
        )
    if value.get("linux_build") != linux_build:
        raise ValueError(
            f"{release_label} build-info does not match the matrix ABI contract"
        )
    if (
        not isinstance(gates, dict)
        or set(gates)
        != {"local_runtime", "local_runtime_authority", "static", "static_abi"}
        or gates.get("static") != "passed"
        or gates.get("static_abi") != "passed"
        or gates.get("local_runtime") != "not_run"
        or gates.get("local_runtime_authority") != "not_run"
    ):
        raise ValueError(
            f"{release_label} build-info does not record factory and static ABI gates"
        )
    build_info_sha256 = hashlib.sha256(build_info_bytes).hexdigest()
    if target.get("public_construction_authority") != "linux-cross-cargo-zigbuild-v1":
        raise ValueError(
            f"{release_label} build-info uses a non-factory construction authority"
        )
    if target.get("public_construction_authority") == "linux-cross-cargo-zigbuild-v1":
        recipe_bytes, pins = factory_recipe(matrix_path, target)
        inputs_path = factory_inputs_path or matrix_path.with_name(
            "release-factory-inputs-v1.json"
        )
        inputs = factory_inputs(inputs_path)
        linux_host = inputs["linux_host"]
        if not isinstance(linux_host, dict):
            raise ValueError("release factory Linux host contract is unavailable")
        expected_sdk_sha256 = None
        expected_sdk_authority = None
        expected_builder_authority = linux_host["authority"]
        expected_builder_os = (
            f'{linux_host["os_id"]}-{linux_host["os_version"]}-'
            f'{linux_host["arch"]}'
        )
        if target.get("os") == "macos":
            macos_sdk = inputs.get("macos_sdk")
            if not isinstance(macos_sdk, dict):
                raise ValueError("release factory macOS SDK contract is unavailable")
            expected_sdk_sha256 = macos_sdk.get("archive_sha256")
            expected_sdk_authority = macos_sdk.get("authority")
        if (
            not isinstance(release_factory, dict)
            or set(release_factory)
            != {
                "authority",
                "cargo_zigbuild_version",
                "macos_sdk_authority",
                "macos_sdk_sha256",
                "zig_version",
            }
            or release_factory.get("authority")
            != target.get("public_construction_authority")
            or release_factory.get("zig_version") != pins["ZIG_VERSION"]
            or release_factory.get("cargo_zigbuild_version")
            != pins["CARGO_ZIGBUILD_VERSION"]
            or release_factory.get("macos_sdk_sha256") != expected_sdk_sha256
            or release_factory.get("macos_sdk_authority")
            != expected_sdk_authority
            or not rust_version_matches(value.get("rust_version"), pins)
        ):
            raise ValueError(
                f"{release_label} build-info does not bind the pinned Linux factory"
            )
        if (
            not isinstance(builder, dict)
            or set(builder) != {"authority", "image_id", "os", "recipe_sha256"}
            or builder.get("authority") != expected_builder_authority
            or builder.get("image_id") is not None
            or builder.get("os") != expected_builder_os
            or builder.get("recipe_sha256")
            != hashlib.sha256(recipe_bytes).hexdigest()
        ):
            raise ValueError(
                f"{release_label} builder provenance does not match the factory inputs and recipe"
            )
        if (
            not isinstance(inspector, dict)
            or set(inspector) != {"authority", "image_id", "tool"}
            or not isinstance(inspector.get("authority"), str)
            or not inspector["authority"]
            or inspector.get("image_id") is not None
            or not isinstance(inspector.get("tool"), str)
            or not inspector["tool"]
        ):
            raise ValueError(f"{release_label} inspector tool identity is missing")
        if (
            runtime
            != {"authority": "native-fanout-deferred-v1", "image_id": None}
            or value.get("representative_cpu_proof")
            != {"profile": None, "qemu_version": None}
        ):
            raise ValueError(
                f"{release_label} runtime provenance is not factory-deferred"
            )
    return build_info_sha256


def candidate_version(
    artifact: Path,
    build_info_path: Path,
    candidate_manifest_path: Path,
    version_path: Path,
    platform: str,
    build_info_sha256: str,
) -> str:
    artifact_bytes = regular(
        artifact, f"{platform} release artifact", 256 * 1024 * 1024
    )
    build_info_bytes = regular(
        build_info_path, f"{platform} release build-info", 64 * 1024
    )
    candidate_bytes = regular(
        candidate_manifest_path, f"{platform} candidate manifest", 32 * 1024 * 1024
    )
    version_bytes = regular(version_path, f"{platform} construction version", 256)
    try:
        build_info = json.loads(build_info_bytes)
        candidate = json.loads(candidate_bytes)
        version_sidecar = version_bytes.decode("utf-8")
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(
            f"{platform} candidate, build-info, or version sidecar is malformed"
        ) from error

    expected_top = {
        "schema_version",
        "kind",
        "construction",
        "product",
        "version",
        "target",
        "source",
        "artifact",
        "evidence",
        "tantivy",
    }
    expected_target_id = "linux-arm64" if platform == "linux-aarch64" else platform
    version = candidate.get("version") if isinstance(candidate, dict) else None
    target = candidate.get("target") if isinstance(candidate, dict) else None
    artifact_record = candidate.get("artifact") if isinstance(candidate, dict) else None
    evidence = candidate.get("evidence") if isinstance(candidate, dict) else None
    build_info_record = evidence.get("build_info") if isinstance(evidence, dict) else None
    source = build_info.get("source") if isinstance(build_info, dict) else None
    actual_build_info_sha256 = hashlib.sha256(build_info_bytes).hexdigest()
    if (
        not isinstance(candidate, dict)
        or set(candidate) != expected_top
        or candidate.get("schema_version") != 1
        or candidate.get("kind") != "ctx-public-cli-candidate"
        or candidate.get("product") != "core"
        or not isinstance(version, str)
        or VERSION.fullmatch(version) is None
        or not isinstance(target, dict)
        or target.get("id") != expected_target_id
        or target.get("platform") != platform
        or not isinstance(build_info, dict)
        or target.get("rust_triple") != build_info.get("target")
        or candidate.get("source") != source
        or actual_build_info_sha256 != build_info_sha256
        or artifact_record
        != {
            "file": artifact.name,
            "sha256": hashlib.sha256(artifact_bytes).hexdigest(),
            "size_bytes": len(artifact_bytes),
        }
        or build_info_record
        != {"file": build_info_path.name, "sha256": build_info_sha256}
    ):
        raise ValueError(
            f"{platform} candidate manifest does not bind the exact artifact and build-info"
        )

    allowed_sidecars = {
        f"ctx {version}\n",
        f"not run on this host: {platform}\n",
    }
    if version_sidecar not in allowed_sidecars:
        raise ValueError(
            f"{platform} construction version sidecar does not match candidate version {version}"
        )
    return version


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--build-info", type=Path, required=True)
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--source-commit")
    parser.add_argument("--cargo-lock", type=Path)
    parser.add_argument("--candidate-manifest", type=Path)
    parser.add_argument("--version-file", type=Path)
    parser.add_argument(
        "--factory-inputs",
        type=Path,
    )
    args = parser.parse_args()
    if args.source_commit is not None and (
        not lower_hex(args.source_commit, 40) or args.source_commit == "0" * 40
    ):
        parser.error("--source-commit must be a nonzero lowercase 40-hex commit")
    if (args.candidate_manifest is None) != (args.version_file is None):
        parser.error(
            "--candidate-manifest and --version-file must be supplied together"
        )
    try:
        build_info_sha256 = validate(
            args.artifact,
            args.build_info,
            args.matrix,
            args.platform,
            args.source_commit,
            args.cargo_lock,
            args.factory_inputs,
        )
        if args.candidate_manifest is None:
            print(build_info_sha256)
        else:
            print(
                candidate_version(
                    args.artifact,
                    args.build_info,
                    args.candidate_manifest,
                    args.version_file,
                    args.platform,
                    build_info_sha256,
                )
            )
    except (OSError, ValueError) as error:
        raise SystemExit(f"error: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
