#!/usr/bin/env python3
"""Create or verify the small native execution proof used by release staging."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import stat


PLATFORMS = {
    "linux-x64",
    "linux-aarch64",
    "macos-arm64",
    "macos-x64",
    "windows-x64",
}
REQUIRED_SMOKE_STEPS = frozenset(
    {
        "version",
        "setup",
        "import",
        "search",
        "read_only",
        "released_defaults",
        "explicit_opt_outs",
        "semantic_offline_fail_closed",
    }
)


def regular(path: Path, label: str, maximum: int) -> bytes:
    metadata = path.lstat()
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise ValueError(f"{label} must be a regular non-symlink file")
    if metadata.st_size <= 0 or metadata.st_size > maximum:
        raise ValueError(f"{label} has an invalid size")
    return path.read_bytes()


def digest(path: Path, label: str, maximum: int = 256 * 1024 * 1024) -> str:
    return hashlib.sha256(regular(path, label, maximum)).hexdigest()


def load_passed_smoke(path: Path) -> tuple[bytes, str]:
    payload = regular(path, "native smoke result", 64 * 1024)
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("native smoke result is malformed") from error
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != 1
        or value.get("kind") != "ctx-native-candidate-smoke"
        or value.get("status") != "passed"
    ):
        raise ValueError("native smoke result is not passed")
    steps = value.get("steps")
    if not isinstance(steps, dict) or any(
        steps.get(step) != "passed" for step in REQUIRED_SMOKE_STEPS
    ):
        raise ValueError("native smoke result is missing required passed steps")
    return payload, hashlib.sha256(payload).hexdigest()


def create(platform: str, artifact: Path, smoke: Path, output: Path) -> None:
    if platform not in PLATFORMS:
        raise ValueError(f"unsupported native proof platform: {platform}")
    smoke_bytes, smoke_sha256 = load_passed_smoke(smoke)
    del smoke_bytes
    document = {
        "schema_version": 1,
        "kind": "ctx-native-execution-proof",
        "platform": platform,
        "status": "passed",
        "validator_authority": f"native-{platform}",
        "artifact_sha256": digest(artifact, "native candidate artifact"),
        "smoke_result_sha256": smoke_sha256,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.tmp")
    temporary.write_text(
        json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    temporary.replace(output)


def verify(platform: str, artifact: Path, proof: Path) -> None:
    if platform not in PLATFORMS:
        raise ValueError(f"unsupported native proof platform: {platform}")
    payload = regular(proof, "native execution proof", 64 * 1024)
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("native execution proof is malformed") from error
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != 1
        or value.get("kind") != "ctx-native-execution-proof"
        or value.get("platform") != platform
        or value.get("status") != "passed"
        or value.get("validator_authority") != f"native-{platform}"
        or not re.fullmatch(r"[0-9a-f]{64}", str(value.get("artifact_sha256", "")))
        or not re.fullmatch(r"[0-9a-f]{64}", str(value.get("smoke_result_sha256", "")))
    ):
        raise ValueError("native execution proof is invalid")
    actual = digest(artifact, "staged native candidate artifact")
    if value["artifact_sha256"] != actual:
        raise ValueError(
            f"native execution proof is for different artifact: expected {actual}, "
            f"got {value['artifact_sha256']}"
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    create_parser = subparsers.add_parser("create")
    create_parser.add_argument("--platform", required=True)
    create_parser.add_argument("--artifact", required=True, type=Path)
    create_parser.add_argument("--smoke-result", required=True, type=Path)
    create_parser.add_argument("--output", required=True, type=Path)
    verify_parser = subparsers.add_parser("verify")
    verify_parser.add_argument("--platform", required=True)
    verify_parser.add_argument("--artifact", required=True, type=Path)
    verify_parser.add_argument("--proof", required=True, type=Path)
    args = parser.parse_args()
    try:
        if args.command == "create":
            create(args.platform, args.artifact, args.smoke_result, args.output)
        else:
            verify(args.platform, args.artifact, args.proof)
    except (OSError, ValueError) as error:
        raise SystemExit(f"error: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
