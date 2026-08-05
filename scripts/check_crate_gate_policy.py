"""Immutable snapshot-backed exceptions for the Rust crate gate."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
from typing import Any

from check_crate_gate_graph import GateError, canonical_bytes


SNAPSHOT = "187c0c6533b5de56119cd2774ba90d29925cb1ac"
SNAPSHOT_INVENTORY_SHA256 = "1d3fc79952804c11d63b87ef7012b52cc90dc75ebfcdd2931a41e2d8bb4bca3c"
SHA256 = re.compile(r"^[0-9a-f]{64}$")


def _normalized_path(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise GateError(f"{label} must be a nonempty path")
    path = PurePosixPath(value)
    if (
        path.is_absolute()
        or value.endswith("/")
        or any(part in {"", ".", ".."} for part in path.parts)
        or any(character in value for character in ("\x00", "\t", "\n", "\r", "\\", "*", "?", "["))
    ):
        raise GateError(f"{label} is not normalized: {value!r}")
    return value


def _load_json(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise GateError(f"{label} is not valid UTF-8 JSON: {error}") from error
    if not isinstance(value, dict):
        raise GateError(f"{label} root must be an object")
    return value


def load_snapshot_inventory(root: Path, policy: dict[str, Any]) -> dict[str, Any]:
    configured = policy.get("snapshot_inventory")
    requested = os.environ.get("CTX_CRATE_LOC_SNAPSHOT_INVENTORY")
    path = Path(requested) if requested else root / _normalized_path(configured, "snapshot_inventory")
    if not path.is_absolute():
        path = root / path
    path = path.absolute()
    try:
        path.relative_to(root)
    except ValueError as error:
        raise GateError("crate gate snapshot inventory must be inside the repository") from error
    if not path.is_file():
        raise GateError(f"crate gate snapshot inventory is missing: {path}")
    raw = path.read_bytes()
    actual_hash = hashlib.sha256(raw).hexdigest()
    if actual_hash != SNAPSHOT_INVENTORY_SHA256:
        raise GateError(
            "crate gate snapshot inventory identity changed: "
            f"expected {SNAPSHOT_INVENTORY_SHA256}, got {actual_hash}"
        )
    value = _load_json(path, "crate gate snapshot inventory")
    if raw != canonical_bytes(value) + b"\n":
        raise GateError("crate gate snapshot inventory must be canonical JSON")
    if set(value) != {
        "schema_version",
        "snapshot",
        "platforms",
        "packages",
        "workspace_edges",
        "exceptions",
        "temporary_edges",
    } or value.get("schema_version") != 1 or value.get("snapshot") != SNAPSHOT:
        raise GateError("crate gate snapshot inventory schema or commit identity is invalid")
    packages = value.get("packages")
    if not isinstance(packages, list) or [item.get("package") for item in packages if isinstance(item, dict)] != sorted(
        item.get("package") for item in packages if isinstance(item, dict)
    ):
        raise GateError("snapshot package inventory must be a sorted array")
    package_by_name: dict[str, dict[str, Any]] = {}
    for item in packages:
        if not isinstance(item, dict) or set(item) != {
            "package",
            "manifest",
            "production_targets",
            "source_digest",
            "source_files",
            "loaded_source_files",
            "production_cloc",
        }:
            raise GateError("snapshot package record is malformed")
        name = item["package"]
        if not isinstance(name, str) or name in package_by_name:
            raise GateError("snapshot package names must be unique strings")
        _normalized_path(item["manifest"], f"snapshot manifest for {name}")
        targets = item["production_targets"]
        if not isinstance(targets, list) or targets != sorted(targets) or len(targets) != len(set(targets)):
            raise GateError(f"snapshot production targets are not canonical: {name}")
        if not isinstance(item["source_digest"], str) or SHA256.fullmatch(item["source_digest"]) is None:
            raise GateError(f"snapshot source digest is invalid: {name}")
        for field in ("source_files", "loaded_source_files", "production_cloc"):
            if not isinstance(item[field], int) or isinstance(item[field], bool) or item[field] < 0:
                raise GateError(f"snapshot {field} is invalid: {name}")
        package_by_name[name] = item
    value["package_by_name"] = package_by_name
    return value


def validate_ledger(policy: dict[str, Any], snapshot: dict[str, Any]) -> dict[str, dict[str, Any]]:
    immutable_entries = snapshot.get("exceptions")
    if not isinstance(immutable_entries, list):
        raise GateError("snapshot exceptions must be an array")
    immutable: dict[str, tuple[str, int]] = {}
    for item in immutable_entries:
        if not isinstance(item, dict) or set(item) != {"exception_id", "package", "maximum_cloc"}:
            raise GateError("snapshot exception is malformed")
        identity = item["exception_id"]
        package = item["package"]
        maximum = item["maximum_cloc"]
        package_record = snapshot["package_by_name"].get(package)
        if (
            not isinstance(identity, str)
            or identity in immutable
            or not isinstance(maximum, int)
            or isinstance(maximum, bool)
            or package_record is None
            or package_record["production_cloc"] != maximum
        ):
            raise GateError("snapshot exception identity or maximum is invalid")
        immutable[identity] = (package, maximum)
    entries = policy.get("grandfathered")
    if not isinstance(entries, list):
        raise GateError("grandfathered entries must be an array")
    result: dict[str, dict[str, Any]] = {}
    keys: list[str] = []
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"exception_id", "package", "code_baseline"}:
            raise GateError("grandfathered entry is malformed")
        exception_id = entry["exception_id"]
        package = entry["package"]
        baseline = entry["code_baseline"]
        maximum = immutable.get(exception_id)
        if maximum is None or maximum[0] != package:
            raise GateError(f"unknown or reassigned grandfathered exception: {exception_id}")
        if (
            not isinstance(baseline, int)
            or isinstance(baseline, bool)
            or baseline <= policy["hard_limit"]
            or baseline > maximum[1]
        ):
            raise GateError(f"invalid no-growth ceiling for {exception_id}")
        if package in result:
            raise GateError(f"duplicate grandfathered package: {package}")
        keys.append(exception_id)
        result[package] = dict(entry)
    if keys != sorted(keys):
        raise GateError("grandfathered entries must be sorted by exception_id")
    return result


def validate_temporary_edges(policy: dict[str, Any], snapshot: dict[str, Any]) -> dict[str, tuple[str, str]]:
    immutable_entries = snapshot.get("temporary_edges")
    if not isinstance(immutable_entries, list):
        raise GateError("snapshot temporary_edges must be an array")
    immutable: dict[str, tuple[str, str, str]] = {}
    snapshot_edges = {(value["from"], value["to"]) for value in snapshot["workspace_edges"]}
    for item in immutable_entries:
        if not isinstance(item, dict) or set(item) != {"exception_id", "from", "to"}:
            raise GateError("snapshot temporary edge is malformed")
        identity = item["exception_id"]
        if not isinstance(identity, str) or identity in immutable:
            raise GateError("snapshot temporary edge identities must be unique strings")
        edge = (item["from"], item["to"])
        if edge not in snapshot_edges:
            raise GateError(f"snapshot temporary edge was absent at {SNAPSHOT}: {identity}")
        immutable[identity] = (edge[0], edge[1], SNAPSHOT)
    entries = policy.get("temporary_edges")
    if not isinstance(entries, list):
        raise GateError("temporary_edges must be an array")
    keys: list[str] = []
    result: dict[str, tuple[str, str]] = {}
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"exception_id", "from", "to", "introduced_at"}:
            raise GateError("temporary edge entry is malformed")
        exception_id = entry["exception_id"]
        immutable_edge = immutable.get(exception_id)
        actual = (entry["from"], entry["to"], entry["introduced_at"])
        if immutable_edge is None or actual != immutable_edge:
            raise GateError(f"unknown or reassigned temporary edge: {exception_id}")
        keys.append(exception_id)
        result[exception_id] = (entry["from"], entry["to"])
    if keys != sorted(keys):
        raise GateError("temporary edges must be sorted by exception_id")
    return result
