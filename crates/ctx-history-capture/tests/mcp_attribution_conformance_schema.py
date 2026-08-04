"""Strict schema primitives for MCP attribution conformance."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import date
import json
from pathlib import Path
import re
from typing import Any


MANIFEST_SCHEMA_VERSION = 6
CAPABILITY_REVISION = 5
PUBLIC_VALIDATION_MODE = "public-validation"
CONFORMANCE_MODES = {PUBLIC_VALIDATION_MODE}

AUDIT_STATUS_TO_MANIFEST = {
    "exact": "supported",
    "not-qualified": "not_qualified",
    "excluded": "excluded",
}
EXPECTED_PROVIDER_STATUS_ROWS = {
    "supported": 3,
    "not_qualified": 38,
    "excluded": 0,
}
EXPECTED_AUDIT_COUNTS = {
    "providers": 41,
    "base_routes": 43,
    "capability_lanes": 46,
    "lane_statuses": {"exact": 3, "not-qualified": 42, "excluded": 1},
    "provider_statuses": {"exact": 3, "not-qualified": 38, "excluded": 0},
}
LOCAL_DEEPAGENTS_ROUTE = (
    "deepagents",
    "native_import",
    "deepagents_sessions_sqlite",
)
HOSTED_BOUNDARY_ROUTE = ("deepagents", "hosted_trace", "langsmith_trace")
HOSTED_BOUNDARY_SOURCE_SCHEMA = "hosted-trace-v1"
HOSTED_BOUNDARY_FORMAT_SCHEMA = {
    "kind": "hosted_boundary",
    "source_schema": HOSTED_BOUNDARY_SOURCE_SCHEMA,
}
HOSTED_BOUNDARY_PRODUCER_DOMAIN = {"kind": "hosted_boundary"}
EXACT_RUNTIME_PRODUCER_BOUND = {
    "kind": "unversioned",
    "generation": 1,
    "unknown_generations": "not-qualified",
}

ALLOWED_STATUSES = {"supported", "not_qualified", "excluded"}
NOT_QUALIFIED_REASONS = {
    "extraction_not_implemented",
    "format_not_audited",
    "identity_not_proven",
    "required_evidence_incomplete",
}
EXCLUDED_REASONS = {
    "hosted_only",
    "no_local_history",
    "provider_boundary_prohibits_local_capture",
}
REQUIRED_EVIDENCE_CLASSES = {
    "exact_positive_pair",
    "canonical_terminal_outcomes",
    "malformed_identity",
    "ambiguity_duplicate_linkage",
    "exact_boundary",
    "max_plus_one",
    "result_preservation",
    "stable_ids",
    "search_nonindexing",
    "privacy_sinks",
}
EVIDENCE_SCOPES = {"tuple", "provider_neutral"}
PROVIDER_NEUTRAL_EVIDENCE_CLASSES = {
    "max_plus_one",
    "privacy_sinks",
    "search_nonindexing",
}
SEMVER_RE = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
DATE_RE = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}")
SHA256_RE = re.compile(r"[0-9a-f]{64}")
GIT_SHA_RE = re.compile(r"[0-9a-f]{40}")
AUDIT_VERSION_RE = re.compile(
    r"[0-9]+(?:\.[0-9]+){1,3}(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?"
)
BAZEL_TARGET_RE = re.compile(r"//[A-Za-z0-9_./+-]*:[A-Za-z0-9_./+-]+")
SUMMARY_RE = re.compile(
    r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;"
)

BaseKey = tuple[str, str, str]
SchemaKey = tuple[str, str, str, str]
LaneKey = tuple[str, str, str, str, str]
AuditLaneKey = tuple[str, str, str, str, str]


class ConformanceError(RuntimeError):
    """Raised when the reviewed attribution inventory stops being authoritative."""


@dataclass(frozen=True)
class ProducerDomain:
    kind: str
    points: frozenset[str] = frozenset()
    lower: tuple[int, int, int] | None = None
    upper: tuple[int, int, int] | None = None


def _object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ConformanceError(f"{label} must be an object")
    return value


def _list(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise ConformanceError(f"{label} must be an array")
    return value


def _exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        raise ConformanceError(
            f"{label} keys differ: missing={sorted(expected - actual)}, "
            f"unknown={sorted(actual - expected)}"
        )


def _nonempty_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ConformanceError(f"{label} must be a nonempty string")
    return value


def _positive_int(value: Any, label: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        raise ConformanceError(f"{label} must be a positive integer")
    return value


def _nonnegative_int(value: Any, label: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise ConformanceError(f"{label} must be a nonnegative integer")
    return value


def _canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def _load_json(path: Path, label: str) -> dict[str, Any]:
    try:
        return _object(json.loads(path.read_text(encoding="utf-8")), label)
    except (OSError, json.JSONDecodeError) as error:
        raise ConformanceError(f"cannot read {label} {path}: {error}") from error


def _base_key(row: dict[str, Any]) -> BaseKey:
    return (row["provider"], row["route"], row["source_format"])


def _schema_key(row: dict[str, Any]) -> SchemaKey:
    return (*_base_key(row), _canonical(row["format_schema"]))


def _lane_key(lane: dict[str, Any]) -> LaneKey:
    return (*_schema_key(lane), _canonical(lane["producer_partition"]))


def _semver(value: Any, label: str) -> tuple[int, int, int]:
    version = _nonempty_string(value, label)
    match = SEMVER_RE.fullmatch(version)
    if match is None:
        raise ConformanceError(
            f"{label} must use strict MAJOR.MINOR.PATCH numeric grammar"
        )
    return tuple(int(component) for component in match.groups())


def _calendar_date(value: Any, label: str) -> tuple[int, int, int]:
    version = _nonempty_string(value, label)
    if DATE_RE.fullmatch(version) is None:
        raise ConformanceError(f"{label} must use strict YYYY-MM-DD grammar")
    try:
        parsed = date.fromisoformat(version)
    except ValueError as error:
        raise ConformanceError(f"{label} is not a real calendar date") from error
    return (parsed.year, parsed.month, parsed.day)


def _sha256(value: Any, label: str) -> str:
    digest = _nonempty_string(value, label)
    if SHA256_RE.fullmatch(digest) is None:
        raise ConformanceError(f"{label} must be a lowercase SHA-256 digest")
    return digest


def _validate_format_schema(value: Any, label: str) -> str:
    schema = _object(value, label)
    kind = _nonempty_string(schema.get("kind"), f"{label}.kind")
    if kind == "structural_revision":
        _exact_keys(schema, {"kind", "revision", "shape_sha256"}, label)
        _positive_int(schema["revision"], f"{label}.revision")
    elif kind == "embedded_integer":
        _exact_keys(schema, {"kind", "version", "shape_sha256"}, label)
        _nonnegative_int(schema["version"], f"{label}.version")
    elif kind == "embedded_semver":
        _exact_keys(schema, {"kind", "version", "shape_sha256"}, label)
        _semver(schema["version"], f"{label}.version")
    elif kind == "hosted_boundary":
        _exact_keys(schema, {"kind", "source_schema"}, label)
        source_schema = _nonempty_string(
            schema["source_schema"], f"{label}.source_schema"
        )
        if source_schema != HOSTED_BOUNDARY_SOURCE_SCHEMA:
            raise ConformanceError(f"{label}.source_schema is not the hosted boundary")
        return _canonical(schema)
    else:
        raise ConformanceError(f"{label} has unknown schema grammar {kind!r}")
    _sha256(schema["shape_sha256"], f"{label}.shape_sha256")
    return _canonical(schema)


def _validate_version_point(value: Any, label: str) -> str:
    point = _object(value, label)
    kind = _nonempty_string(point.get("kind"), f"{label}.kind")
    if kind == "unversioned_generation":
        _exact_keys(point, {"kind", "generation"}, label)
        _positive_int(point["generation"], f"{label}.generation")
    elif kind == "semver":
        _exact_keys(point, {"kind", "version"}, label)
        _semver(point["version"], f"{label}.version")
    elif kind == "integer":
        _exact_keys(point, {"kind", "version"}, label)
        _nonnegative_int(point["version"], f"{label}.version")
    elif kind == "calendar_date":
        _exact_keys(point, {"kind", "version"}, label)
        _calendar_date(point["version"], f"{label}.version")
    else:
        raise ConformanceError(f"{label} has unknown producer version grammar {kind!r}")
    return _canonical(point)


def _validate_producer_domain(
    value: Any, label: str, *, is_partition: bool = False
) -> ProducerDomain:
    domain = _object(value, label)
    kind = _nonempty_string(domain.get("kind"), f"{label}.kind")
    if kind == "discrete":
        _exact_keys(domain, {"kind", "versions"}, label)
        versions = _list(domain["versions"], f"{label}.versions")
        if not versions:
            raise ConformanceError(f"{label}.versions must not be empty")
        if is_partition and len(versions) != 1:
            raise ConformanceError(
                f"{label} must contain exactly one discrete producer version"
            )
        points = [
            _validate_version_point(version, f"{label}.versions[{index}]")
            for index, version in enumerate(versions)
        ]
        if len(points) != len(set(points)):
            raise ConformanceError(f"{label}.versions contains duplicate generations")
        return ProducerDomain(kind=kind, points=frozenset(points))
    if kind == "semver_interval":
        _exact_keys(domain, {"kind", "lower_inclusive", "upper_exclusive"}, label)
        lower = _semver(domain["lower_inclusive"], f"{label}.lower_inclusive")
        upper = _semver(domain["upper_exclusive"], f"{label}.upper_exclusive")
        if lower >= upper:
            raise ConformanceError(f"{label} must be a nonempty half-open interval")
        return ProducerDomain(kind=kind, lower=lower, upper=upper)
    if kind == "hosted_boundary":
        _exact_keys(domain, {"kind"}, label)
        return ProducerDomain(kind=kind)
    raise ConformanceError(f"{label} has unknown producer partition grammar {kind!r}")


def _validate_audit_producer_bound(
    value: Any, label: str, audit_status: str
) -> str:
    bound = _object(value, label)
    kind = _nonempty_string(bound.get("kind"), f"{label}.kind")
    expected_unknown = "excluded" if audit_status == "excluded" else "not-qualified"
    if bound.get("unknown_generations") != expected_unknown:
        raise ConformanceError(
            f"{label}.unknown_generations must be {expected_unknown!r} for "
            f"status {audit_status!r}"
        )
    if kind == "unversioned":
        _exact_keys(bound, {"kind", "generation", "unknown_generations"}, label)
        _positive_int(bound["generation"], f"{label}.generation")
    elif kind == "source_commits":
        _exact_keys(bound, {"kind", "commits", "unknown_generations"}, label)
        commits = _list(bound["commits"], f"{label}.commits")
        if not commits:
            raise ConformanceError(f"{label}.commits must not be empty")
        for index, commit in enumerate(commits):
            if not isinstance(commit, str) or GIT_SHA_RE.fullmatch(commit) is None:
                raise ConformanceError(f"{label}.commits[{index}] is not a Git SHA")
    elif kind == "versions":
        _exact_keys(
            bound,
            {
                "kind",
                "versions",
                "ranges",
                "source_commits",
                "unknown_generations",
            },
            label,
        )
        versions = _list(bound["versions"], f"{label}.versions")
        ranges = _list(bound["ranges"], f"{label}.ranges")
        commits = _list(bound["source_commits"], f"{label}.source_commits")
        if not versions and not ranges and not commits:
            raise ConformanceError(f"{label} has no bounded producer identity")
        for index, version in enumerate(versions):
            if not isinstance(version, str) or AUDIT_VERSION_RE.fullmatch(version) is None:
                raise ConformanceError(f"{label}.versions[{index}] is not a version")
        for index, raw_range in enumerate(ranges):
            range_label = f"{label}.ranges[{index}]"
            version_range = _object(raw_range, range_label)
            _exact_keys(version_range, {"minimum", "maximum"}, range_label)
            for endpoint in ["minimum", "maximum"]:
                version = version_range[endpoint]
                if (
                    not isinstance(version, str)
                    or AUDIT_VERSION_RE.fullmatch(version) is None
                ):
                    raise ConformanceError(
                        f"{range_label}.{endpoint} is not a version"
                    )
        for index, commit in enumerate(commits):
            if not isinstance(commit, str) or GIT_SHA_RE.fullmatch(commit) is None:
                raise ConformanceError(
                    f"{label}.source_commits[{index}] is not a Git SHA"
                )
    elif kind == "hosted_boundary":
        _exact_keys(bound, {"kind", "unknown_generations"}, label)
        if audit_status != "excluded":
            raise ConformanceError(f"{label} can only describe an excluded lane")
    else:
        raise ConformanceError(f"{label} has unknown producer bound kind {kind!r}")
    return _canonical(bound)
