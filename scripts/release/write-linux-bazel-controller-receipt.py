#!/usr/bin/env python3
"""Write a descriptor-pinned receipt for an outer Linux release controller."""

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
from typing import Any, Callable


_PUBLISHER_PATH = Path(__file__).with_name("publish-linux-bazel-release.py")
_PUBLISHER_SPEC = importlib.util.spec_from_file_location(
    "ctx_linux_release_publisher_for_receipt", _PUBLISHER_PATH
)
if _PUBLISHER_SPEC is None or _PUBLISHER_SPEC.loader is None:
    raise RuntimeError("could not load completed candidate snapshot authority")
_PUBLISHER = importlib.util.module_from_spec(_PUBLISHER_SPEC)
_PUBLISHER_SPEC.loader.exec_module(_PUBLISHER)
CompletedCandidateSnapshot = _PUBLISHER.CompletedCandidateSnapshot
PublicationError = _PUBLISHER.PublicationError
completion_leaf = _PUBLISHER.completion_leaf
release_binary_leaf = _PUBLISHER.release_binary_leaf
open_directory = _PUBLISHER._open_directory
verify_directory_binding = _PUBLISHER._verify_directory_binding


COMMIT = re.compile(r"[0-9a-f]{40}")
SHA256 = re.compile(r"(?:sha256:)?[0-9a-f]{64}")
MAX_JSON = 32 * 1024 * 1024
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


def descriptor_bytes(
    snapshot: CompletedCandidateSnapshot, name: str, maximum: int = MAX_INPUT
) -> bytes:
    record = snapshot.records[name]
    size = int(record["size"])
    if size <= 0 or size > maximum:
        raise ReceiptError(f"completed candidate leaf has an invalid size: {name}")
    descriptor = snapshot.descriptors[name]
    chunks: list[bytes] = []
    offset = 0
    while offset < size:
        chunk = os.pread(descriptor, min(1024 * 1024, size - offset), offset)
        if not chunk:
            raise ReceiptError(f"completed candidate leaf ended early: {name}")
        chunks.append(chunk)
        offset += len(chunk)
    value = b"".join(chunks)
    if digest(value) != record["sha256"]:
        raise ReceiptError(f"completed candidate leaf changed while read: {name}")
    return value


def json_leaf(
    snapshot: CompletedCandidateSnapshot, name: str
) -> dict[str, Any]:
    try:
        value = json.loads(descriptor_bytes(snapshot, name, MAX_JSON))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ReceiptError(f"completed candidate JSON is malformed: {name}") from error
    if not isinstance(value, dict):
        raise ReceiptError(f"completed candidate JSON is malformed: {name}")
    return value


def exact_checksum(
    snapshot: CompletedCandidateSnapshot, checksum_name: str, target_name: str
) -> None:
    expected = f'{snapshot.records[target_name]["sha256"]}\n'.encode("ascii")
    if descriptor_bytes(snapshot, checksum_name, 256) != expected:
        raise ReceiptError(
            f"completed candidate checksum does not bind {target_name}"
        )


def evidence_binding(
    candidate: dict[str, Any],
    key: str,
    name: str,
    snapshot: CompletedCandidateSnapshot,
) -> None:
    evidence = candidate.get("evidence")
    observed = evidence.get(key) if isinstance(evidence, dict) else None
    expected = {"file": name, "sha256": snapshot.records[name]["sha256"]}
    if observed != expected:
        raise ReceiptError(f"candidate manifest does not bind {name}")


