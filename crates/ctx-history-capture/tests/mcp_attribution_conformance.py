#!/usr/bin/env python3
"""Fail-closed MCP tool-call attribution provider conformance runner."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
from typing import Any

from mcp_attribution_contract_registry import (
    ROUTE_CONTRACT_CLASSIFICATION,
    ROUTE_SCHEMA_CONTRACTS,
    RouteSchemaContract,
)
from mcp_attribution_conformance_schema import (
    ALLOWED_STATUSES,
    AUDIT_STATUS_TO_MANIFEST,
    AuditLaneKey,
    BAZEL_TARGET_RE,
    BaseKey,
    CAPABILITY_REVISION,
    CONFORMANCE_MODES,
    ConformanceError,
    EVIDENCE_SCOPES,
    EXACT_RUNTIME_PRODUCER_BOUND,
    EXCLUDED_REASONS,
    EXPECTED_AUDIT_COUNTS,
    EXPECTED_PROVIDER_STATUS_ROWS,
    HOSTED_BOUNDARY_FORMAT_SCHEMA,
    HOSTED_BOUNDARY_PRODUCER_DOMAIN,
    HOSTED_BOUNDARY_ROUTE,
    HOSTED_BOUNDARY_SOURCE_SCHEMA,
    LaneKey,
    LOCAL_DEEPAGENTS_ROUTE,
    MANIFEST_SCHEMA_VERSION,
    NOT_QUALIFIED_REASONS,
    PROVIDER_NEUTRAL_EVIDENCE_CLASSES,
    ProducerDomain,
    PUBLIC_VALIDATION_MODE,
    REQUIRED_EVIDENCE_CLASSES,
    SUMMARY_RE,
    SchemaKey,
    _base_key,
    _canonical,
    _exact_keys,
    _lane_key,
    _list,
    _load_json,
    _nonempty_string,
    _object,
    _positive_int,
    _schema_key,
    _sha256,
    _validate_audit_producer_bound,
    _validate_format_schema,
    _validate_producer_domain,
)


CapabilityClaims = dict[str, dict[str, set[str]]]
RouteContracts = tuple[RouteSchemaContract, ...]


@dataclass(frozen=True)
class SuiteBinding:
    target: str
    binary: Path
    selected_inventory: bool = False


SuiteBindings = dict[str, SuiteBinding]


@dataclass(frozen=True)
class ConformanceInventory:
    provider_count: int
    base_route_count: int
    schema_generation_count: int
    capability_lane_count: int
    status_rows: dict[str, int]
    public_claims: CapabilityClaims

    @property
    def public_executable_count(self) -> int:
        return sum(len(tests) for tests in self.public_claims.values())


def _provider_status_rows(statuses: dict[str, set[str]]) -> dict[str, int]:
    rows = {kind: 0 for kind in ALLOWED_STATUSES}
    for provider, provider_statuses in statuses.items():
        if "supported" in provider_statuses:
            rows["supported"] += 1
        elif provider_statuses == {"excluded"}:
            rows["excluded"] += 1
        elif provider_statuses <= {"not_qualified", "excluded"}:
            rows["not_qualified"] += 1
        else:
            raise ConformanceError(
                f"provider {provider} has incoherent statuses {sorted(provider_statuses)}"
            )
    return rows


def _validate_capability_audit_projection(
    manifest: dict[str, Any], capability_audit: dict[str, Any] | None = None
) -> None:
    expected_status_rows = manifest["expected_status_rows"]
    expected_provider_rows = manifest["expected_provider_status_rows"]
    expected_audit_counts = {
        "providers": manifest["expected_provider_count"],
        "base_routes": manifest["expected_base_route_count"],
        "capability_lanes": manifest["expected_capability_lane_count"],
        "lane_statuses": {
            audit: expected_status_rows[manifest_status]
            for audit, manifest_status in AUDIT_STATUS_TO_MANIFEST.items()
        },
        "provider_statuses": {
            audit: expected_provider_rows[manifest_status]
            for audit, manifest_status in AUDIT_STATUS_TO_MANIFEST.items()
        },
    }
    if capability_audit is None:
        manifest_to_audit = {
            manifest_status: audit
            for audit, manifest_status in AUDIT_STATUS_TO_MANIFEST.items()
        }
        capability_audit = {
            "expected_counts": expected_audit_counts,
            "routes": [
                {
                    "provider_id": lane["provider"],
                    "route": lane["route"],
                    "source_format": lane["source_format"],
                    "source_schema": lane["source_schema"],
                    "producer_bound": lane["producer_bound"],
                    "status": manifest_to_audit[lane["status"]["kind"]],
                }
                for lane in manifest["capability_lanes"]
            ],
        }
    if capability_audit.get("expected_counts") != expected_audit_counts:
        raise ConformanceError("capability audit expected counts are stale")
    audit_rows: dict[AuditLaneKey, str] = {}
    audit_provider_statuses: dict[str, set[str]] = {}
    audit_bases: set[BaseKey] = set()
    for index, raw_row in enumerate(
        _list(capability_audit.get("routes"), "capability audit routes")
    ):
        label = f"capability audit routes[{index}]"
        row = _object(raw_row, label)
        provider = _nonempty_string(row.get("provider_id"), f"{label}.provider_id")
        route = _nonempty_string(row.get("route"), f"{label}.route")
        source_format = _nonempty_string(
            row.get("source_format"), f"{label}.source_format"
        )
        source_schema = _nonempty_string(
            row.get("source_schema"), f"{label}.source_schema"
        )
        audit_status = _nonempty_string(row.get("status"), f"{label}.status")
        manifest_status = AUDIT_STATUS_TO_MANIFEST.get(audit_status)
        if manifest_status is None:
            raise ConformanceError(f"{label}.status is not closed")
        bound = _validate_audit_producer_bound(
            row.get("producer_bound"), f"{label}.producer_bound", audit_status
        )
        key = (provider, route, source_format, source_schema, bound)
        if key in audit_rows:
            raise ConformanceError(f"duplicate capability audit lane {key}")
        audit_rows[key] = manifest_status
        audit_provider_statuses.setdefault(provider, set()).add(manifest_status)
        audit_bases.add((provider, route, source_format))

    if (
        len(audit_provider_statuses) != expected_audit_counts["providers"]
        or len(audit_bases) != expected_audit_counts["base_routes"]
        or len(audit_rows) != expected_audit_counts["capability_lanes"]
    ):
        raise ConformanceError("capability audit route arithmetic is stale")
    audit_provider_rows = _provider_status_rows(audit_provider_statuses)
    if audit_provider_rows != expected_provider_rows:
        raise ConformanceError(
            f"capability audit provider statuses differ: {audit_provider_rows}"
        )
    if expected_audit_counts["providers"] == EXPECTED_AUDIT_COUNTS["providers"]:
        supported_providers = {
            provider
            for provider, statuses in audit_provider_statuses.items()
            if "supported" in statuses
        }
        if supported_providers != {"codex", "warp", "copilot_cli"}:
            raise ConformanceError(
                f"capability audit exact providers differ: {sorted(supported_providers)}"
            )
        expected_runtime_bound = _canonical(EXACT_RUNTIME_PRODUCER_BOUND)
        drifted_runtime_bounds = sorted(
            key
            for key, status in audit_rows.items()
            if status == "supported" and key[4] != expected_runtime_bound
        )
        if drifted_runtime_bounds:
            raise ConformanceError(
                "exact runtime lanes must use structural unversioned generation 1: "
                f"{drifted_runtime_bounds}"
            )
        hosted_keys = [key for key in audit_rows if key[:3] == HOSTED_BOUNDARY_ROUTE]
        if (
            len(hosted_keys) != 1
            or hosted_keys[0][3] != HOSTED_BOUNDARY_SOURCE_SCHEMA
            or audit_rows[hosted_keys[0]] != "excluded"
        ):
            raise ConformanceError("capability audit hosted boundary tuple differs")

    manifest_rows: dict[AuditLaneKey, str] = {}
    manifest_provider_statuses: dict[str, set[str]] = {}
    for index, raw_lane in enumerate(
        _list(manifest.get("capability_lanes"), "capability_lanes")
    ):
        label = f"capability_lanes[{index}]"
        lane = _object(raw_lane, label)
        for field in ["provider", "route", "source_format", "source_schema"]:
            _nonempty_string(lane.get(field), f"{label}.{field}")
        status = _object(lane.get("status"), f"{label}.status").get("kind")
        if status not in ALLOWED_STATUSES:
            raise ConformanceError(f"{label}.status is not closed")
        audit_status = next(
            audit_name
            for audit_name, manifest_name in AUDIT_STATUS_TO_MANIFEST.items()
            if manifest_name == status
        )
        bound = _validate_audit_producer_bound(
            lane.get("producer_bound"), f"{label}.producer_bound", audit_status
        )
        key = (
            lane["provider"],
            lane["route"],
            lane["source_format"],
            lane["source_schema"],
            bound,
        )
        if key in manifest_rows:
            raise ConformanceError(f"duplicate manifest capability projection {key}")
        manifest_rows[key] = status
        manifest_provider_statuses.setdefault(lane["provider"], set()).add(status)
    if manifest_rows != audit_rows:
        raise ConformanceError(
            "manifest capability projection differs from capability audit: "
            f"missing={sorted(set(audit_rows) - set(manifest_rows))}, "
            f"unknown={sorted(set(manifest_rows) - set(audit_rows))}, "
            f"status_drift={sorted(key for key in set(audit_rows) & set(manifest_rows) if audit_rows[key] != manifest_rows[key])}"
        )
    manifest_provider_rows = _provider_status_rows(manifest_provider_statuses)
    if manifest_provider_rows != expected_provider_rows:
        raise ConformanceError(
            f"manifest provider statuses differ: {manifest_provider_rows}"
        )


def _default_repository_root() -> Path:
    candidates = [
        Path.cwd().resolve(),
        Path(__file__).resolve().parents[3],
        Path(__file__).absolute().parents[3],
    ]
    marker = Path("crates/ctx-history-capture/tests/mcp_attribution_conformance.py")
    for candidate in candidates:
        if not (candidate / marker).is_file():
            continue
        result = subprocess.run(
            ["git", "-C", str(candidate), "rev-parse", "--show-toplevel"],
            check=False,
            capture_output=True,
            text=True,
            encoding="utf-8",
        )
        if result.returncode == 0 and Path(result.stdout.strip()).resolve() == candidate:
            return candidate
    raise ConformanceError("cannot locate repository root for shape validation")


def _contract_format_schema(contract: RouteSchemaContract) -> dict[str, Any]:
    return {**contract.format_schema, "shape_sha256": contract.sha256}


def _contract_document(contract: RouteSchemaContract) -> dict[str, Any]:
    return {
        "contract_schema_version": 1,
        "classification": contract.classification,
        "provider": contract.provider,
        "route": contract.route,
        "source_format": contract.source_format,
        "format_schema": contract.format_schema,
        "producer_domain": contract.producer_domain,
    }


def _require_tracked_regular_file(
    repository_root: Path,
    reference_path: Path,
    allowed_subtree: Path,
    label: str,
) -> bytes:
    if (
        reference_path.is_absolute()
        or allowed_subtree.is_absolute()
        or ".." in reference_path.parts
        or ".." in allowed_subtree.parts
    ):
        raise ConformanceError(f"{label} paths must be repository-relative")
    if reference_path.parent != allowed_subtree:
        raise ConformanceError(f"{label}.path is outside its exact allowed route subtree")

    artifact = repository_root / reference_path
    current = repository_root
    try:
        for part in reference_path.parts:
            current /= part
            metadata = current.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                raise ConformanceError(f"{label}.path contains a symlink: {reference_path}")
    except FileNotFoundError as error:
        raise ConformanceError(f"{label}.path does not exist: {reference_path}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise ConformanceError(f"{label}.path is not a regular file: {reference_path}")

    root_resolved = repository_root.resolve(strict=True)
    allowed_resolved = (repository_root / allowed_subtree).resolve(strict=True)
    artifact_resolved = artifact.resolve(strict=True)
    if not allowed_resolved.is_relative_to(root_resolved):
        raise ConformanceError(f"{label}.allowed_subtree escapes the repository")
    if not artifact_resolved.is_relative_to(allowed_resolved):
        raise ConformanceError(f"{label}.path resolves outside its exact allowed route subtree")

    tracked = subprocess.run(
        [
            "git",
            "-C",
            str(repository_root),
            "ls-files",
            "--error-unmatch",
            "--",
            reference_path.as_posix(),
        ],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if tracked.returncode != 0:
        raise ConformanceError(f"{label}.path is not a tracked file: {reference_path}")
    return artifact.read_bytes()


def _validate_route_contract_registry(
    route_contracts: RouteContracts,
    repository_root: Path,
) -> dict[SchemaKey, RouteSchemaContract]:
    if not route_contracts:
        raise ConformanceError("closed route schema contract registry is empty")
    contracts: dict[SchemaKey, RouteSchemaContract] = {}
    paths: dict[str, BaseKey] = {}
    digests: dict[str, BaseKey] = {}
    for index, contract in enumerate(route_contracts):
        label = f"route_schema_contracts[{index}]"
        for field, value in [
            ("provider", contract.provider),
            ("route", contract.route),
            ("source_format", contract.source_format),
            ("path", contract.path),
            ("allowed_subtree", contract.allowed_subtree),
            ("classification", contract.classification),
        ]:
            _nonempty_string(value, f"{label}.{field}")
        if contract.classification != ROUTE_CONTRACT_CLASSIFICATION:
            raise ConformanceError(f"{label}.classification is not closed")
        digest = _sha256(contract.sha256, f"{label}.sha256")
        format_schema = _contract_format_schema(contract)
        _validate_format_schema(format_schema, f"{label}.format_schema")
        _validate_producer_domain(
            contract.producer_domain, f"{label}.producer_domain"
        )
        schema_key = (*contract.key, _canonical(format_schema))
        if schema_key in contracts:
            raise ConformanceError(f"duplicate closed route schema contract {schema_key}")
        if contract.path in paths:
            raise ConformanceError(
                f"route schema contract path {contract.path} is reused by "
                f"{paths[contract.path]} and {contract.key}"
            )
        if digest in digests:
            raise ConformanceError(
                f"route shape digest {digest} is reused by "
                f"{digests[digest]} and {contract.key}"
            )
        contents = _require_tracked_regular_file(
            repository_root,
            Path(contract.path),
            Path(contract.allowed_subtree),
            label,
        )
        actual = hashlib.sha256(contents).hexdigest()
        if actual != digest:
            raise ConformanceError(
                f"{label}.sha256 differs from {contract.path}: "
                f"expected={digest}, actual={actual}"
            )
        try:
            document = json.loads(contents)
        except json.JSONDecodeError as error:
            raise ConformanceError(f"{label}.path is not valid JSON") from error
        if document != _contract_document(contract):
            raise ConformanceError(
                f"{label}.path contents differ from the closed route schema contract"
            )
        contracts[schema_key] = contract
        paths[contract.path] = contract.key
        digests[digest] = contract.key
    return contracts


def _validate_provenance(
    value: Any,
    label: str,
    contract: RouteSchemaContract,
) -> None:
    provenance = _object(value, label)
    expected = {
        "kind": "route_schema_contract",
        "classification": contract.classification,
        "reference": contract.path,
        "sha256": contract.sha256,
    }
    if provenance != expected:
        raise ConformanceError(
            f"{label} differs from the closed route schema contract registry"
        )


def _matrix_inventory(
    provider_matrix: dict[str, Any],
) -> tuple[set[str], set[BaseKey]]:
    _exact_keys(
        provider_matrix,
        {
            "schema_version",
            "scope",
            "lineage_capability_values",
            "custom_history_lineage_support",
            "providers",
        },
        "matrix",
    )
    if provider_matrix["schema_version"] != 2:
        raise ConformanceError("provider matrix schema_version must equal 2")
    provider_ids: set[str] = set()
    base_routes: set[BaseKey] = set()
    for provider_index, raw_provider in enumerate(
        _list(provider_matrix["providers"], "matrix.providers")
    ):
        label = f"matrix.providers[{provider_index}]"
        provider = _object(raw_provider, label)
        provider_id = _nonempty_string(provider.get("id"), f"{label}.id")
        if provider_id in provider_ids:
            raise ConformanceError(f"duplicate matrix provider {provider_id}")
        provider_ids.add(provider_id)
        if provider.get("status") != "supported":
            raise ConformanceError(f"matrix provider {provider_id} is not supported")
        native_count = 0
        for path_index, raw_path in enumerate(
            _list(provider.get("implemented_paths"), f"{label}.implemented_paths")
        ):
            path = _object(raw_path, f"{label}.implemented_paths[{path_index}]")
            if path.get("kind") != "native_import":
                continue
            native_count += 1
            key = (
                provider_id,
                "native_import",
                _nonempty_string(
                    path.get("source_format"),
                    f"{label}.implemented_paths[{path_index}].source_format",
                ),
            )
            if key in base_routes:
                raise ConformanceError(f"duplicate native import matrix route {key}")
            base_routes.add(key)
            if key == LOCAL_DEEPAGENTS_ROUTE:
                base_routes.add(HOSTED_BOUNDARY_ROUTE)
        if native_count == 0:
            raise ConformanceError(f"matrix provider {provider_id} has no native import route")
    return provider_ids, base_routes


def _validate_status(status: dict[str, Any], label: str) -> str:
    kind = _nonempty_string(status.get("kind"), f"{label}.kind")
    if kind not in ALLOWED_STATUSES:
        raise ConformanceError(f"{label} has forbidden status {kind!r}")
    if kind == "supported":
        _exact_keys(status, {"kind"}, label)
        return kind
    _exact_keys(status, {"kind", "reason"}, label)
    reason = _object(status["reason"], f"{label}.reason")
    _exact_keys(reason, {"kind", "evidence_ref"}, f"{label}.reason")
    reason_kind = _nonempty_string(reason["kind"], f"{label}.reason.kind")
    _nonempty_string(reason["evidence_ref"], f"{label}.reason.evidence_ref")
    allowed = NOT_QUALIFIED_REASONS if kind == "not_qualified" else EXCLUDED_REASONS
    if reason_kind not in allowed:
        raise ConformanceError(f"{label} has unknown {kind} reason {reason_kind!r}")
    return kind


def _claim(
    claims: CapabilityClaims, suite: str, test: str, evidence_class: str
) -> None:
    claims.setdefault(suite, {}).setdefault(test, set()).add(evidence_class)


def _validate_partition_coverage(
    schema_domains: dict[SchemaKey, ProducerDomain],
    lane_partitions: dict[SchemaKey, list[ProducerDomain]],
) -> None:
    if set(lane_partitions) != set(schema_domains):
        raise ConformanceError(
            "capability lanes do not cover every schema generation: "
            f"missing={sorted(set(schema_domains) - set(lane_partitions))}, "
            f"unclaimed={sorted(set(lane_partitions) - set(schema_domains))}"
        )
    for schema_key, domain in schema_domains.items():
        partitions = lane_partitions[schema_key]
        if any(partition.kind != domain.kind for partition in partitions):
            raise ConformanceError(
                f"producer partitions for schema {schema_key} use a different grammar"
            )
        if domain.kind == "discrete":
            claimed: set[str] = set()
            for partition in partitions:
                outside = partition.points - domain.points
                if outside:
                    raise ConformanceError(
                        f"producer partition for schema {schema_key} exceeds inventoried "
                        f"generations {sorted(outside)}"
                    )
                overlap = claimed & partition.points
                if overlap:
                    raise ConformanceError(
                        f"overlapping producer partitions for schema {schema_key}: "
                        f"{sorted(overlap)}"
                    )
                claimed.update(partition.points)
            if claimed != domain.points:
                raise ConformanceError(
                    f"incomplete producer partition for schema {schema_key}: "
                    f"missing={sorted(domain.points - claimed)}"
                )
            continue

        if domain.kind == "hosted_boundary":
            if len(partitions) != 1:
                raise ConformanceError(
                    f"hosted boundary schema {schema_key} must have exactly one lane"
                )
            continue

        ordered = sorted(partitions, key=lambda partition: partition.lower)
        cursor = domain.lower
        for partition in ordered:
            assert cursor is not None
            assert partition.lower is not None and partition.upper is not None
            assert domain.upper is not None
            if partition.lower < cursor:
                raise ConformanceError(
                    f"overlapping producer ranges for schema {schema_key}"
                )
            if partition.lower > cursor:
                raise ConformanceError(f"incomplete producer range for schema {schema_key}")
            if partition.upper > domain.upper:
                raise ConformanceError(
                    f"producer range for schema {schema_key} exceeds inventoried domain"
                )
            cursor = partition.upper
        if cursor != domain.upper:
            raise ConformanceError(f"incomplete producer range for schema {schema_key}")


def validate_manifest(
    manifest: dict[str, Any],
    provider_matrix: dict[str, Any],
    repository_root: Path | None = None,
    route_contracts: RouteContracts | None = None,
    capability_audit: dict[str, Any] | None = None,
) -> ConformanceInventory:
    """Validate base routes, exact schema partitions, and closed evidence."""

    repository_root = repository_root or _default_repository_root()
    route_contracts = ROUTE_SCHEMA_CONTRACTS if route_contracts is None else route_contracts
    closed_contracts = _validate_route_contract_registry(
        route_contracts, repository_root
    )
    local_deepagents_contracts = [
        key for key in closed_contracts if key[:3] == LOCAL_DEEPAGENTS_ROUTE
    ]
    if len(local_deepagents_contracts) > 1:
        raise ConformanceError(
            "closed route schema registry contains duplicate local DeepAgents contracts"
        )
    has_hosted_boundary = bool(local_deepagents_contracts)
    _exact_keys(
        manifest,
        {
            "schema_version",
            "capability",
            "capability_revision",
            "expected_provider_count",
            "expected_base_route_count",
            "expected_schema_generation_count",
            "expected_capability_lane_count",
            "expected_status_rows",
            "expected_provider_status_rows",
            "required_supported_evidence_classes",
            "base_routes",
            "schema_generations",
            "capability_lanes",
        },
        "manifest",
    )
    if manifest["schema_version"] != MANIFEST_SCHEMA_VERSION:
        raise ConformanceError(
            f"manifest schema_version must equal {MANIFEST_SCHEMA_VERSION}"
        )
    if manifest["capability"] != "mcp_tool_call_attribution":
        raise ConformanceError("manifest capability is not mcp_tool_call_attribution")
    if manifest["capability_revision"] != CAPABILITY_REVISION:
        raise ConformanceError(
            f"manifest capability_revision must equal {CAPABILITY_REVISION}"
        )
    expected_provider_count = _positive_int(
        manifest["expected_provider_count"], "expected_provider_count"
    )
    expected_base_route_count = _positive_int(
        manifest["expected_base_route_count"], "expected_base_route_count"
    )
    expected_schema_count = _positive_int(
        manifest["expected_schema_generation_count"],
        "expected_schema_generation_count",
    )
    expected_lane_count = _positive_int(
        manifest["expected_capability_lane_count"],
        "expected_capability_lane_count",
    )

    required_classes = _list(
        manifest["required_supported_evidence_classes"],
        "required_supported_evidence_classes",
    )
    if len(required_classes) != len(set(required_classes)):
        raise ConformanceError("required_supported_evidence_classes contains duplicates")
    if set(required_classes) != REQUIRED_EVIDENCE_CLASSES:
        raise ConformanceError(
            "required supported evidence classes differ: "
            f"missing={sorted(REQUIRED_EVIDENCE_CLASSES - set(required_classes))}, "
            f"unknown={sorted(set(required_classes) - REQUIRED_EVIDENCE_CLASSES)}"
        )

    expected_status_rows = _object(
        manifest["expected_status_rows"], "expected_status_rows"
    )
    _exact_keys(expected_status_rows, ALLOWED_STATUSES, "expected_status_rows")
    for kind, count in expected_status_rows.items():
        if not isinstance(count, int) or isinstance(count, bool) or count < 0:
            raise ConformanceError(f"expected_status_rows.{kind} must be nonnegative")
    if sum(expected_status_rows.values()) != expected_lane_count:
        raise ConformanceError(
            "expected status rows do not add up to expected_capability_lane_count"
        )
    expected_provider_status_rows = _object(
        manifest["expected_provider_status_rows"], "expected_provider_status_rows"
    )
    _exact_keys(
        expected_provider_status_rows, ALLOWED_STATUSES, "expected_provider_status_rows"
    )
    for kind, count in expected_provider_status_rows.items():
        if not isinstance(count, int) or isinstance(count, bool) or count < 0:
            raise ConformanceError(
                f"expected_provider_status_rows.{kind} must be nonnegative"
            )
    if sum(expected_provider_status_rows.values()) != expected_provider_count:
        raise ConformanceError(
            "expected provider status rows do not add up to expected_provider_count"
        )
    if (
        expected_provider_count == EXPECTED_AUDIT_COUNTS["providers"]
        and expected_provider_status_rows != EXPECTED_PROVIDER_STATUS_ROWS
    ):
        raise ConformanceError(
            "expected provider status rows differ: "
            f"expected={EXPECTED_PROVIDER_STATUS_ROWS}, "
            f"actual={expected_provider_status_rows}"
        )
    matrix_providers, matrix_routes = _matrix_inventory(provider_matrix)
    if len(matrix_providers) != expected_provider_count:
        raise ConformanceError(
            f"matrix provider count {len(matrix_providers)} differs from "
            f"manifest expectation {expected_provider_count}"
        )
    if len(matrix_routes) != expected_base_route_count:
        raise ConformanceError(
            f"matrix base route count {len(matrix_routes)} differs from "
            f"manifest expectation {expected_base_route_count}"
        )

    declared_routes: set[BaseKey] = set()
    for route_index, raw_route in enumerate(_list(manifest["base_routes"], "base_routes")):
        label = f"base_routes[{route_index}]"
        route = _object(raw_route, label)
        _exact_keys(route, {"provider", "route", "source_format"}, label)
        for field in ["provider", "route", "source_format"]:
            _nonempty_string(route[field], f"{label}.{field}")
        key = _base_key(route)
        if key in declared_routes:
            raise ConformanceError(f"duplicate base route {key}")
        declared_routes.add(key)
    if len(declared_routes) != expected_base_route_count:
        raise ConformanceError(
            f"base route row count {len(declared_routes)} differs from expected "
            f"count {expected_base_route_count}"
        )
    if declared_routes != matrix_routes:
        raise ConformanceError(
            "base route inventory differs from provider matrix: "
            f"missing={sorted(matrix_routes - declared_routes)}, "
            f"unclaimed={sorted(declared_routes - matrix_routes)}"
        )
    contract_routes = {_base_key_from_schema(key) for key in closed_contracts}
    if has_hosted_boundary:
        contract_routes.add(HOSTED_BOUNDARY_ROUTE)
    if contract_routes != declared_routes:
        raise ConformanceError(
            "closed route schema registry differs from the route inventory: "
            f"missing={sorted(declared_routes - contract_routes)}, "
            f"unknown={sorted(contract_routes - declared_routes)}"
        )

    schema_domains: dict[SchemaKey, ProducerDomain] = {}
    schema_contract_domains: dict[SchemaKey, dict[str, Any]] = {}
    seen_contracts: set[SchemaKey] = set()
    manifest_digest_owners: dict[str, BaseKey] = {}
    for schema_index, raw_schema in enumerate(
        _list(manifest["schema_generations"], "schema_generations")
    ):
        label = f"schema_generations[{schema_index}]"
        schema = _object(raw_schema, label)
        _exact_keys(
            schema,
            {
                "provider",
                "route",
                "source_format",
                "format_schema",
                "producer_domain",
                "provenance",
            },
            label,
        )
        for field in ["provider", "route", "source_format"]:
            _nonempty_string(schema[field], f"{label}.{field}")
        if _base_key(schema) not in declared_routes:
            raise ConformanceError(
                f"schema generation has unclaimed base route {_base_key(schema)}"
            )
        _validate_format_schema(schema["format_schema"], f"{label}.format_schema")
        key = _schema_key(schema)
        if _base_key(schema) == HOSTED_BOUNDARY_ROUTE:
            raise ConformanceError(
                f"{label} cannot declare a schema generation for a hosted boundary"
            )
        digest = schema["format_schema"]["shape_sha256"]
        prior_digest_owner = manifest_digest_owners.get(digest)
        if prior_digest_owner is not None:
            raise ConformanceError(
                f"route shape digest {digest} is reused by "
                f"{prior_digest_owner} and {_base_key(schema)}"
            )
        manifest_digest_owners[digest] = _base_key(schema)
        contract = closed_contracts.get(key)
        if contract is None:
            raise ConformanceError(
                f"schema generation {key} is not in the closed route schema registry"
            )
        _validate_provenance(
            schema["provenance"],
            f"{label}.provenance",
            contract,
        )
        if key in schema_domains:
            raise ConformanceError(f"duplicate schema generation {key}")
        schema_domains[key] = _validate_producer_domain(
            schema["producer_domain"], f"{label}.producer_domain"
        )
        schema_contract_domains[key] = contract.producer_domain
        seen_contracts.add(key)
    if len(schema_domains) != expected_schema_count:
        raise ConformanceError(
            f"schema generation count {len(schema_domains)} differs from expected "
            f"count {expected_schema_count}"
        )
    if seen_contracts != set(closed_contracts):
        raise ConformanceError(
            "schema generations differ from the closed route schema registry: "
            f"missing={sorted(set(closed_contracts) - seen_contracts)}, "
            f"unknown={sorted(seen_contracts - set(closed_contracts))}"
        )
    schema_routes = {_base_key_from_schema(key) for key in schema_domains}
    imported_routes = declared_routes - {HOSTED_BOUNDARY_ROUTE}
    if schema_routes != imported_routes:
        raise ConformanceError(
            "schema generations do not cover every imported base route: "
            f"missing={sorted(imported_routes - schema_routes)}, "
            f"unknown={sorted(schema_routes - imported_routes)}"
        )

    lanes = _list(manifest["capability_lanes"], "capability_lanes")
    if not lanes:
        raise ConformanceError("capability lane inventory is empty")
    if len(lanes) != expected_lane_count:
        raise ConformanceError(
            f"capability lane count {len(lanes)} differs from expected {expected_lane_count}"
        )

    seen_lanes: set[LaneKey] = set()
    lane_partitions: dict[SchemaKey, list[ProducerDomain]] = {}
    evidence_owner: dict[tuple[str, str, str], LaneKey] = {}
    evidence_capability: dict[tuple[str, str, str], str] = {}
    evidence_scope: dict[tuple[str, str, str], str] = {}
    public_claims: CapabilityClaims = {}
    status_rows = {kind: 0 for kind in ALLOWED_STATUSES}

    for lane_index, raw_lane in enumerate(lanes):
        label = f"capability_lanes[{lane_index}]"
        lane = _object(raw_lane, label)
        _exact_keys(
            lane,
            {
                "provider",
                "route",
                "source_format",
                "source_schema",
                "format_schema",
                "producer_partition",
                "producer_bound",
                "status",
                "evidence",
            },
            label,
        )
        for field in ["provider", "route", "source_format", "source_schema"]:
            _nonempty_string(lane[field], f"{label}.{field}")
        _validate_format_schema(lane["format_schema"], f"{label}.format_schema")
        schema_key = _schema_key(lane)
        is_hosted_boundary = _base_key(lane) == HOSTED_BOUNDARY_ROUTE
        if is_hosted_boundary:
            if (
                lane["source_schema"] != HOSTED_BOUNDARY_SOURCE_SCHEMA
                or lane["format_schema"] != HOSTED_BOUNDARY_FORMAT_SCHEMA
                or lane["producer_partition"] != HOSTED_BOUNDARY_PRODUCER_DOMAIN
            ):
                raise ConformanceError(
                    f"{label} differs from the closed hosted boundary contract"
                )
        elif schema_key not in schema_domains:
            raise ConformanceError(
                f"capability lane has unclaimed schema generation {schema_key}"
            )
        partition = _validate_producer_domain(
            lane["producer_partition"],
            f"{label}.producer_partition",
            is_partition=True,
        )
        key = _lane_key(lane)
        if key in seen_lanes:
            raise ConformanceError(f"duplicate full capability tuple {key}")
        seen_lanes.add(key)
        if not is_hosted_boundary:
            lane_partitions.setdefault(schema_key, []).append(partition)

        status = _validate_status(
            _object(lane["status"], f"{label}.status"), f"{label}.status"
        )
        if is_hosted_boundary and status != "excluded":
            raise ConformanceError(f"{label} hosted boundary must be excluded")
        status_rows[status] += 1

        evidence_by_class: dict[str, list[dict[str, Any]]] = {
            evidence_class: [] for evidence_class in REQUIRED_EVIDENCE_CLASSES
        }
        seen_evidence: set[tuple[tuple[str, str, str], str, str]] = set()
        for evidence_index, raw_evidence in enumerate(
            _list(lane["evidence"], f"{label}.evidence")
        ):
            evidence_label = f"{label}.evidence[{evidence_index}]"
            evidence = _object(raw_evidence, evidence_label)
            kind = _nonempty_string(evidence.get("kind"), f"{evidence_label}.kind")
            if kind == "rust_test":
                _exact_keys(
                    evidence,
                    {"class", "kind", "suite", "test", "scope"},
                    evidence_label,
                )
                suite = _nonempty_string(evidence["suite"], f"{evidence_label}.suite")
                test_name = _nonempty_string(evidence["test"], f"{evidence_label}.test")
                claim_key = ("public", suite, test_name)
                claims = public_claims
            else:
                raise ConformanceError(f"{evidence_label} has unknown kind {kind!r}")

            evidence_class = _nonempty_string(
                evidence["class"], f"{evidence_label}.class"
            )
            if evidence_class not in REQUIRED_EVIDENCE_CLASSES:
                raise ConformanceError(
                    f"{evidence_label} has unknown evidence class {evidence_class!r}"
                )
            scope = _nonempty_string(evidence["scope"], f"{evidence_label}.scope")
            if scope not in EVIDENCE_SCOPES:
                raise ConformanceError(f"{evidence_label} has unknown scope {scope!r}")
            if (
                scope == "provider_neutral"
                and evidence_class not in PROVIDER_NEUTRAL_EVIDENCE_CLASSES
            ):
                raise ConformanceError(
                    f"{evidence_label} class {evidence_class!r} cannot be provider-neutral"
                )
            prior_class = evidence_capability.get(claim_key)
            if prior_class is not None and prior_class != evidence_class:
                raise ConformanceError(
                    f"executable test {claim_key[1]}::{claim_key[2]} cannot claim "
                    f"multiple evidence classes: {sorted({prior_class, evidence_class})}"
                )
            prior_owner = evidence_owner.get(claim_key)
            prior_scope = evidence_scope.get(claim_key)
            if prior_owner is not None and prior_owner != key and (
                scope != "provider_neutral" or prior_scope != "provider_neutral"
            ):
                raise ConformanceError(
                    f"executable test {claim_key[1]}::{claim_key[2]} is reused across "
                    f"tuples {prior_owner} and {key}"
                )
            if prior_scope is not None and prior_scope != scope:
                raise ConformanceError(
                    f"executable test {claim_key[1]}::{claim_key[2]} mixes evidence scopes"
                )
            evidence_identity = (claim_key, evidence_class, scope)
            if evidence_identity in seen_evidence:
                raise ConformanceError(f"duplicate evidence claim {evidence_identity} in {key}")
            seen_evidence.add(evidence_identity)
            evidence_owner[claim_key] = key
            evidence_capability[claim_key] = evidence_class
            evidence_scope[claim_key] = scope
            _claim(claims, suite, test_name, evidence_class)
            evidence_by_class[evidence_class].append(evidence)

        if status == "supported":
            if (
                lane["format_schema"].get("kind") != "structural_revision"
                or lane["format_schema"].get("revision") != 1
                or lane["producer_partition"]
                != {
                    "kind": "discrete",
                    "versions": [
                        {"kind": "unversioned_generation", "generation": 1}
                    ],
                }
                or lane["producer_bound"] != EXACT_RUNTIME_PRODUCER_BOUND
            ):
                raise ConformanceError(
                    f"supported lane {key} must bind structural unversioned generation 1"
                )
            missing_classes = []
            for evidence_class in REQUIRED_EVIDENCE_CLASSES:
                accepted_scopes = {"tuple"}
                if evidence_class in PROVIDER_NEUTRAL_EVIDENCE_CLASSES:
                    accepted_scopes.add("provider_neutral")
                if not any(
                    evidence["kind"] == "rust_test"
                    and evidence["scope"] in accepted_scopes
                    for evidence in evidence_by_class[evidence_class]
                ):
                    missing_classes.append(evidence_class)
            if missing_classes:
                raise ConformanceError(
                    f"supported lane {key} is missing required executable classes "
                    f"{sorted(missing_classes)}"
                )
    _validate_partition_coverage(schema_domains, lane_partitions)
    for schema_key, contract_domain in schema_contract_domains.items():
        schema_index = next(
            index
            for index, raw_schema in enumerate(manifest["schema_generations"])
            if _schema_key(raw_schema) == schema_key
        )
        manifest_domain = manifest["schema_generations"][schema_index][
            "producer_domain"
        ]
        if manifest_domain != contract_domain:
            raise ConformanceError(
                f"schema_generations[{schema_index}].producer_domain differs from "
                "the closed route schema registry"
            )
    if {key[0] for key in seen_lanes} != matrix_providers:
        raise ConformanceError("capability lane provider partition differs from matrix")
    if status_rows != expected_status_rows:
        raise ConformanceError(
            f"status rows differ: expected={expected_status_rows}, actual={status_rows}"
        )
    _validate_capability_audit_projection(manifest, capability_audit)

    return ConformanceInventory(
        provider_count=len(matrix_providers),
        base_route_count=len(declared_routes),
        schema_generation_count=len(schema_domains),
        capability_lane_count=len(seen_lanes),
        status_rows=status_rows,
        public_claims=public_claims,
    )


def _base_key_from_schema(key: SchemaKey) -> BaseKey:
    return key[:3]


def _listed_tests(binary: Path) -> set[str]:
    result = subprocess.run(
        [str(binary), "--list"],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if result.returncode != 0:
        raise ConformanceError(
            f"test binary {binary} --list failed with {result.returncode}: {result.stderr}"
        )
    tests = {
        line.removesuffix(": test")
        for line in result.stdout.splitlines()
        if line.endswith(": test")
    }
    if not tests:
        raise ConformanceError(f"test binary {binary} listed zero tests")
    return tests


def _run_suite(
    suite_id: str,
    binary: Path,
    expected_tests: set[str],
    selected_inventory: bool,
    temp_root: Path,
) -> None:
    suite_temp = temp_root / suite_id
    suite_temp.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment["TEST_TMPDIR"] = str(suite_temp)
    environment["TMPDIR"] = str(suite_temp)
    selections: list[str | None] = (
        sorted(expected_tests) if selected_inventory else [None]
    )
    for test_name in selections:
        command = [str(binary)]
        expected_count = len(expected_tests)
        selection_label = suite_id
        if test_name is not None:
            command.extend([test_name, "--exact"])
            expected_count = 1
            selection_label = f"{suite_id}::{test_name}"
        command.extend(["--color", "never", "--test-threads=1"])
        result = subprocess.run(
            command,
            check=False,
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=environment,
        )
        output = result.stdout + result.stderr
        summaries = [
            match
            for line in output.splitlines()
            if (match := SUMMARY_RE.match(line)) is not None
        ]
        if len(summaries) != 1:
            raise ConformanceError(
                f"suite {selection_label} produced {len(summaries)} test summaries; "
                f"expected one\n{output}"
            )
        summary = summaries[0]
        passed, failed, ignored = (int(summary.group(index)) for index in (2, 3, 4))
        errors: list[str] = []
        if result.returncode != 0:
            errors.append(f"binary exited {result.returncode}")
        if summary.group(1) != "ok":
            errors.append("summary status was not ok")
        if passed != expected_count:
            errors.append(
                f"executed pass count {passed} differs from manifest count {expected_count}"
            )
        if failed != 0:
            errors.append(f"selected tests failed: {failed}")
        if ignored != 0:
            errors.append(f"selected tests were ignored: {ignored}")
        if errors:
            raise ConformanceError(
                f"suite {selection_label}: {'; '.join(errors)}\n{output}"
            )


def _validate_and_run_bindings(
    kind: str,
    claims: CapabilityClaims,
    bindings: SuiteBindings,
    capabilities: CapabilityClaims,
    temp_root: Path,
) -> None:
    expected_ids = set(claims)
    provided_ids = set(bindings)
    capability_ids = set(capabilities)
    if expected_ids != provided_ids:
        raise ConformanceError(
            f"{kind} suite bindings differ from manifest evidence: "
            f"missing={sorted(expected_ids - provided_ids)}, "
            f"stale={sorted(provided_ids - expected_ids)}"
        )
    if expected_ids != capability_ids:
        raise ConformanceError(
            f"{kind} capability bindings differ from manifest evidence: "
            f"missing={sorted(expected_ids - capability_ids)}, "
            f"stale={sorted(capability_ids - expected_ids)}"
        )
    for suite_id in sorted(expected_ids):
        binding = bindings[suite_id]
        binary = binding.binary
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise ConformanceError(
                f"{kind} suite {suite_id} target {binding.target} is not executable: "
                f"{binary}"
            )
        expected_tests = set(claims[suite_id])
        declared_tests = set(capabilities[suite_id])
        if expected_tests != declared_tests:
            raise ConformanceError(
                f"{kind} suite {suite_id} capability test inventory differs: "
                f"missing={sorted(expected_tests - declared_tests)}, "
                f"stale={sorted(declared_tests - expected_tests)}"
            )
        for test_name in sorted(expected_tests):
            claimed = claims[suite_id][test_name]
            declared = capabilities[suite_id][test_name]
            if claimed != declared:
                raise ConformanceError(
                    f"{kind} executable {binding.target}::{test_name} evidence "
                    f"capabilities differ: undeclared={sorted(claimed - declared)}, "
                    f"unclaimed={sorted(declared - claimed)}"
                )
        actual = _listed_tests(binary)
        missing_actual = expected_tests - actual
        unclaimed_actual = actual - expected_tests
        if missing_actual or (unclaimed_actual and not binding.selected_inventory):
            raise ConformanceError(
                f"{kind} suite {suite_id} test inventory differs: "
                f"missing={sorted(missing_actual)}, "
                f"unclaimed={sorted(unclaimed_actual)}"
            )
        _run_suite(
            suite_id,
            binary,
            expected_tests,
            binding.selected_inventory,
            temp_root / kind,
        )


def _validate_physical_bindings(*groups: tuple[str, SuiteBindings]) -> None:
    targets: dict[str, tuple[str, str]] = {}
    binaries: dict[tuple[int, int], tuple[str, str, str]] = {}
    for kind, bindings in groups:
        for suite_id, binding in bindings.items():
            if BAZEL_TARGET_RE.fullmatch(binding.target) is None:
                raise ConformanceError(
                    f"{kind} suite {suite_id} has invalid Bazel target {binding.target!r}"
                )
            prior_target = targets.get(binding.target)
            if prior_target is not None:
                raise ConformanceError(
                    f"duplicate physical Bazel target binding {binding.target}: "
                    f"{prior_target[0]} suite {prior_target[1]} and {kind} suite {suite_id}"
                )
            targets[binding.target] = (kind, suite_id)
            if not binding.binary.is_file() or not os.access(binding.binary, os.X_OK):
                continue
            metadata = binding.binary.stat()
            physical_binary = (metadata.st_dev, metadata.st_ino)
            prior_binary = binaries.get(physical_binary)
            if prior_binary is not None:
                raise ConformanceError(
                    f"duplicate physical test binary binding {binding.binary.resolve()}: "
                    f"{prior_binary[0]} suite {prior_binary[1]} target "
                    f"{prior_binary[2]} and {kind} suite {suite_id} target {binding.target}"
                )
            binaries[physical_binary] = (kind, suite_id, binding.target)


def run_conformance(
    manifest: dict[str, Any],
    provider_matrix: dict[str, Any],
    public_suite_binaries: SuiteBindings,
    public_capabilities: CapabilityClaims,
    temp_root: Path,
    mode: str = PUBLIC_VALIDATION_MODE,
    repository_root: Path | None = None,
    route_contracts: RouteContracts | None = None,
) -> ConformanceInventory:
    if mode not in CONFORMANCE_MODES:
        raise ConformanceError(f"unknown conformance mode {mode!r}")
    inventory = validate_manifest(
        manifest,
        provider_matrix,
        repository_root,
        route_contracts,
    )
    _validate_physical_bindings(("public", public_suite_binaries))
    _validate_and_run_bindings(
        "public",
        inventory.public_claims,
        public_suite_binaries,
        public_capabilities,
        temp_root,
    )
    return inventory


def _parse_suite_bindings(
    values: list[str], flag: str, *, selected_inventory: bool = False
) -> SuiteBindings:
    bindings: SuiteBindings = {}
    for value in values:
        parts = value.split("=", 2)
        if len(parts) != 3 or not all(parts):
            raise ConformanceError(
                f"invalid {flag} binding {value!r}; expected ID=TARGET=PATH"
            )
        suite_id, target, path = parts
        if BAZEL_TARGET_RE.fullmatch(target) is None:
            raise ConformanceError(
                f"invalid {flag} target {target!r}; expected absolute Bazel target"
            )
        if suite_id in bindings:
            raise ConformanceError(f"duplicate {flag} binding {suite_id}")
        bindings[suite_id] = SuiteBinding(
            target=target,
            binary=Path(path),
            selected_inventory=selected_inventory,
        )
    return bindings


def _merge_suite_bindings(*groups: SuiteBindings) -> SuiteBindings:
    merged: SuiteBindings = {}
    for bindings in groups:
        overlap = set(merged) & set(bindings)
        if overlap:
            raise ConformanceError(f"duplicate suite binding IDs {sorted(overlap)}")
        merged.update(bindings)
    return merged


def _parse_capability_bindings(values: list[str], flag: str) -> CapabilityClaims:
    bindings: CapabilityClaims = {}
    for value in values:
        identity, separator, raw_classes = value.partition("=")
        suite_id, suite_separator, test_name = identity.partition("::")
        if (
            not separator
            or not suite_separator
            or not suite_id
            or not test_name
            or not raw_classes
        ):
            raise ConformanceError(
                f"invalid {flag} binding {value!r}; expected SUITE::TEST=CLASS[,CLASS]"
            )
        classes = raw_classes.split(",")
        if len(classes) != len(set(classes)):
            raise ConformanceError(f"duplicate class in {flag} binding {value!r}")
        if len(classes) != 1:
            raise ConformanceError(
                f"{flag} binding {value!r} must claim exactly one evidence class"
            )
        unknown = set(classes) - REQUIRED_EVIDENCE_CLASSES
        if unknown:
            raise ConformanceError(
                f"{flag} binding {value!r} has unknown evidence classes {sorted(unknown)}"
            )
        suite = bindings.setdefault(suite_id, {})
        if test_name in suite:
            raise ConformanceError(f"duplicate {flag} binding {suite_id}::{test_name}")
        suite[test_name] = set(classes)
    return bindings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("provider_matrix", type=Path)
    parser.add_argument("--mode", choices=sorted(CONFORMANCE_MODES), required=True)
    parser.add_argument("--suite", action="append", default=[])
    parser.add_argument("--suite-alias", action="append", default=[])
    parser.add_argument("--test-capability", action="append", default=[])
    args = parser.parse_args(argv)
    try:
        inventory = run_conformance(
            _load_json(args.manifest, "attribution manifest"),
            _load_json(args.provider_matrix, "provider matrix"),
            _merge_suite_bindings(
                _parse_suite_bindings(args.suite, "--suite"),
                _parse_suite_bindings(
                    args.suite_alias,
                    "--suite-alias",
                    selected_inventory=True,
                ),
            ),
            _parse_capability_bindings(args.test_capability, "--test-capability"),
            Path(os.environ.get("TEST_TMPDIR", "/tmp")) / "mcp-attribution",
            mode=args.mode,
        )
    except ConformanceError as error:
        print(f"MCP attribution conformance failed: {error}", file=sys.stderr)
        return 1
    status = ", ".join(
        f"{kind}={inventory.status_rows[kind]}"
        for kind in ["supported", "not_qualified", "excluded"]
    )
    print(
        "MCP attribution conformance passed: "
        f"mode={args.mode}, "
        f"providers={inventory.provider_count}, "
        f"base_routes={inventory.base_route_count}, "
        f"schema_generations={inventory.schema_generation_count}, "
        f"capability_lanes={inventory.capability_lane_count}, "
        f"status_rows=({status}), "
        f"public_executable_tests={inventory.public_executable_count}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
