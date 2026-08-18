#!/usr/bin/env python3
"""Seal or verify the shared five-target Core release output."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat


def load_release_bundle():
    path = Path(__file__).resolve().with_name("release_bundle.py")
    spec = importlib.util.spec_from_file_location("ctx_release_bundle", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load release bundle module: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release_bundle = load_release_bundle()

CORE_COMPLETION_KIND = "ctx-public-core-release-completion"
CORE_COMPLETION_LEAF = "ctx-core.release-complete.json"
CORE_COMPLETION_SCHEMA_VERSION = 1
CORE_TARGETS = (
    ("linux-arm64", "linux-aarch64", "ctx-linux-aarch64"),
    ("linux-x64", "linux-x64", "ctx"),
    ("macos-arm64", "macos-arm64", "ctx-macos-arm64"),
    ("macos-x64", "macos-x64", "ctx-macos-x64"),
    ("windows-x64", "windows-x64", "ctx.exe"),
)
FACTORY_MANIFEST = "ctx-release-factory.json"
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)")


def _unique_object(pairs: list[tuple[str, object]]) -> dict[str, object]:
    value: dict[str, object] = {}
    for key, item in pairs:
        if key in value:
            raise release_bundle.BundleError(
                f"Core factory manifest repeats field {key!r}"
            )
        value[key] = item
    return value


def verify_factory_manifest(candidate: Path, source_commit: str) -> dict[str, object]:
    path = candidate / FACTORY_MANIFEST
    try:
        metadata = path.lstat()
        if (
            stat.S_ISLNK(metadata.st_mode)
            or not stat.S_ISREG(metadata.st_mode)
            or metadata.st_size <= 0
            or metadata.st_size > 16 * 1024 * 1024
        ):
            raise release_bundle.BundleError("Core factory manifest is invalid")
        raw = path.read_bytes()
        value = json.loads(raw, object_pairs_hook=_unique_object)
    except (FileNotFoundError, json.JSONDecodeError, UnicodeDecodeError) as error:
        raise release_bundle.BundleError("Core factory manifest is invalid") from error
    if (
        not isinstance(value, dict)
        or raw != (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
        or set(value)
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
        or value.get("selected_targets")
        != [target_id for target_id, _, _ in CORE_TARGETS]
        or value.get("releasable") is not True
        or value.get("runtime_sidecars_included") is not False
        or not isinstance(value.get("version"), str)
        or VERSION.fullmatch(value["version"]) is None
    ):
        raise release_bundle.BundleError(
            "Core factory manifest is not the exact releasable five-target source"
        )
    records = value.get("files")
    if not isinstance(records, list):
        raise release_bundle.BundleError("Core factory file inventory is invalid")
    inventory: dict[str, dict[str, object]] = {}
    for record in records:
        if (
            not isinstance(record, dict)
            or set(record) != {"file", "sha256", "size_bytes"}
            or not isinstance(record.get("file"), str)
        ):
            raise release_bundle.BundleError("Core factory file inventory is invalid")
        name = record["file"]
        if (
            name in inventory
            or not name
            or name.startswith(".")
            or Path(name).name != name
            or name in {FACTORY_MANIFEST, CORE_COMPLETION_LEAF}
        ):
            raise release_bundle.BundleError("Core factory file inventory is invalid")
        actual = release_bundle._file_record(candidate / name, name)
        if (
            record.get("sha256") != actual["sha256"]
            or record.get("size_bytes") != actual["size"]
        ):
            raise release_bundle.BundleError(
                f"Core factory manifest does not bind exact bytes for {name}"
            )
        inventory[name] = record
    actual_names = set(release_bundle._names(candidate)) - {
        FACTORY_MANIFEST,
        CORE_COMPLETION_LEAF,
    }
    if actual_names != set(inventory):
        raise release_bundle.BundleError("Core factory file inventory is not exact")
    return value


def expected_core_release_leaves() -> list[str]:
    leaves = ["ctx-release-factory.json"]
    for _, _, binary in CORE_TARGETS:
        leaves.extend(
            (
                binary,
                f"{binary}.build-info.json",
                f"{binary}.candidate.json",
                f"{binary}.cdx.json",
                f"{binary}.cdx.json.sha256",
                f"{binary}.dependency-advisory.json",
                f"{binary}.sha256",
                f"{binary}.size.json",
                f"{binary}.third-party-notices.txt",
                f"{binary}.third-party-notices.txt.sha256",
                f"{binary}.version",
            )
        )
    return sorted(leaves)


def _core_identity(source_commit: str) -> dict[str, object]:
    if not release_bundle._valid_commit(source_commit):
        raise release_bundle.BundleError("Core release source commit is invalid")
    return {
        "kind": CORE_COMPLETION_KIND,
        "schema_version": CORE_COMPLETION_SCHEMA_VERSION,
        "source_commit": source_commit,
        "targets": [target_id for target_id, _, _ in CORE_TARGETS],
    }


def _core_file_record(
    path: Path, name: str, *, durable: bool = False
) -> dict[str, object]:
    return release_bundle._file_record(path, name, durable=durable)


def seal_core_candidate(candidate: Path, source_commit: str) -> str:
    identity = _core_identity(source_commit)
    root_binding = release_bundle._require_directory(candidate, "Core release stage")
    verify_factory_manifest(candidate, source_commit)
    initial_names = release_bundle._names(candidate)
    marker = candidate / CORE_COMPLETION_LEAF
    if CORE_COMPLETION_LEAF in initial_names:
        raise release_bundle.BundleError("Core release stage is already sealed")
    expected = expected_core_release_leaves()
    missing = sorted(set(expected) - set(initial_names))
    if missing:
        raise release_bundle.BundleError(
            f"Core release stage is incomplete; missing {missing}"
        )
    records = [
        _core_file_record(candidate / name, name, durable=True) for name in expected
    ]
    if (
        release_bundle._binding(candidate) != root_binding
        or release_bundle._names(candidate) != initial_names
    ):
        raise release_bundle.BundleError("Core release stage changed while sealed")
    payload = {**identity, "files": records}
    encoded = (
        json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode()
    descriptor = os.open(
        marker,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
        0o600,
    )
    try:
        with os.fdopen(descriptor, "wb", closefd=False) as destination:
            destination.write(encoded)
            destination.flush()
            os.fsync(destination.fileno())
    finally:
        os.close(descriptor)
    release_bundle._fsync_directory(candidate)
    return hashlib.sha256(encoded).hexdigest()


def verify_core_candidate(candidate: Path, source_commit: str) -> dict[str, object]:
    identity = _core_identity(source_commit)
    root_binding = release_bundle._require_directory(candidate, "Core release bundle")
    initial_names = release_bundle._names(candidate)
    marker = candidate / CORE_COMPLETION_LEAF
    try:
        marker_binding = release_bundle._binding(marker)
        if not stat.S_ISREG(marker_binding[2]) or marker_binding[3] > 1024 * 1024:
            raise release_bundle.BundleError("Core release completion marker is invalid")
        marker_bytes = marker.read_bytes()
        payload = json.loads(marker_bytes)
    except (FileNotFoundError, json.JSONDecodeError, UnicodeDecodeError) as error:
        raise release_bundle.BundleError(
            "Core release completion marker is invalid"
        ) from error
    if release_bundle._binding(marker) != marker_binding:
        raise release_bundle.BundleError("Core release completion marker changed")
    if (
        not isinstance(payload, dict)
        or set(payload) != {*identity, "files"}
        or {key: payload.get(key) for key in identity} != identity
    ):
        raise release_bundle.BundleError("Core release completion identity is invalid")
    verify_factory_manifest(candidate, source_commit)
    expected = expected_core_release_leaves()
    records = payload.get("files")
    if not isinstance(records, list) or len(records) != len(expected):
        raise release_bundle.BundleError("Core release completion file manifest is invalid")
    expected_records: dict[str, dict[str, object]] = {}
    for name, record in zip(expected, records, strict=True):
        if (
            not isinstance(record, dict)
            or set(record) != {"name", "sha256", "size"}
            or record.get("name") != name
        ):
            raise release_bundle.BundleError(
                "Core release completion file manifest is invalid"
            )
        expected_records[name] = record
    missing = sorted(set(expected) - set(initial_names))
    if missing:
        raise release_bundle.BundleError(
            f"Core release bundle is missing declared leaves: {missing}"
        )
    for name in expected:
        if _core_file_record(candidate / name, name) != expected_records[name]:
            raise release_bundle.BundleError(
                f"Core release leaf does not match completion marker: {name}"
            )
    if (
        release_bundle._binding(candidate) != root_binding
        or release_bundle._binding(marker) != marker_binding
        or release_bundle._names(candidate) != initial_names
    ):
        raise release_bundle.BundleError("Core release bundle changed while verified")
    return payload


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--candidate-dir", required=True, type=Path)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    candidate = args.candidate_dir.resolve(strict=True)
    if args.verify:
        verify_core_candidate(candidate, args.source_commit)
        print("Core release completion verified")
        return 0
    print(seal_core_candidate(candidate, args.source_commit))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