def validate_candidate_relationships(
    snapshot: CompletedCandidateSnapshot, platform: str, source_commit: str
) -> tuple[str, dict[str, Any]]:
    binary = release_binary_leaf(platform)
    target_id = "linux-arm64" if platform == "linux-aarch64" else platform
    rust_triple = (
        "aarch64-unknown-linux-gnu"
        if platform == "linux-aarch64"
        else "x86_64-unknown-linux-gnu"
    )
    runtime = f"ctx-onnxruntime-{platform}"
    artifact_record = snapshot.records[binary]
    artifact_sha256 = str(artifact_record["sha256"])
    artifact_size = int(artifact_record["size"])

    build_name = f"{binary}.build-info.json"
    candidate_name = f"{binary}.candidate.json"
    size_name = f"{binary}.size.json"
    sbom_name = f"{binary}.cdx.json"
    notices_name = f"{binary}.third-party-notices.txt"
    advisory_name = f"{binary}.dependency-advisory.json"
    build_info = json_leaf(snapshot, build_name)
    source = {"clean": True, "commit": source_commit}
    if (
        build_info.get("artifact_sha256") != artifact_sha256
        or build_info.get("platform") != platform
        or build_info.get("source") != source
        or build_info.get("target") != rust_triple
        or not isinstance(build_info.get("release_version"), str)
    ):
        raise ReceiptError("build-info does not bind the completed artifact identity")
    version = str(build_info["release_version"])

    candidate = json_leaf(snapshot, candidate_name)
    if (
        candidate.get("artifact")
        != {"file": binary, "sha256": artifact_sha256, "size_bytes": artifact_size}
        or candidate.get("source") != source
        or candidate.get("construction")
        != {
            "authority": "bazel-release-route-v1",
            "label": f"//:ctx_release_{target_id.replace('-', '_')}",
        }
    ):
        raise ReceiptError("candidate manifest does not bind the completed artifact")
    for key, name in (
        ("build_info", build_name),
        ("binary_size_report", size_name),
        ("cyclonedx_sbom", sbom_name),
        ("third_party_notices", notices_name),
    ):
        evidence_binding(candidate, key, name, snapshot)

    size_report = json_leaf(snapshot, size_name)
    if (
        size_report.get("artifact")
        != {"file": binary, "sha256": artifact_sha256, "size_bytes": artifact_size}
        or size_report.get("target")
        != {"id": target_id, "platform": platform, "rust_triple": rust_triple}
        or size_report.get("version") != version
    ):
        raise ReceiptError("size report does not bind the completed artifact")

    advisory = json_leaf(snapshot, advisory_name)
    if (
        advisory.get("source") != {"commit": source_commit, "dirty": False}
        or advisory.get("target_id") != target_id
        or advisory.get("status") != "clean"
    ):
        raise ReceiptError("dependency advisory does not bind the completed source")

    sbom = json_leaf(snapshot, sbom_name)
    metadata = sbom.get("metadata")
    component = metadata.get("component") if isinstance(metadata, dict) else None
    properties = metadata.get("properties") if isinstance(metadata, dict) else None
    property_map = {
        item.get("name"): item.get("value")
        for item in properties or []
        if isinstance(item, dict)
    }
    if (
        not isinstance(component, dict)
        or component.get("bom-ref") != f"urn:ctx:artifact:sha256:{artifact_sha256}"
        or component.get("hashes")
        != [{"alg": "SHA-256", "content": artifact_sha256}]
        or component.get("version") != version
        or property_map.get("ctx:build-info:sha256")
        != snapshot.records[build_name]["sha256"]
        or property_map.get("ctx:construction:authority")
        != "bazel-release-route-v1"
        or property_map.get("ctx:construction:label")
        != f"//:ctx_release_{target_id.replace('-', '_')}"
        or property_map.get("ctx:platform") != platform
        or property_map.get("ctx:source:public-commit") != source_commit
        or property_map.get("ctx:target") != rust_triple
        or property_map.get("ctx:target-id") != target_id
    ):
        raise ReceiptError("CycloneDX evidence does not bind the completed artifact")

    if descriptor_bytes(snapshot, f"{binary}.version", 1024) != (
        f"ctx {version}\n".encode("utf-8")
    ):
        raise ReceiptError("version leaf does not bind the completed artifact")
    notices = descriptor_bytes(snapshot, notices_name, MAX_JSON)
    if f"artifact_sha256: {artifact_sha256}\n".encode("ascii") not in notices:
        raise ReceiptError("third-party notices do not bind the completed artifact")

    for checksum_name, target_name in (
        (f"{binary}.sha256", binary),
        (f"{binary}.cdx.json.sha256", sbom_name),
        (f"{binary}.third-party-notices.txt.sha256", notices_name),
        (f"{runtime}.tar.gz.sha256", f"{runtime}.tar.gz"),
        (f"{runtime}.tar.zst.sha256", f"{runtime}.tar.zst"),
    ):
        exact_checksum(snapshot, checksum_name, target_name)

    asset_name = f"{runtime}.tar.zst.asset.json"
    asset = json_leaf(snapshot, asset_name)
    asset_record = asset.get("asset")
    expected_asset_id = (
        "linux_aarch64_cpu" if platform == "linux-aarch64" else "linux_x64_cpu"
    )
    if (
        asset.get("id") != expected_asset_id
        or not isinstance(asset_record, dict)
        or asset_record.get("artifact") != f"{runtime}.tar.zst"
        or asset_record.get("archive_sha256")
        != snapshot.records[f"{runtime}.tar.zst"]["sha256"]
        or asset_record.get("platform") != platform
    ):
        raise ReceiptError("runtime asset metadata does not bind its completed archive")
    snapshot.revalidate()
    return binary, build_info


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


