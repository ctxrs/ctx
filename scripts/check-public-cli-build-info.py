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


def validate(
    artifact: Path,
    build_info_path: Path,
    matrix_path: Path,
    platform: str,
    expected_source_commit: str | None = None,
    cargo_lock_path: Path | None = None,
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
    if (
        value.get("schema_version") != 1
        or value.get("platform") != platform
        or value.get("target") != target.get("public_rust_target")
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
            f"{release_label} build-info does not match the matrix build contract"
        )
    if (
        not isinstance(gates, dict)
        or gates.get("static") != "passed"
        or gates.get("static_abi") != "passed"
    ):
        raise ValueError(
            f"{release_label} build-info does not record passed static ABI gates"
        )
    build_info_sha256 = hashlib.sha256(build_info_bytes).hexdigest()
    if target.get("os") != "linux":
        return build_info_sha256

    if not isinstance(linux_build, dict):
        raise ValueError("Linux release build contract is malformed")
    if (
        gates.get("local_runtime") != "passed"
        or gates.get("local_runtime_authority") != "authoritative"
    ):
        raise ValueError(
            "Linux release build-info does not record an authoritative runtime gate"
        )

    builder_image = linux_build.get("builder_image")
    if (
        not isinstance(builder_image, str)
        or "@" not in builder_image
        or not isinstance(builder, dict)
    ):
        raise ValueError("Linux release builder provenance is malformed")
    expected_base = builder_image.rsplit("@", 1)[1]
    base_image = builder.get("base_image")
    if (
        not isinstance(base_image, dict)
        or base_image.get("expected") != expected_base
        or base_image.get("actual") != expected_base
        or not lower_hex(builder.get("recipe_sha256"), 64)
        or not lower_hex(
            builder.get("image_id")[7:]
            if isinstance(builder.get("image_id"), str)
            and builder.get("image_id").startswith("sha256:")
            else None,
            64,
        )
    ):
        raise ValueError(
            "Linux release build-info does not bind the exact builder image"
        )
    for label, image in (("runtime", runtime), ("inspector", inspector)):
        image_id = image.get("image_id") if isinstance(image, dict) else None
        if (
            not isinstance(image_id, str)
            or not image_id.startswith("sha256:")
            or not lower_hex(image_id[7:], 64)
        ):
            raise ValueError(f"Linux release {label} image provenance is invalid")

    rust_toolchain = linux_build.get("rust_toolchain")
    rust_commit = linux_build.get("rust_commit")
    rust_version = value.get("rust_version")
    if (
        not isinstance(rust_toolchain, str)
        or not lower_hex(rust_commit, 40)
        or not isinstance(rust_version, str)
        or re.fullmatch(
            rf"rustc {re.escape(rust_toolchain)} "
            rf"\({re.escape(rust_commit[:9])} \d{{4}}-\d{{2}}-\d{{2}}\)",
            rust_version,
        )
        is None
    ):
        raise ValueError(
            "Linux release build-info does not bind the matrix Rust toolchain"
        )
    return build_info_sha256


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--build-info", type=Path, required=True)
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--source-commit")
    parser.add_argument("--cargo-lock", type=Path)
    args = parser.parse_args()
    if args.source_commit is not None and (
        not lower_hex(args.source_commit, 40) or args.source_commit == "0" * 40
    ):
        parser.error("--source-commit must be a nonzero lowercase 40-hex commit")
    try:
        print(
            validate(
                args.artifact,
                args.build_info,
                args.matrix,
                args.platform,
                args.source_commit,
                args.cargo_lock,
            )
        )
    except (OSError, ValueError) as error:
        raise SystemExit(f"error: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
