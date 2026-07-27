#!/usr/bin/env python3
"""Read the exact public ctx release-target matrix for release tools."""

from __future__ import annotations

import argparse
import importlib.util
from pathlib import Path
import shlex
import sys
from types import ModuleType
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
MATRIX = ROOT / "contracts" / "release-targets-v1.json"
CHECKER = ROOT / "scripts" / "check-release-target-matrix.py"
RAW_BINARIES = {
    "freebsd-x64": "ctx-freebsd-x64",
    "linux-arm64": "ctx-linux-aarch64",
    "linux-x64": "ctx",
    "macos-arm64": "ctx-macos-arm64",
    "macos-x64": "ctx-macos-x64",
    "windows-x64": "ctx.exe",
}


class ContractError(ValueError):
    pass


def load_checker(path: Path = CHECKER) -> ModuleType:
    spec = importlib.util.spec_from_file_location("ctx_release_target_checker", path)
    if spec is None or spec.loader is None:
        raise ContractError(f"cannot load release-target checker: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_matrix(path: Path = MATRIX) -> dict[str, Any]:
    try:
        value = load_checker().load_and_validate(path)
    except (OSError, ValueError) as error:
        raise ContractError(str(error)) from error
    if not isinstance(value, dict):
        raise ContractError("release-target checker returned an invalid matrix")
    return value


def find_target(value: dict[str, Any], target_id: str) -> dict[str, Any]:
    targets = value.get("targets")
    if isinstance(targets, list):
        for target in targets:
            if isinstance(target, dict) and target.get("id") == target_id:
                return target
    raise ContractError(f"unsupported release target: {target_id}")


def shell_values(target: dict[str, Any]) -> str:
    target_id = str(target["id"])
    linux_build = target.get("linux_build")
    if not isinstance(linux_build, dict):
        linux_build = {}
    values = {
        "CTX_PUBLIC_TARGET_ID": target_id,
        "CTX_PUBLIC_TARGET_PLATFORM": (
            "linux-aarch64" if target_id == "linux-arm64" else target_id
        ),
        "CTX_PUBLIC_TARGET_OS": target["os"],
        "CTX_PUBLIC_TARGET_ARCH": target["arch"],
        "CTX_PUBLIC_TARGET_TRIPLE": target["public_rust_target"],
        "CTX_PUBLIC_TARGET_ARTIFACT": target["public_artifact"],
        "CTX_PUBLIC_TARGET_BINARY": RAW_BINARIES[target_id],
        "CTX_PUBLIC_TARGET_ARCHIVE": target["archive"],
        "CTX_PUBLIC_TARGET_RUNTIME_AUTHORITY": target["runtime_authority"],
        "CTX_PUBLIC_TARGET_PLATFORM_SIGNATURE": target["platform_signature"],
        "CTX_PUBLIC_TARGET_GLIBC_MAX": linux_build.get("glibc_max", ""),
        "CTX_PUBLIC_TARGET_LINUX_BUILDER_IMAGE": linux_build.get(
            "builder_image", ""
        ),
        "CTX_PUBLIC_TARGET_LINUX_UBUNTU_SNAPSHOT": linux_build.get(
            "ubuntu_snapshot", ""
        ),
        "CTX_PUBLIC_TARGET_LINUX_RUST_TOOLCHAIN": linux_build.get(
            "rust_toolchain", ""
        ),
        "CTX_PUBLIC_TARGET_LINUX_RUST_COMMIT": linux_build.get("rust_commit", ""),
        "CTX_PUBLIC_TARGET_LINUX_RUST_SYSROOT": linux_build.get("rust_sysroot", ""),
    }
    return "\n".join(
        f"{name}={shlex.quote(str(value))}" for name, value in values.items()
    )


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    result.add_argument("--matrix", type=Path, default=MATRIX)
    commands = result.add_subparsers(dest="command", required=True)
    commands.add_parser("verify")
    shell = commands.add_parser("shell")
    shell.add_argument("target")
    targets = commands.add_parser("targets")
    targets.add_argument(
        "--field",
        choices=(
            "id",
            "public_artifact",
            "public_rust_target",
            "runtime_authority",
        ),
        default="id",
    )
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        value = load_matrix(args.matrix)
        if args.command == "verify":
            print(
                "public CLI release-target matrix: "
                f"OK ({len(value['targets'])} targets)"
            )
        elif args.command == "shell":
            print(shell_values(find_target(value, args.target)))
        else:
            print(
                "\n".join(
                    str(target[args.field]) for target in value["targets"]
                )
            )
    except (ContractError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