def write_receipt(
    args: argparse.Namespace,
    phase_hook: Callable[[str, CompletedCandidateSnapshot], None] | None = None,
) -> None:
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
    socket_before = {
        "device": args.socket_device_before,
        "inode": args.socket_inode_before,
        "mode": args.socket_mode_before,
    }
    socket_after = {
        "device": args.socket_device_after,
        "inode": args.socket_inode_after,
        "mode": args.socket_mode_after,
    }
    controller_socket_before = {
        "device": args.controller_socket_device_before,
        "inode": args.controller_socket_inode_before,
        "mode": args.controller_socket_mode_before,
    }
    controller_socket_after = {
        "device": args.controller_socket_device_after,
        "inode": args.controller_socket_inode_after,
        "mode": args.controller_socket_mode_after,
    }
    for socket_identity in (
        socket_before,
        socket_after,
        controller_socket_before,
        controller_socket_after,
    ):
        if (
            not str(socket_identity["device"]).isdecimal()
            or not str(socket_identity["inode"]).isdecimal()
            or re.fullmatch(r"[0-9a-f]+", str(socket_identity["mode"])) is None
        ):
            raise ReceiptError("Docker Unix socket identity is malformed")
    if (
        socket_before != socket_after
        or controller_socket_before != controller_socket_after
    ):
        raise ReceiptError("Docker Unix socket authority changed during construction")

    snapshot = CompletedCandidateSnapshot.open(
        args.candidate_dir,
        [args.platform],
        args.source_commit,
        allow_extra=False,
    )
    try:
        binary, _ = validate_candidate_relationships(
            snapshot, args.platform, args.source_commit
        )
        if phase_hook is not None:
            phase_hook("snapshot_verified", snapshot)
        marker = completion_leaf(args.platform)
        document = {
            "artifact": {
                "path": binary,
                "sha256": snapshot.records[binary]["sha256"],
                "size": snapshot.records[binary]["size"],
            },
            "candidate_receipts": {
                "completion_sha256": snapshot.records[marker]["sha256"],
                "leaves": [snapshot.records[name] for name in sorted(snapshot.records)],
            },
            "controller": {
                "authority": args.controller_authority,
                "base_image": args.controller_base_image,
                "buildx": {
                    "sha256": checked_sha(args.buildx_sha256, "Buildx digest"),
                    "version": args.buildx_version,
                },
                "docker_client": {
                    "sha256": checked_sha(
                        args.docker_client_sha256, "Docker client digest"
                    ),
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
                    "controller": {
                        "after": controller_socket_after,
                        "before": controller_socket_before,
                    },
                    "launcher": {"after": socket_after, "before": socket_before},
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
        payload = (
            json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
        ).encode("utf-8")
        output_parent = open_directory(args.output.parent, create=False)
        temporary_name = f".ctx-controller-receipt.{secrets.token_hex(16)}"
        temporary_descriptor = -1
        try:
            if args.output.exists() or args.output.is_symlink():
                raise ReceiptError(f"controller receipt already exists: {args.output}")
            temporary_descriptor = os.open(
                temporary_name,
                os.O_WRONLY | os.O_CLOEXEC | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                0o400,
                dir_fd=output_parent,
            )
            offset = 0
            while offset < len(payload):
                written = os.write(temporary_descriptor, payload[offset:])
                if written == 0:
                    raise OSError("short controller receipt write")
                offset += written
            os.fchmod(temporary_descriptor, 0o444)
            os.fsync(temporary_descriptor)
            snapshot.revalidate()
            if phase_hook is not None:
                phase_hook("before_publish", snapshot)
            snapshot.revalidate()
            os.link(
                temporary_name,
                args.output.name,
                src_dir_fd=output_parent,
                dst_dir_fd=output_parent,
                follow_symlinks=False,
            )
            os.fsync(output_parent)
            if phase_hook is not None:
                phase_hook("after_publish", snapshot)
            snapshot.revalidate()
            verify_directory_binding(
                args.output.parent, output_parent, "controller receipt parent"
            )
        finally:
            if temporary_descriptor >= 0:
                os.close(temporary_descriptor)
            try:
                os.unlink(temporary_name, dir_fd=output_parent)
            except FileNotFoundError:
                pass
            os.close(output_parent)
    finally:
        snapshot.close()


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
    value.add_argument("--controller-socket-device-after", required=True)
    value.add_argument("--controller-socket-device-before", required=True)
    value.add_argument("--controller-socket-inode-after", required=True)
    value.add_argument("--controller-socket-inode-before", required=True)
    value.add_argument("--controller-socket-mode-after", required=True)
    value.add_argument("--controller-socket-mode-before", required=True)
    value.add_argument("--daemon-after", required=True)
    value.add_argument("--daemon-before", required=True)
    value.add_argument("--docker-client-sha256", required=True)
    value.add_argument("--docker-client-version", required=True)
    value.add_argument("--launcher-authority", required=True)
    value.add_argument("--launcher-evidence", required=True)
    value.add_argument("--launcher-os", required=True)
    value.add_argument("--output", type=Path, required=True)
    value.add_argument("--platform", choices=("linux-x64", "linux-aarch64"), required=True)
    value.add_argument("--socket-device-after", required=True)
    value.add_argument("--socket-device-before", required=True)
    value.add_argument("--socket-inode-after", required=True)
    value.add_argument("--socket-inode-before", required=True)
    value.add_argument("--socket-mode-after", required=True)
    value.add_argument("--socket-mode-before", required=True)
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
    except (OSError, PublicationError, ReceiptError) as error:
        raise SystemExit(f"error: {error}") from error
