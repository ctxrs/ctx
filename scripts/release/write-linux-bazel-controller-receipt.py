#!/usr/bin/env python3
"""Bind an authoritative Linux controller to one sealed release bundle."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import stat
from typing import Any


_BUNDLE_PATH = Path(__file__).with_name("release_bundle.py")
_BUNDLE_SPEC = importlib.util.spec_from_file_location(
    "ctx_linux_release_bundle_for_controller_receipt", _BUNDLE_PATH
)
if _BUNDLE_SPEC is None or _BUNDLE_SPEC.loader is None:
    raise RuntimeError("could not load release bundle authority")
_BUNDLE = importlib.util.module_from_spec(_BUNDLE_SPEC)
_BUNDLE_SPEC.loader.exec_module(_BUNDLE)
BundleError = _BUNDLE.BundleError


COMMIT = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"(?:sha256:)?[0-9a-f]{64}")
MAX_INPUT = 32 * 1024 * 1024


class ReceiptError(ValueError):
    pass


def regular_bytes(path: Path, label: str, maximum: int = MAX_INPUT) -> bytes:
    metadata = _BUNDLE._binding(path)
    if not stat.S_ISREG(metadata[2]):
        raise ReceiptError(f"{label} is not a regular non-symlink file")
    if metadata[3] <= 0 or metadata[3] > maximum:
        raise ReceiptError(f"{label} has an invalid size")
    value = path.read_bytes()
    if _BUNDLE._binding(path) != metadata:
        raise ReceiptError(f"{label} changed while read")
    return value


def digest(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def parse_evidence(value: str, label: str) -> dict[str, Any]:
    fields = value.split("\t")
    if len(fields) != 9 or fields[8] not in {"0", "1"}:
        raise ReceiptError(f"{label} evidence is malformed")
    return {
        "system": fields[0],
        "arch": fields[1],
        "native_arch": fields[2],
        "process_translated": fields[3],
        "native_arch_probe": fields[4],
        "hardware_identity": fields[5],
        "emulation": fields[6],
        "hypervisor": fields[7],
        "complete": fields[8] == "1",
    }


def parse_os(value: str, label: str) -> dict[str, str]:
    fields = value.split("\t")
    if len(fields) != 3:
        raise ReceiptError(f"{label} OS evidence is malformed")
    return {"identity": fields[0], "version": fields[1], "product_type": fields[2]}


def parse_daemon(value: str, label: str) -> dict[str, str]:
    fields = value.split("\t")
    if len(fields) != 3 or not all(fields):
        raise ReceiptError(f"{label} Docker daemon evidence is malformed")
    return {"arch": fields[0], "version": fields[1], "id": fields[2]}


def checked_sha(value: str, label: str) -> str:
    if SHA256.fullmatch(value) is None:
        raise ReceiptError(f"{label} is not a SHA-256 digest")
    return value


def socket_identity(args: argparse.Namespace, scope: str) -> dict[str, dict[str, str]]:
    before = {
        "device": getattr(args, f"{scope}_device_before"),
        "inode": getattr(args, f"{scope}_inode_before"),
        "mode": getattr(args, f"{scope}_mode_before"),
    }
    after = {
        "device": getattr(args, f"{scope}_device_after"),
        "inode": getattr(args, f"{scope}_inode_after"),
        "mode": getattr(args, f"{scope}_mode_after"),
    }
    for identity in (before, after):
        if (
            not identity["device"].isdecimal()
            or not identity["inode"].isdecimal()
            or re.fullmatch(r"[0-9a-f]+", identity["mode"]) is None
        ):
            raise ReceiptError("Docker Unix socket identity is malformed")
    if before != after:
        raise ReceiptError("Docker Unix socket authority changed during construction")
    return {"after": after, "before": before}


def bundle_identity(
    candidate: Path, platform: str, source_commit: str
) -> tuple[dict[str, Any], str]:
    payload = _BUNDLE.verify_bundle(candidate, platform, source_commit)
    marker_name = _BUNDLE.completion_leaf(platform)
    marker_path = candidate / marker_name
    marker_bytes = regular_bytes(marker_path, "release completion marker")
    marker_metadata = marker_path.lstat()
    records = [dict(record) for record in payload["files"]]
    records.append(
        {
            "mode": f"{stat.S_IMODE(marker_metadata.st_mode):04o}",
            "name": marker_name,
            "sha256": digest(marker_bytes),
            "size": len(marker_bytes),
        }
    )
    records.sort(key=lambda record: str(record["name"]))
    return {"leaves": records}, digest(marker_bytes)


def write_atomic(output: Path, payload: bytes) -> None:
    if not output.is_absolute():
        raise ReceiptError("controller receipt output must be absolute")
    parent = output.parent
    parent_descriptor = _BUNDLE._open_bound_directory(
        parent, "controller receipt parent"
    )
    temporary = f".ctx-controller-receipt.{secrets.token_hex(16)}"
    descriptor = -1
    try:
        _BUNDLE._require_bound_directory_identity(
            parent_descriptor, parent, "controller receipt parent"
        )
        if _BUNDLE._entry_binding(parent_descriptor, output.name) is not None:
            raise ReceiptError(f"controller receipt already exists: {output}")
        descriptor = os.open(
            temporary,
            os.O_WRONLY | os.O_CLOEXEC | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
            0o400,
            dir_fd=parent_descriptor,
        )
        offset = 0
        while offset < len(payload):
            written = os.write(descriptor, payload[offset:])
            if written == 0:
                raise OSError("short controller receipt write")
            offset += written
        os.fchmod(descriptor, 0o444)
        os.fsync(descriptor)
        os.link(
            temporary,
            _BUNDLE._valid_leaf_name(output.name, "controller receipt"),
            src_dir_fd=parent_descriptor,
            dst_dir_fd=parent_descriptor,
            follow_symlinks=False,
        )
        os.fsync(parent_descriptor)
        _BUNDLE._require_bound_directory_identity(
            parent_descriptor, parent, "controller receipt parent"
        )
    except FileExistsError as error:
        raise ReceiptError(f"controller receipt already exists: {output}") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        try:
            os.unlink(temporary, dir_fd=parent_descriptor)
        except FileNotFoundError:
            pass
        os.close(parent_descriptor)


def write_receipt(args: argparse.Namespace) -> None:
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
    daemon_before = parse_daemon(args.daemon_before, "initial")
    daemon_after = parse_daemon(args.daemon_after, "final")
    if daemon_before != daemon_after:
        raise ReceiptError("Docker daemon authority changed during construction")
    if (
        controller_evidence["system"] != "Linux"
        or controller_evidence["arch"] != expected_arch
        or controller_evidence["native_arch"] != expected_arch
        or controller_evidence["process_translated"] != "0"
        or controller_evidence["emulation"] != "none"
        or not controller_evidence["complete"]
        or daemon_before["arch"] != expected_arch
    ):
        raise ReceiptError("outer controller native authority is inconsistent")

    candidate_receipts, completion_sha256 = bundle_identity(
        args.candidate_dir, args.platform, args.source_commit
    )
    records = {record["name"]: record for record in candidate_receipts["leaves"]}
    binary = "ctx" if args.platform == "linux-x64" else "ctx-linux-aarch64"
    artifact = records[binary]
    candidate_receipts["completion_sha256"] = completion_sha256
    document = {
        "artifact": {
            "path": binary,
            "sha256": artifact["sha256"],
            "size": artifact["size"],
        },
        "candidate_receipts": candidate_receipts,
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
            "recipe_sha256": digest(
                regular_bytes(args.controller_recipe, "controller recipe")
            ),
            "zstd": {
                "sha256": checked_sha(args.zstd_sha256, "zstd digest"),
                "version": args.zstd_version,
            },
        },
        "docker": {
            "daemon": {"after": daemon_after, "before": daemon_before},
            "socket": {
                "controller": socket_identity(args, "controller_socket"),
                "launcher": socket_identity(args, "socket"),
            },
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
    expected_bundle_identity = (
        {"leaves": candidate_receipts["leaves"]},
        completion_sha256,
    )
    if bundle_identity(
        args.candidate_dir, args.platform, args.source_commit
    ) != expected_bundle_identity:
        raise ReceiptError("release bundle changed while writing controller receipt")
    encoded = (json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n").encode()
    write_atomic(args.output, encoded)


def parser() -> argparse.ArgumentParser:
    value = argparse.ArgumentParser()
    value.add_argument("--buildx-sha256", required=True)
    value.add_argument("--buildx-version", required=True)
    value.add_argument("--candidate-dir", type=Path, required=True)
    value.add_argument("--controller-authority", required=True)
    value.add_argument("--controller-base-image", required=True)
    value.add_argument("--controller-evidence", required=True)
    value.add_argument("--controller-image-id", required=True)
    value.add_argument("--controller-os", required=True)
    value.add_argument("--controller-recipe", type=Path, required=True)
    for scope in ("controller-socket", "socket"):
        for field in ("device", "inode", "mode"):
            for phase in ("after", "before"):
                value.add_argument(f"--{scope}-{field}-{phase}", required=True)
    value.add_argument("--daemon-after", required=True)
    value.add_argument("--daemon-before", required=True)
    value.add_argument("--docker-client-sha256", required=True)
    value.add_argument("--docker-client-version", required=True)
    value.add_argument("--launcher-authority", required=True)
    value.add_argument("--launcher-evidence", required=True)
    value.add_argument("--launcher-os", required=True)
    value.add_argument("--output", type=Path, required=True)
    value.add_argument(
        "--platform", choices=("linux-x64", "linux-aarch64"), required=True
    )
    value.add_argument("--source-commit", required=True)
    value.add_argument("--source-tree", required=True)
    value.add_argument("--zstd-sha256", required=True)
    value.add_argument("--zstd-version", required=True)
    return value


def main() -> int:
    write_receipt(parser().parse_args())
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, BundleError, ReceiptError) as error:
        raise SystemExit(f"error: {error}") from error
