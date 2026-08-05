#!/usr/bin/env python3
"""Write an exact receipt for the outer Linux release controller."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
from pathlib import Path
from typing import Any


COMMIT = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"(?:sha256:)?[0-9a-f]{64}")
MAX_INPUT = 256 * 1024 * 1024


class ReceiptError(ValueError):
    pass


def regular_bytes(path: Path, label: str, maximum: int = MAX_INPUT) -> bytes:
    metadata = path.lstat()
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise ReceiptError(f"{label} is not a regular non-symlink file")
    if metadata.st_size <= 0 or metadata.st_size > maximum:
        raise ReceiptError(f"{label} has an invalid size")
    return path.read_bytes()


def digest(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def parse_evidence(value: str, label: str) -> dict[str, Any]:
    fields = value.split("\t")
    if len(fields) != 9:
        raise ReceiptError(f"{label} evidence is malformed")
    complete = fields[8]
    if complete not in {"0", "1"}:
        raise ReceiptError(f"{label} evidence completeness is malformed")
    return {
        "system": fields[0],
        "arch": fields[1],
        "native_arch": fields[2],
        "process_translated": fields[3],
        "native_arch_probe": fields[4],
        "hardware_identity": fields[5],
        "emulation": fields[6],
        "hypervisor": fields[7],
        "complete": complete == "1",
    }


def parse_os(value: str, label: str) -> dict[str, str]:
    fields = value.split("\t")
    if len(fields) != 3:
        raise ReceiptError(f"{label} OS evidence is malformed")
    return {"identity": fields[0], "version": fields[1], "product_type": fields[2]}


def checked_sha(value: str, label: str) -> str:
    if SHA256.fullmatch(value) is None:
        raise ReceiptError(f"{label} is not a SHA-256 digest")
    return value


def load_json(path: Path, label: str) -> tuple[dict[str, Any], bytes]:
    raw = regular_bytes(path, label)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ReceiptError(f"{label} is malformed") from error
    if not isinstance(value, dict):
        raise ReceiptError(f"{label} is malformed")
    return value, raw


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--build-info", type=Path, required=True)
    parser.add_argument("--buildx-sha256", required=True)
    parser.add_argument("--buildx-version", required=True)
    parser.add_argument("--completion", type=Path, required=True)
    parser.add_argument("--controller-authority", required=True)
    parser.add_argument("--controller-base-image", required=True)
    parser.add_argument("--controller-evidence", required=True)
    parser.add_argument("--controller-image-id", required=True)
    parser.add_argument("--controller-os", required=True)
    parser.add_argument("--controller-recipe", type=Path, required=True)
    parser.add_argument("--daemon-arch", required=True)
    parser.add_argument("--daemon-id", required=True)
    parser.add_argument("--daemon-version", required=True)
    parser.add_argument("--docker-client-sha256", required=True)
    parser.add_argument("--docker-client-version", required=True)
    parser.add_argument("--launcher-authority", required=True)
    parser.add_argument("--launcher-evidence", required=True)
    parser.add_argument("--launcher-os", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--platform", choices=("linux-x64", "linux-aarch64"), required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-tree", required=True)
    parser.add_argument("--zstd-sha256", required=True)
    parser.add_argument("--zstd-version", required=True)
    args = parser.parse_args()

    if COMMIT.fullmatch(args.source_commit) is None or args.source_commit == "0" * 40:
        raise ReceiptError("source commit is malformed")
    if COMMIT.fullmatch(args.source_tree) is None:
        raise ReceiptError("source tree is malformed")
    if args.controller_authority != "authoritative":
        raise ReceiptError("outer controller is not authoritative")
    if args.launcher_authority not in {"authoritative", "non_authoritative"}:
        raise ReceiptError("launcher authority is malformed")

    controller_os = parse_os(args.controller_os, "controller")
    if controller_os["identity"] != "ubuntu" or controller_os["version"] != "22.04":
        raise ReceiptError("outer controller is not Ubuntu 22.04")
    controller_evidence = parse_evidence(args.controller_evidence, "controller")
    expected_arch = "x86_64" if args.platform == "linux-x64" else "aarch64"
    if (
        controller_evidence["system"] != "Linux"
        or controller_evidence["arch"] != expected_arch
        or controller_evidence["native_arch"] != expected_arch
        or controller_evidence["process_translated"] != "0"
        or controller_evidence["emulation"] != "none"
        or not controller_evidence["complete"]
        or args.daemon_arch != expected_arch
    ):
        raise ReceiptError("outer controller native authority is inconsistent")

    build_info, build_info_raw = load_json(args.build_info, "build-info")
    completion, completion_raw = load_json(args.completion, "completion marker")
    source = build_info.get("source")
    if (
        build_info.get("platform") != args.platform
        or not isinstance(source, dict)
        or source.get("commit") != args.source_commit
        or source.get("clean") is not True
        or completion.get("platform") != args.platform
        or completion.get("source_commit") != args.source_commit
    ):
        raise ReceiptError("candidate receipts do not bind the requested source and platform")

    document = {
        "artifact": {
            "path": args.artifact.name,
            "sha256": digest(regular_bytes(args.artifact, "artifact")),
        },
        "candidate_receipts": {
            "build_info_sha256": digest(build_info_raw),
            "completion_sha256": digest(completion_raw),
        },
        "controller": {
            "authority": args.controller_authority,
            "base_image": args.controller_base_image,
            "buildx": {
                "sha256": checked_sha(args.buildx_sha256, "Buildx digest"),
                "version": args.buildx_version,
            },
            "docker_client": {
                "sha256": checked_sha(args.docker_client_sha256, "Docker client digest"),
                "version": args.docker_client_version,
            },
            "evidence": controller_evidence,
            "image_id": checked_sha(args.controller_image_id, "controller image ID"),
            "os": controller_os,
            "recipe_sha256": digest(regular_bytes(args.controller_recipe, "controller recipe")),
            "zstd": {
                "sha256": checked_sha(args.zstd_sha256, "zstd digest"),
                "version": args.zstd_version,
            },
        },
        "docker_daemon": {
            "arch": args.daemon_arch,
            "id": args.daemon_id,
            "version": args.daemon_version,
        },
        "launcher": {
            "authority": args.launcher_authority,
            "evidence": parse_evidence(args.launcher_evidence, "launcher"),
            "os": parse_os(args.launcher_os, "launcher"),
        },
        "platform": args.platform,
        "schema_version": 1,
        "source": {
            "clean": True,
            "commit": args.source_commit,
            "tree": args.source_tree,
        },
    }
    output = args.output
    if output.exists() or output.is_symlink():
        raise ReceiptError(f"controller receipt already exists: {output}")
    payload = (json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n").encode()
    descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o444)
    with os.fdopen(descriptor, "wb") as destination:
        destination.write(payload)
        destination.flush()
        os.fsync(destination.fileno())
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ReceiptError) as error:
        raise SystemExit(f"error: {error}") from error
