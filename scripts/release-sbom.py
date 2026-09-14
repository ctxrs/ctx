#!/usr/bin/env python3
"""Generate and verify exact-byte public CLI release evidence bundles."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import stat
import sys
from typing import Any

_SCRIPT_DIRECTORY = os.fspath(Path(__file__).resolve().parent)
sys.path.insert(0, _SCRIPT_DIRECTORY)
try:
    from release_sbom.dependency_materials import (
        BUILD_INFO_CLASSIFICATION,
        HEX_40,
        HEX_64,
        Identity,
        TANTIVY_FEATURES,
        TANTIVY_RESOLVED_FEATURES,
        TANTIVY_VERSION,
        VERSION,
        assert_tantivy_contract,
        canonical,
        cargo_materials,
        material_ref,
        package_identity,
        package_metadata,
        parse_cargo_lock,
        properties,
        regular_bytes,
        selected_adjacency,
        sha256_bytes,
        sha256_file,
        tantivy_closure,
        target_package_identities,
    )
    from release_sbom.generation import build_bundle, load_core_build_info, target_contract
finally:
    del sys.path[0]
del _SCRIPT_DIRECTORY


RELEASE_SUMS = "SHA256SUMS"
HANDOFF_DOCUMENT = "ctx-core-github-handoff.json"
FACTORY_MANIFEST = "ctx-release-factory.json"
FACTORY_COMPLETION = "ctx-core.release-complete.json"
SUM_LINE = re.compile(r"([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._-]{0,127})")
MAX_RELEASE_SUMS_BYTES = 64 * 1024
MAX_HANDOFF_JSON_BYTES = 1024 * 1024
MAX_FACTORY_JSON_BYTES = 16 * 1024 * 1024
MAX_COMPLETION_JSON_BYTES = 16 * 1024 * 1024
MAX_CANDIDATE_JSON_BYTES = 16 * 1024 * 1024
MAX_HANDOFF_LEAF_BYTES = 256 * 1024 * 1024

# Candidate order is part of the canonical handoff schema emitted by
# stage-github-release-assets.sh.
CORE_TARGETS = (
    (
        "linux-arm64",
        "linux-aarch64",
        "aarch64-unknown-linux-gnu",
        "ctx-linux-aarch64",
    ),
    ("linux-x64", "linux-x64", "x86_64-unknown-linux-gnu", "ctx"),
    ("macos-arm64", "macos-arm64", "aarch64-apple-darwin", "ctx-macos-arm64"),
    ("macos-x64", "macos-x64", "x86_64-apple-darwin", "ctx-macos-x64"),
    ("windows-x64", "windows-x64", "x86_64-pc-windows-gnu", "ctx.exe"),
)
CORE_CANDIDATE_MANIFESTS = tuple(
    f"{binary}.candidate.json" for _, _, _, binary in CORE_TARGETS
)
CORE_RELEASE_SOURCES = (
    ("ctx-linux-x64", "ctx"),
    ("ctx-linux-aarch64", "ctx-linux-aarch64"),
    ("ctx-macos-arm64", "ctx-macos-arm64"),
    ("ctx-macos-x64", "ctx-macos-x64"),
    ("ctx-windows-x64.exe", "ctx.exe"),
)
CORE_RELEASE_BINDINGS = tuple(
    (release_name + suffix, source_name + suffix)
    for release_name, source_name in CORE_RELEASE_SOURCES
    for suffix in ("", ".cdx.json", ".third-party-notices.txt")
)
CORE_RELEASE_ASSETS = tuple(name for name, _ in CORE_RELEASE_BINDINGS)
CORE_FACTORY_SUFFIXES = (
    "",
    ".build-info.json",
    ".candidate.json",
    ".cdx.json",
    ".cdx.json.sha256",
    ".dependency-advisory.json",
    ".sha256",
    ".size.json",
    ".third-party-notices.txt",
    ".third-party-notices.txt.sha256",
    ".version",
)
CORE_FACTORY_LEAVES = tuple(
    sorted(
        f"{binary}{suffix}"
        for _, _, _, binary in CORE_TARGETS
        for suffix in CORE_FACTORY_SUFFIXES
    )
)
CORE_COMPLETION_LEAVES = tuple(sorted((FACTORY_MANIFEST, *CORE_FACTORY_LEAVES)))
WINDOWS_HANDOFF_LEAVES = (
    "ctx.exe",
    "ctx.exe.build-info.json",
    "ctx.exe.cdx.json",
    "ctx.exe.size.json",
    "ctx.exe.third-party-notices.txt",
)
RELEASE_AUTHORITY_HANDOFF_LEAVES = tuple(
    sorted(
        (
            RELEASE_SUMS,
            HANDOFF_DOCUMENT,
            f"{HANDOFF_DOCUMENT}.sha256",
            FACTORY_COMPLETION,
            FACTORY_MANIFEST,
            *WINDOWS_HANDOFF_LEAVES,
            *CORE_CANDIDATE_MANIFESTS,
            *(f"{name}.sha256" for name in CORE_CANDIDATE_MANIFESTS),
        )
    )
)
CANDIDATE_FIELDS = {
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
CANDIDATE_EVIDENCE_FIELDS = {
    "binary_size_report",
    "build_info",
    "candidate_schema",
    "cargo_lock",
    "ctx_history_index_manifest",
    "ctx_history_index_format_manifest",
    "ctx_history_index_query_manifest",
    "cyclonedx_sbom",
    "license_materials_inventory",
    "module_file",
    "module_lock",
    "target_dependency_inventory",
    "target_matrix",
    "third_party_notices",
    "workspace_manifest",
}


def release_sums_record(path: Path) -> tuple[dict[str, str], dict[str, object]]:
    if path.name != RELEASE_SUMS:
        raise ValueError(f"release checksum manifest must be named {RELEASE_SUMS}")
    payload = regular_bytes(path, "release SHA256SUMS", MAX_RELEASE_SUMS_BYTES)
    if not payload.endswith(b"\n") or b"\r" in payload or b"\x00" in payload:
        raise ValueError("release SHA256SUMS is not canonical lowercase SHA-256 text")
    try:
        lines = payload.decode("ascii").splitlines()
    except UnicodeDecodeError as error:
        raise ValueError("release SHA256SUMS is not ASCII") from error
    entries: dict[str, str] = {}
    for index, line in enumerate(lines, 1):
        match = SUM_LINE.fullmatch(line)
        if match is None:
            raise ValueError(f"release SHA256SUMS line {index} is malformed")
        digest, name = match.groups()
        if name in entries:
            raise ValueError(f"release SHA256SUMS repeats {name}")
        entries[name] = digest
    names = tuple(entries)
    if names != CORE_RELEASE_ASSETS:
        raise ValueError(
            "Core SHA256SUMS does not have the exact canonical 15-entry "
            "inventory and order"
        )
    return entries, {
        "file": RELEASE_SUMS,
        "sha256": sha256_bytes(payload),
        "size_bytes": len(payload),
    }


def atomic_write(path: Path, payload: bytes) -> None:
    temporary = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    try:
        temporary.write_bytes(payload)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def release_handoff_binding(path: Path) -> tuple[object, ...]:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise ValueError(f"release authority handoff is unavailable: {path}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise ValueError(f"release authority handoff is not a directory: {path}")
    try:
        names = tuple(sorted(entry.name for entry in path.iterdir()))
    except OSError as error:
        raise ValueError(f"release authority handoff is unavailable: {path}") from error
    if names != RELEASE_AUTHORITY_HANDOFF_LEAVES:
        raise ValueError(
            "release authority handoff does not have the exact production inventory"
        )
    leaves: list[tuple[object, ...]] = []
    for name in names:
        leaf = path / name
        try:
            leaf_metadata = leaf.lstat()
        except OSError as error:
            raise ValueError(f"release authority leaf is unavailable: {leaf}") from error
        if (
            stat.S_ISLNK(leaf_metadata.st_mode)
            or not stat.S_ISREG(leaf_metadata.st_mode)
            or leaf_metadata.st_size <= 0
            or leaf_metadata.st_size > MAX_HANDOFF_LEAF_BYTES
        ):
            raise ValueError(f"release authority leaf is invalid: {leaf}")
        leaves.append(
            (
                name,
                leaf_metadata.st_dev,
                leaf_metadata.st_ino,
                leaf_metadata.st_mode,
                leaf_metadata.st_size,
                leaf_metadata.st_mtime_ns,
                leaf_metadata.st_ctime_ns,
            )
        )
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
        names,
        tuple(leaves),
    )


def digest_sidecar(path: Path, label: str) -> str:
    payload = regular_bytes(path, label, 128)
    try:
        value = payload.decode("ascii")
    except UnicodeDecodeError as error:
        raise ValueError(f"{label} is not ASCII") from error
    if not value.endswith("\n") or value.count("\n") != 1:
        raise ValueError(f"{label} is not canonical SHA-256 text")
    digest = value[:-1]
    if HEX_64.fullmatch(digest) is None or digest == "0" * 64:
        raise ValueError(f"{label} is not canonical SHA-256 text")
    return digest


def actual_record(
    path: Path, label: str, maximum: int = MAX_HANDOFF_LEAF_BYTES
) -> tuple[dict[str, object], bytes]:
    payload = regular_bytes(path, label, maximum)
    return {
        "file": path.name,
        "sha256": sha256_bytes(payload),
        "size_bytes": len(payload),
    }, payload


def require_document_record(
    value: object,
    expected: dict[str, object],
    label: str,
) -> None:
    if value != expected:
        raise ValueError(f"Core handoff does not bind the exact {label}")


def factory_manifest_records(
    value: dict[str, Any], source_commit: str
) -> dict[str, dict[str, object]]:
    target_ids = [target_id for target_id, _, _, _ in CORE_TARGETS]
    if (
        set(value)
        != {
            "files",
            "kind",
            "releasable",
            "runtime_sidecars_included",
            "schema_version",
            "selected_targets",
            "source_commit",
            "version",
        }
        or value.get("kind") != "ctx-linux-release-factory"
        or value.get("schema_version") != 1
        or value.get("source_commit") != source_commit
        or value.get("selected_targets") != target_ids
        or value.get("releasable") is not True
        or value.get("runtime_sidecars_included") is not False
        or not isinstance(value.get("version"), str)
        or VERSION.fullmatch(value["version"]) is None
    ):
        raise ValueError("Core factory manifest identity is malformed")
    files = value.get("files")
    if not isinstance(files, list):
        raise ValueError("Core factory manifest file inventory is malformed")
    records: dict[str, dict[str, object]] = {}
    ordered_names: list[str] = []
    for record in files:
        if (
            not isinstance(record, dict)
            or set(record) != {"file", "sha256", "size_bytes"}
            or not isinstance(record.get("file"), str)
            or not record["file"]
            or record["file"].startswith(".")
            or Path(record["file"]).name != record["file"]
            or HEX_64.fullmatch(str(record.get("sha256"))) is None
            or record.get("sha256") == "0" * 64
            or type(record.get("size_bytes")) is not int
            or record["size_bytes"] <= 0
            or record["file"] in records
            or record["file"] in {FACTORY_MANIFEST, FACTORY_COMPLETION}
        ):
            raise ValueError("Core factory manifest file inventory is malformed")
        name = record["file"]
        ordered_names.append(name)
        records[name] = record
    if ordered_names != sorted(ordered_names) or set(CORE_FACTORY_LEAVES) - set(records):
        raise ValueError("Core factory manifest does not bind the complete Core factory")
    return records


def completion_records(
    value: dict[str, Any], source_commit: str
) -> dict[str, dict[str, object]]:
    target_ids = [target_id for target_id, _, _, _ in CORE_TARGETS]
    if (
        set(value) != {"files", "kind", "schema_version", "source_commit", "targets"}
        or value.get("kind") != "ctx-public-core-release-completion"
        or value.get("schema_version") != 1
        or value.get("source_commit") != source_commit
        or value.get("targets") != target_ids
    ):
        raise ValueError("Core factory completion identity is malformed")
    files = value.get("files")
    if not isinstance(files, list) or len(files) != len(CORE_COMPLETION_LEAVES):
        raise ValueError("Core factory completion file inventory is malformed")
    records: dict[str, dict[str, object]] = {}
    for expected_name, record in zip(CORE_COMPLETION_LEAVES, files, strict=True):
        if (
            not isinstance(record, dict)
            or set(record) != {"name", "sha256", "size"}
            or record.get("name") != expected_name
            or HEX_64.fullmatch(str(record.get("sha256"))) is None
            or record.get("sha256") == "0" * 64
            or type(record.get("size")) is not int
            or record["size"] <= 0
        ):
            raise ValueError("Core factory completion file inventory is malformed")
        records[expected_name] = record
    return records


def verify_factory_completion_binding(
    factory: dict[str, dict[str, object]],
    completion: dict[str, dict[str, object]],
    factory_record: dict[str, object],
) -> None:
    completion_factory = completion[FACTORY_MANIFEST]
    if (
        completion_factory["sha256"] != factory_record["sha256"]
        or completion_factory["size"] != factory_record["size_bytes"]
    ):
        raise ValueError("Core factory completion does not bind the factory manifest")
    for name in CORE_FACTORY_LEAVES:
        factory_leaf = factory[name]
        completion_leaf = completion[name]
        if (
            factory_leaf["sha256"] != completion_leaf["sha256"]
            or factory_leaf["size_bytes"] != completion_leaf["size"]
        ):
            raise ValueError(
                f"Core factory and completion bindings disagree for {name}"
            )


def verify_retained_factory_leaf(
    name: str,
    record: dict[str, object],
    factory: dict[str, dict[str, object]],
    completion: dict[str, dict[str, object]],
) -> None:
    if (
        factory[name]["sha256"] != record["sha256"]
        or factory[name]["size_bytes"] != record["size_bytes"]
        or completion[name]["sha256"] != record["sha256"]
        or completion[name]["size"] != record["size_bytes"]
    ):
        raise ValueError(f"Core handoff does not retain exact factory bytes for {name}")


def verify_candidate_identity(
    candidate: dict[str, Any],
    target: tuple[str, str, str, str],
    source_commit: str,
    version: str,
    factory: dict[str, dict[str, object]],
) -> None:
    target_id, platform, rust_triple, binary = target
    artifact = factory[binary]
    if (
        set(candidate) != CANDIDATE_FIELDS
        or candidate.get("schema_version") != 1
        or candidate.get("kind") != "ctx-public-cli-candidate"
        or candidate.get("product") != "core"
        or candidate.get("version") != version
        or candidate.get("source") != {"clean": True, "commit": source_commit}
        or candidate.get("construction")
        != {
            "authority": "linux-cross-cargo-zigbuild-v1",
            "label": "scripts/release/build-public-candidate-on-linux.sh",
        }
        or candidate.get("target")
        != {"id": target_id, "platform": platform, "rust_triple": rust_triple}
        or candidate.get("artifact")
        != {
            "file": binary,
            "sha256": artifact["sha256"],
            "size_bytes": artifact["size_bytes"],
        }
    ):
        raise ValueError(f"Core candidate identity is malformed for {target_id}")
    evidence = candidate.get("evidence")
    if not isinstance(evidence, dict) or set(evidence) != CANDIDATE_EVIDENCE_FIELDS:
        raise ValueError(f"Core candidate evidence is malformed for {target_id}")
    for name, record in evidence.items():
        if (
            not isinstance(record, dict)
            or set(record) != {"file", "sha256"}
            or not isinstance(record.get("file"), str)
            or not record["file"]
            or HEX_64.fullmatch(str(record.get("sha256"))) is None
        ):
            raise ValueError(f"Core candidate evidence is malformed for {target_id}")
    retained_evidence = {
        "binary_size_report": f"{binary}.size.json",
        "build_info": f"{binary}.build-info.json",
        "cyclonedx_sbom": f"{binary}.cdx.json",
        "third_party_notices": f"{binary}.third-party-notices.txt",
    }
    for evidence_name, leaf_name in retained_evidence.items():
        if evidence[evidence_name] != {
            "file": leaf_name,
            "sha256": factory[leaf_name]["sha256"],
        }:
            raise ValueError(
                f"Core candidate does not bind exact {evidence_name} for {target_id}"
            )


def verify_release_handoff(args: argparse.Namespace) -> str:
    handoff = args.handoff_dir
    before = release_handoff_binding(handoff)

    handoff_document, handoff_bytes = read_canonical_json(
        handoff / HANDOFF_DOCUMENT,
        "Core GitHub handoff document",
        MAX_HANDOFF_JSON_BYTES,
    )
    handoff_sha256 = sha256_bytes(handoff_bytes)
    expected = args.expected_handoff_sha256
    if HEX_64.fullmatch(expected) is None or expected == "0" * 64:
        raise ValueError("expected Core handoff digest is invalid")
    if handoff_sha256 != expected:
        raise ValueError(
            "Core handoff digest does not match the independently supplied "
            "expected handoff digest"
        )
    if digest_sidecar(
        handoff / f"{HANDOFF_DOCUMENT}.sha256", "Core handoff digest sidecar"
    ) != handoff_sha256:
        raise ValueError("Core handoff digest sidecar does not match the handoff")

    source_commit = handoff_document.get("source_commit")
    if (
        set(handoff_document)
        != {
            "candidate_manifests",
            "factory_completion",
            "factory_manifest",
            "kind",
            "release_sums",
            "schema_version",
            "source_commit",
        }
        or handoff_document.get("kind") != "ctx-public-core-github-handoff"
        or handoff_document.get("schema_version") != 1
        or not isinstance(source_commit, str)
        or HEX_40.fullmatch(source_commit) is None
        or source_commit == "0" * 40
    ):
        raise ValueError("Core GitHub handoff document identity is malformed")

    candidate_records = handoff_document.get("candidate_manifests")
    if not isinstance(candidate_records, list) or len(candidate_records) != len(
        CORE_CANDIDATE_MANIFESTS
    ):
        raise ValueError("Core handoff candidate manifest inventory is malformed")
    candidates: dict[str, dict[str, Any]] = {}
    retained_records: dict[str, dict[str, object]] = {}
    candidate_digests: dict[str, str] = {}
    for name, declared in zip(CORE_CANDIDATE_MANIFESTS, candidate_records, strict=True):
        candidate, payload = read_canonical_json(
            handoff / name, f"Core candidate manifest {name}", MAX_CANDIDATE_JSON_BYTES
        )
        record = {
            "file": name,
            "sha256": sha256_bytes(payload),
            "size_bytes": len(payload),
        }
        require_document_record(declared, record, f"candidate manifest {name}")
        if digest_sidecar(
            handoff / f"{name}.sha256", f"Core candidate digest sidecar {name}"
        ) != record["sha256"]:
            raise ValueError(f"Core candidate digest sidecar does not match {name}")
        candidates[name] = candidate
        retained_records[name] = record
        candidate_digests[name] = str(record["sha256"])

    sums, sums_record = release_sums_record(handoff / RELEASE_SUMS)
    require_document_record(
        handoff_document.get("release_sums"), sums_record, RELEASE_SUMS
    )

    factory_document, factory_bytes = read_canonical_json(
        handoff / FACTORY_MANIFEST, "Core factory manifest", MAX_FACTORY_JSON_BYTES
    )
    factory_record = {
        "file": FACTORY_MANIFEST,
        "sha256": sha256_bytes(factory_bytes),
        "size_bytes": len(factory_bytes),
    }
    require_document_record(
        handoff_document.get("factory_manifest"), factory_record, FACTORY_MANIFEST
    )
    completion_document, completion_bytes = read_canonical_json(
        handoff / FACTORY_COMPLETION,
        "Core factory completion",
        MAX_COMPLETION_JSON_BYTES,
    )
    completion_record = {
        "file": FACTORY_COMPLETION,
        "sha256": sha256_bytes(completion_bytes),
        "size_bytes": len(completion_bytes),
    }
    require_document_record(
        handoff_document.get("factory_completion"),
        completion_record,
        FACTORY_COMPLETION,
    )
    factory = factory_manifest_records(factory_document, source_commit)
    completion = completion_records(completion_document, source_commit)
    verify_factory_completion_binding(factory, completion, factory_record)

    version = str(factory_document["version"])
    for target, name in zip(CORE_TARGETS, CORE_CANDIDATE_MANIFESTS, strict=True):
        verify_candidate_identity(
            candidates[name], target, source_commit, version, factory
        )
        verify_retained_factory_leaf(name, retained_records[name], factory, completion)

    for release_name, source_name in CORE_RELEASE_BINDINGS:
        if sums[release_name] != factory[source_name]["sha256"]:
            raise ValueError(
                f"Core SHA256SUMS does not bind {release_name} to the factory bytes"
            )

    for name in WINDOWS_HANDOFF_LEAVES:
        record, _ = actual_record(handoff / name, f"Windows Core handoff leaf {name}")
        retained_records[name] = record
        verify_retained_factory_leaf(name, record, factory, completion)

    args.artifact = handoff / "ctx.exe"
    args.build_info = handoff / "ctx.exe.build-info.json"
    args.sbom = handoff / "ctx.exe.cdx.json"
    args.notices = handoff / "ctx.exe.third-party-notices.txt"
    args.size_report = handoff / "ctx.exe.size.json"
    args.candidate_manifest = handoff / "ctx.exe.candidate.json"
    if verify_bundle_only(args) != candidate_digests["ctx.exe.candidate.json"]:
        raise ValueError("Windows candidate changed while its exact bytes were verified")

    if release_handoff_binding(handoff) != before:
        raise ValueError("release authority handoff changed while verified")
    return handoff_sha256


def read_canonical_json(path: Path, label: str, maximum: int) -> tuple[dict[str, Any], bytes]:
    payload = regular_bytes(path, label, maximum)
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is malformed") from error
    if not isinstance(value, dict) or canonical(value) != payload:
        raise ValueError(f"{label} is not canonical JSON")
    return value, payload


def verify_bundle_only(
    args: argparse.Namespace,
) -> str:
    artifact_sha256 = sha256_file(
        args.artifact, "Core artifact", 256 * 1024 * 1024
    )
    artifact_size = args.artifact.stat().st_size
    candidate, candidate_bytes = read_canonical_json(
        args.candidate_manifest, "candidate manifest", 16 * 1024 * 1024
    )
    candidate_sha256 = sha256_bytes(candidate_bytes)
    candidate_artifact_name = args.candidate_artifact_name or args.artifact.name
    if (
        not candidate_artifact_name
        or Path(candidate_artifact_name).name != candidate_artifact_name
    ):
        raise ValueError("candidate artifact name is invalid")
    if (
        set(candidate) != CANDIDATE_FIELDS
        or candidate.get("schema_version") != 1
        or candidate.get("kind") != "ctx-public-cli-candidate"
        or candidate.get("product") != "core"
        or candidate.get("artifact")
        != {
            "file": candidate_artifact_name,
            "sha256": artifact_sha256,
            "size_bytes": artifact_size,
        }
        or candidate.get("construction", {}).get("authority")
        != "linux-cross-cargo-zigbuild-v1"
    ):
        raise ValueError("candidate manifest does not bind the exact construction artifact")
    target = candidate.get("target")
    if (
        not isinstance(target, dict)
        or set(target) != {"id", "platform", "rust_triple"}
        or not all(
            isinstance(target.get(name), str) and target[name]
            for name in ("id", "platform", "rust_triple")
        )
        or candidate.get("version") is None
        or VERSION.fullmatch(str(candidate["version"])) is None
        or target["platform"]
        != ("linux-aarch64" if target["id"] == "linux-arm64" else target["id"])
    ):
        raise ValueError("candidate manifest target is malformed")
    construction = candidate.get("construction")
    authority = construction.get("authority") if isinstance(construction, dict) else None
    label = construction.get("label") if isinstance(construction, dict) else None
    if authority != "linux-cross-cargo-zigbuild-v1" or label != (
        "scripts/release/build-public-candidate-on-linux.sh"
    ):
        raise ValueError("candidate manifest does not bind its target construction route")
    build_info_bytes = regular_bytes(args.build_info, "build-info", 64 * 1024)
    build_info, _ = load_core_build_info(
        build_info_bytes,
        artifact_sha256,
        None,
        str(target["platform"]),
    )
    if (
        candidate.get("source") != build_info["source"]
        or target["rust_triple"] != build_info["target"]
    ):
        raise ValueError("candidate manifest does not bind its exact build-info")
    evidence_paths = {
        "binary_size_report": (args.size_report, ".size.json"),
        "build_info": (args.build_info, ".build-info.json"),
        "cyclonedx_sbom": (args.sbom, ".cdx.json"),
        "third_party_notices": (args.notices, ".third-party-notices.txt"),
    }
    evidence = candidate.get("evidence")
    if not isinstance(evidence, dict) or set(evidence) != CANDIDATE_EVIDENCE_FIELDS:
        raise ValueError("candidate manifest evidence is malformed")
    for name, record in evidence.items():
        if (
            not isinstance(record, dict)
            or set(record) != {"file", "sha256"}
            or not isinstance(record.get("file"), str)
            or not record["file"]
            or HEX_64.fullmatch(str(record.get("sha256"))) is None
        ):
            raise ValueError(f"candidate manifest {name} evidence is malformed")
    for name, (path, suffix) in evidence_paths.items():
        record = evidence.get(name)
        payload = regular_bytes(path, name.replace("_", " "), 32 * 1024 * 1024)
        if record != {
            "file": f"{candidate_artifact_name}{suffix}",
            "sha256": sha256_bytes(payload),
        }:
            raise ValueError(f"candidate manifest does not bind {name}")
    size_report, _ = read_canonical_json(
        args.size_report, "binary size report", 256 * 1024
    )
    if (
        set(size_report)
        != {"artifact", "kind", "product", "schema_version", "target", "version"}
        or size_report.get("schema_version") != 1
        or size_report.get("kind") != "ctx-binary-size-report"
        or size_report.get("product") != candidate["product"]
        or size_report.get("version") != candidate["version"]
        or size_report.get("target") != target
        or size_report.get("artifact") != candidate["artifact"]
    ):
        raise ValueError("binary size report does not bind the exact artifact")
    sbom, _ = read_canonical_json(args.sbom, "CycloneDX SBOM", 16 * 1024 * 1024)
    sbom_root = sbom.get("metadata", {}).get("component", {})
    if (
        sbom.get("bomFormat") != "CycloneDX"
        or sbom_root.get("name") != "ctx"
        or sbom_root.get("version") != candidate["version"]
        or sbom_root.get("hashes")
        != [{"alg": "SHA-256", "content": artifact_sha256}]
    ):
        raise ValueError("CycloneDX SBOM does not bind the exact artifact")
    notices = regular_bytes(
        args.notices, "third-party notices", 32 * 1024 * 1024
    )
    notice_bindings = (
        f"version: {candidate['version']}\n",
        f"target: {target['id']}\n",
        f"platform: {target['platform']}\n",
        f"artifact_sha256: {artifact_sha256}\n",
    )
    if any(binding.encode() not in notices for binding in notice_bindings):
        raise ValueError("third-party notices do not bind the exact artifact")
    tantivy = candidate.get("tantivy")
    closure = tantivy.get("dependency_closure") if isinstance(tantivy, dict) else None
    closure_identities: list[tuple[str, str, str]] = []
    if isinstance(closure, list):
        for package in closure:
            allowed = {"checksum", "license", "name", "source", "version"}
            if (
                not isinstance(package, dict)
                or not {"license", "name", "source", "version"}.issubset(package)
                or not set(package).issubset(allowed)
                or not all(
                    isinstance(package.get(name), str) and package[name]
                    for name in ("license", "name", "source", "version")
                )
                or (
                    "checksum" in package
                    and HEX_64.fullmatch(str(package["checksum"])) is None
                )
            ):
                raise ValueError("candidate manifest Tantivy closure is malformed")
            closure_identities.append(
                (package["name"], package["version"], package["source"])
            )
    if (
        not isinstance(tantivy, dict)
        or tantivy.get("version") != TANTIVY_VERSION
        or tantivy.get("default_features") is not False
        or tantivy.get("features") != TANTIVY_FEATURES
        or tantivy.get("resolved_crate_features") != TANTIVY_RESOLVED_FEATURES
        or HEX_64.fullmatch(str(tantivy.get("dependency_closure_sha256"))) is None
        or not closure_identities
        or closure_identities != sorted(set(closure_identities))
        or sha256_bytes(canonical(closure))
        != tantivy.get("dependency_closure_sha256")
        or ("tantivy", TANTIVY_VERSION)
        not in {(name, version) for name, version, _ in closure_identities}
        or {"fs4", "lz4_flex", "memmap2", "tempfile", "zstd"}
        - {name for name, _, _ in closure_identities}
        or any(name == "rust-stemmers" for name, _, _ in closure_identities)
    ):
        raise ValueError("candidate manifest Tantivy contract is malformed")
    return candidate_sha256


def require_full_arguments(parser: argparse.ArgumentParser, args: argparse.Namespace) -> None:
    names = (
        "artifact",
        "build_info",
        "candidate_manifest",
        "candidate_schema",
        "cargo_lock",
        "index_manifest",
        "index_format_manifest",
        "index_query_manifest",
        "license_materials",
        "module_file",
        "module_lock",
        "notices",
        "notices_output",
        "output",
        "platform",
        "product",
        "sbom",
        "size_report",
        "size_report_output",
        "target_id",
        "target_inventory",
        "target_matrix",
        "version",
        "workspace_manifest",
    )
    generate_only = {"notices_output", "output", "size_report_output"}
    verify_only = {"notices", "sbom", "size_report"}
    missing = []
    for name in names:
        if args.mode == "generate" and name in verify_only:
            continue
        if args.mode == "verify" and name in generate_only:
            continue
        if getattr(args, name) is None:
            missing.append("--" + name.replace("_", "-"))
    if missing:
        parser.error(f"{args.mode} requires " + ", ".join(missing))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "mode",
        choices=(
            "generate",
            "verify",
            "verify-bundle",
            "verify-release",
        ),
    )
    parser.add_argument("--product", choices=("core",))
    parser.add_argument("--version")
    parser.add_argument("--target-id")
    parser.add_argument("--platform")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--build-info", type=Path)
    parser.add_argument("--cargo-lock", type=Path)
    parser.add_argument("--module-lock", type=Path)
    parser.add_argument("--module-file", type=Path)
    parser.add_argument("--target-inventory", type=Path)
    parser.add_argument("--license-materials", type=Path)
    parser.add_argument("--target-matrix", type=Path)
    parser.add_argument("--candidate-schema", type=Path)
    parser.add_argument("--workspace-manifest", type=Path)
    parser.add_argument("--index-manifest", type=Path)
    parser.add_argument("--index-format-manifest", type=Path)
    parser.add_argument("--index-query-manifest", type=Path)
    parser.add_argument("--runfiles-root", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--notices-output", type=Path)
    parser.add_argument("--size-report-output", type=Path)
    parser.add_argument("--candidate-manifest", type=Path)
    parser.add_argument("--candidate-artifact-name")
    parser.add_argument("--sbom", type=Path)
    parser.add_argument("--notices", type=Path)
    parser.add_argument("--size-report", type=Path)
    parser.add_argument("--expected-handoff-sha256")
    parser.add_argument("--handoff-dir", type=Path)
    args = parser.parse_args()
    try:
        if args.mode == "verify-release":
            required = ("handoff_dir", "expected_handoff_sha256")
            missing = [
                "--" + name.replace("_", "-")
                for name in required
                if getattr(args, name) is None
            ]
            if missing:
                parser.error(f"verify-release requires " + ", ".join(missing))
            explicit_inputs = (
                "artifact",
                "build_info",
                "candidate_manifest",
                "notices",
                "sbom",
                "size_report",
            )
            if any(getattr(args, name) is not None for name in explicit_inputs):
                parser.error(
                    "verify-release accepts release inputs only through --handoff-dir"
                )
            print(verify_release_handoff(args))
            return 0

        if args.mode == "verify-bundle":
            required = (
                "artifact",
                "build_info",
                "candidate_manifest",
                "notices",
                "sbom",
                "size_report",
            )
            missing = [
                "--" + name.replace("_", "-")
                for name in required
                if getattr(args, name) is None
            ]
            if missing:
                parser.error(f"{args.mode} requires " + ", ".join(missing))
            print(verify_bundle_only(args))
            return 0

        require_full_arguments(parser, args)
        if args.mode == "generate":
            outputs = (
                args.output,
                args.notices_output,
                args.size_report_output,
                args.candidate_manifest,
            )
            if len(set(outputs)) != len(outputs):
                parser.error("generate outputs must be distinct")
            bundle = build_bundle(args)
            atomic_write(args.output, bundle["sbom"])
            atomic_write(args.notices_output, bundle["notices"])
            atomic_write(args.size_report_output, bundle["size"])
            atomic_write(args.candidate_manifest, bundle["candidate"])
            print(sha256_bytes(bundle["candidate"]))
        else:
            args.output = args.sbom
            args.notices_output = args.notices
            args.size_report_output = args.size_report
            bundle = build_bundle(args)
            actual = {
                "candidate": regular_bytes(
                    args.candidate_manifest,
                    "candidate manifest",
                    16 * 1024 * 1024,
                ),
                "notices": regular_bytes(
                    args.notices,
                    "third-party notices",
                    32 * 1024 * 1024,
                ),
                "sbom": regular_bytes(
                    args.sbom,
                    "CycloneDX SBOM",
                    16 * 1024 * 1024,
                ),
                "size": regular_bytes(
                    args.size_report,
                    "binary size report",
                    256 * 1024,
                ),
            }
            mismatched = [name for name in bundle if actual[name] != bundle[name]]
            if mismatched:
                raise ValueError(
                    "release evidence does not match the exact artifact, source, "
                    "build, license, feature, and dependency material: "
                    + ", ".join(sorted(mismatched))
                )
            print(sha256_bytes(actual["candidate"]))
    except (OSError, ValueError) as error:
        raise SystemExit(f"error: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
