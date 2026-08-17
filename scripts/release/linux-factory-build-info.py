#!/usr/bin/env python3
"""Create deterministic build evidence for the Linux cross-release factory."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess


HEX_40 = re.compile(r"[0-9a-f]{40}")
RECIPE_PIN = re.compile(
    r'^readonly (RUST_VERSION|RUST_COMMIT|ZIG_VERSION|CARGO_ZIGBUILD_VERSION)="([^"\n]+)"$',
    re.MULTILINE,
)
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def target(matrix: Path, platform: str) -> dict[str, object]:
    value = json.loads(matrix.read_text(encoding="utf-8"))
    target_id = "linux-arm64" if platform == "linux-aarch64" else platform
    matches = [item for item in value["targets"] if item["id"] == target_id]
    if len(matches) != 1:
        raise ValueError("release target matrix does not contain the exact platform")
    return matches[0]


def factory_inputs(matrix: Path) -> dict[str, object]:
    path = matrix.with_name("release-factory-inputs-v1.json")
    value = json.loads(path.read_text(encoding="utf-8"))
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


def exact_recipe(matrix: Path, selected: dict[str, object], supplied: Path) -> Path:
    label = selected.get("public_construction_label")
    if not isinstance(label, str) or not label or Path(label).is_absolute():
        raise ValueError("release target construction label is malformed")
    matrix_path = matrix.resolve(strict=True)
    root = matrix_path.parent.parent
    expected = (root / label).resolve(strict=True)
    recipe = supplied.resolve(strict=True)
    try:
        metadata = supplied.lstat()
    except OSError as error:
        raise ValueError("release factory recipe is unavailable") from error
    if (
        not expected.is_relative_to(root)
        or recipe != expected
        or stat.S_ISLNK(metadata.st_mode)
        or not stat.S_ISREG(metadata.st_mode)
    ):
        raise ValueError("release factory recipe does not match the target contract")
    return recipe


def recipe_pins(recipe: Path) -> dict[str, str]:
    source = recipe.read_text(encoding="utf-8")
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
        or HEX_40.fullmatch(pins["RUST_COMMIT"]) is None
        or pins["RUST_COMMIT"] == "0" * 40
        or VERSION.fullmatch(pins["ZIG_VERSION"]) is None
        or VERSION.fullmatch(pins["CARGO_ZIGBUILD_VERSION"]) is None
    ):
        raise ValueError("release factory recipe toolchain pins are malformed")
    return pins


def expected_rust_version(pins: dict[str, str]) -> re.Pattern[str]:
    version = re.escape(pins["RUST_VERSION"])
    commit = re.escape(pins["RUST_COMMIT"][:9])
    return re.compile(rf"rustc {version} \({commit} [0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}\)")


def clean_source(repo: Path, commit: str) -> None:
    if HEX_40.fullmatch(commit) is None or commit == "0" * 40:
        raise ValueError("source commit must be nonzero lowercase 40-hex")
    observed = subprocess.check_output(
        ["git", "-C", os.fspath(repo), "rev-parse", "--verify", "HEAD^{commit}"],
        text=True,
    ).strip()
    if observed != commit:
        raise ValueError("source commit does not match the factory checkout")
    status = subprocess.check_output(
        ["git", "-C", os.fspath(repo), "status", "--porcelain=v1"], text=True
    )
    if status:
        raise ValueError("release factory source checkout is dirty")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", required=True, type=Path)
    parser.add_argument("--cargo-lock", required=True, type=Path)
    parser.add_argument("--matrix", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--recipe", required=True, type=Path)
    parser.add_argument("--rust-version", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-repo", required=True, type=Path)
    parser.add_argument("--static-status", choices=("passed",), required=True)
    parser.add_argument(
        "--local-runtime-status", choices=("passed", "not_run"), required=True
    )
    parser.add_argument(
        "--local-runtime-authority",
        choices=("authoritative", "not_run"),
        required=True,
    )
    parser.add_argument("--zig-version", required=True)
    parser.add_argument("--cargo-zigbuild-version", required=True)
    parser.add_argument("--builder-authority", required=True)
    parser.add_argument("--builder-os", required=True)
    parser.add_argument("--inspector-authority", required=True)
    parser.add_argument("--inspector-tool", required=True)
    parser.add_argument("--macos-sdk-sha256")
    parser.add_argument("--macos-sdk-authority")
    args = parser.parse_args()
    try:
        repo = args.source_repo.resolve(strict=True)
        clean_source(repo, args.source_commit)
        selected = target(args.matrix, args.platform)
        inputs = factory_inputs(args.matrix)
        recipe = exact_recipe(args.matrix, selected, args.recipe)
        pins = recipe_pins(recipe)
        linux_host = inputs["linux_host"]
        if not isinstance(linux_host, dict):
            raise ValueError("release factory Linux host contract is malformed")
        expected_builder_os = (
            f'{linux_host.get("os_id")}-{linux_host.get("os_version")}-'
            f'{linux_host.get("arch")}'
        )
        if (
            args.builder_authority != linux_host.get("authority")
            or args.builder_os != expected_builder_os
        ):
            raise ValueError("builder identity does not match the factory input contract")
        if (
            args.zig_version != pins["ZIG_VERSION"]
            or args.cargo_zigbuild_version != pins["CARGO_ZIGBUILD_VERSION"]
            or expected_rust_version(pins).fullmatch(args.rust_version) is None
        ):
            raise ValueError("observed toolchain does not match the factory recipe")
        is_macos = selected["os"] == "macos"
        if is_macos != bool(args.macos_sdk_sha256):
            raise ValueError("macOS SDK identity is required exactly for macOS targets")
        if is_macos != bool(args.macos_sdk_authority):
            raise ValueError("macOS SDK authority is required exactly for macOS targets")
        if is_macos:
            macos_sdk = inputs.get("macos_sdk")
            if (
                not isinstance(macos_sdk, dict)
                or args.macos_sdk_sha256 != macos_sdk.get("archive_sha256")
                or args.macos_sdk_authority != macos_sdk.get("authority")
            ):
                raise ValueError("macOS SDK identity does not match the factory input contract")
        if (args.local_runtime_status == "passed") != (
            args.local_runtime_authority == "authoritative"
        ):
            raise ValueError("runtime status and authority disagree")
        document = {
            "artifact_sha256": sha256(args.artifact),
            "build_system": "cargo-zigbuild",
            "builder": {
                "authority": args.builder_authority,
                "image_id": None,
                "os": args.builder_os,
                "recipe_sha256": sha256(recipe),
            },
            "cargo_lock_sha256": sha256(args.cargo_lock),
            "gates": {
                "local_runtime": args.local_runtime_status,
                "local_runtime_authority": args.local_runtime_authority,
                "static": args.static_status,
                "static_abi": args.static_status,
            },
            "inspector": {
                "authority": args.inspector_authority,
                "image_id": None,
                "tool": args.inspector_tool,
            },
            "linux_build": selected["linux_build"],
            "platform": args.platform,
            "release_factory": {
                "authority": selected["public_construction_authority"],
                "cargo_zigbuild_version": args.cargo_zigbuild_version,
                "macos_sdk_sha256": args.macos_sdk_sha256,
                "macos_sdk_authority": args.macos_sdk_authority,
                "zig_version": args.zig_version,
            },
            "representative_cpu_proof": {"profile": None, "qemu_version": None},
            "runtime": {
                "authority": "native-fanout-deferred-v1",
                "image_id": None,
            },
            "rust_version": args.rust_version,
            "schema_version": 1,
            "source": {"clean": True, "commit": args.source_commit},
            "target": selected["public_rust_target"],
        }
        payload = json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
        args.output.parent.mkdir(parents=True, exist_ok=True)
        temporary = args.output.with_name(f".{args.output.name}.tmp.{os.getpid()}")
        temporary.write_text(payload, encoding="utf-8")
        os.replace(temporary, args.output)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"error: {error}") from error
    print(args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
