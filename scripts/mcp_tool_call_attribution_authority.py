"""Cross-check the docs projection against the public conformance authority."""

from __future__ import annotations

import json
from hashlib import sha256
from collections import Counter
from collections.abc import Mapping
from pathlib import Path
from typing import Any


CONFORMANCE_MANIFEST = (
    "crates/ctx-history-capture/tests/mcp-attribution-conformance.manifest.json"
)
CONFORMANCE_SUITES = "crates/ctx-history-capture/tests/mcp_attribution_suites.bzl"
CODEX_SESSION_FORMAT = "codex_session_jsonl_tree"
CODEX_HISTORY_FORMAT = "codex_history_jsonl"
CODEX_NOT_QUALIFIED_VERSIONS = ("0.200.0", "0.201.0", "0.202.0")
CONFORMANCE_AUTHORITY = {
    "manifest": CONFORMANCE_MANIFEST,
    "suite_registry": CONFORMANCE_SUITES,
    "manifest_sha256": "c80b234d154852175ec02e3c55416980906dedfcd4696c5039853b096b67c85f",
    "suite_registry_sha256": "5f78f78e4a2a0df9739313067ffddc7a11eadc075ec00910f95c352bf65d10fa",
    "manifest_schema_version": 6,
    "capability_revision": 5,
    "status_mapping": {
        "exact": "supported",
        "not-qualified": "not_qualified",
        "excluded": "excluded",
    },
    "expected_counts": {
        "providers": 41,
        "base_routes": 43,
        "schema_generations": 42,
        "capability_lanes": 46,
        "status_rows": {"supported": 3, "not_qualified": 42, "excluded": 1},
    },
}


class AuthorityError(ValueError):
    pass


def _fail(message: str) -> None:
    raise AuthorityError(message)


def _object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        _fail(f"{label} must be an object")
    return value


def _items(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        _fail(f"{label} must be an array")
    return value


def _authority_text(
    relative: str,
    repo_root: Path,
    overrides: Mapping[str, str],
) -> str | None:
    if relative in overrides:
        return overrides[relative]
    path = repo_root / relative
    if not path.exists():
        return None
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        _fail(f"cannot read conformance authority {relative}: {exc}")


def _manifest_version(lane: dict[str, Any]) -> tuple[str, str | int] | None:
    partition = lane.get("producer_partition")
    if not isinstance(partition, dict) or partition.get("kind") != "discrete":
        return None
    versions = partition.get("versions")
    if not isinstance(versions, list) or len(versions) != 1:
        return None
    version = versions[0]
    if not isinstance(version, dict):
        return None
    if set(version) == {"kind", "generation"}:
        return version.get("kind"), version.get("generation")
    if set(version) == {"kind", "version"}:
        return version.get("kind"), version.get("version")
    return None


def validate_authority(
    capability: dict[str, Any],
    repo_root: Path,
    authority_overrides: Mapping[str, str] | None = None,
) -> bool:
    if capability.get("conformance_authority") != CONFORMANCE_AUTHORITY:
        _fail("conformance_authority does not name the real manifest and suite registry")
    overrides = dict(authority_overrides or {})
    unexpected = set(overrides) - {CONFORMANCE_MANIFEST, CONFORMANCE_SUITES}
    if unexpected:
        _fail(f"unexpected conformance authority override: {sorted(unexpected)}")
    manifest_text = _authority_text(CONFORMANCE_MANIFEST, repo_root, overrides)
    suites_text = _authority_text(CONFORMANCE_SUITES, repo_root, overrides)
    if manifest_text is None and suites_text is None:
        return False
    if manifest_text is None or suites_text is None:
        _fail("conformance manifest and suite registry must be available together")
    try:
        manifest = _object(json.loads(manifest_text), "conformance manifest")
    except json.JSONDecodeError as exc:
        _fail(f"cannot parse conformance manifest: {exc}")

    authority_counts = CONFORMANCE_AUTHORITY["expected_counts"]
    declared_counts = {
        "providers": manifest.get("expected_provider_count"),
        "base_routes": manifest.get("expected_base_route_count"),
        "schema_generations": manifest.get("expected_schema_generation_count"),
        "capability_lanes": manifest.get("expected_capability_lane_count"),
        "status_rows": manifest.get("expected_status_rows"),
    }
    if (
        manifest.get("schema_version") != CONFORMANCE_AUTHORITY["manifest_schema_version"]
        or manifest.get("capability_revision") != CONFORMANCE_AUTHORITY["capability_revision"]
        or declared_counts != authority_counts
    ):
        _fail("conformance manifest revision or declared arithmetic is stale")
    lanes = [
        _object(value, f"conformance capability_lanes[{index}]")
        for index, value in enumerate(
            _items(manifest.get("capability_lanes"), "conformance capability_lanes")
        )
    ]
    statuses = Counter(
        _object(lane.get("status"), "conformance lane status").get("kind")
        for lane in lanes
    )
    if (
        len(lanes) != authority_counts["capability_lanes"]
        or dict(statuses) != authority_counts["status_rows"]
    ):
        _fail("conformance manifest lane arithmetic does not match its declaration")

    status_mapping = CONFORMANCE_AUTHORITY["status_mapping"]
    capability_projection = Counter(
        json.dumps(
            {
                "provider": row.get("provider_id"),
                "route": row.get("route"),
                "source_format": row.get("source_format"),
                "source_schema": row.get("source_schema"),
                "producer_bound": row.get("producer_bound"),
                "status": status_mapping.get(row.get("status")),
            },
            sort_keys=True,
            separators=(",", ":"),
        )
        for row in _items(capability.get("routes"), "capability routes")
        if isinstance(row, dict)
    )
    manifest_projection = Counter(
        json.dumps(
            {
                "provider": lane.get("provider"),
                "route": lane.get("route"),
                "source_format": lane.get("source_format"),
                "source_schema": lane.get("source_schema"),
                "producer_bound": lane.get("producer_bound"),
                "status": _object(
                    lane.get("status"), "conformance lane status"
                ).get("kind"),
            },
            sort_keys=True,
            separators=(",", ":"),
        )
        for lane in lanes
    )
    if capability_projection != manifest_projection:
        _fail("conformance manifest tuple projection differs from the capability matrix")

    codex = [lane for lane in lanes if lane.get("provider") == "codex"]
    session = [lane for lane in codex if lane.get("source_format") == CODEX_SESSION_FORMAT]
    history = [lane for lane in codex if lane.get("source_format") == CODEX_HISTORY_FORMAT]
    supported = [lane for lane in session if lane.get("status", {}).get("kind") == "supported"]
    not_qualified = [
        lane for lane in session if lane.get("status", {}).get("kind") == "not_qualified"
    ]
    semvers = {
        value
        for lane in not_qualified
        if (kind_value := _manifest_version(lane)) is not None
        for kind, value in [kind_value]
        if kind == "semver"
    }
    if (
        len(session) != 4
        or len(supported) != 1
        or _manifest_version(supported[0]) != ("unversioned_generation", 1)
        or semvers != set(CODEX_NOT_QUALIFIED_VERSIONS)
        or len(history) != 1
        or history[0].get("status", {}).get("kind") != "not_qualified"
        or _manifest_version(history[0]) != ("unversioned_generation", 1)
    ):
        _fail("conformance manifest does not freeze the exact Codex generation partition")

    for index, raw_check in enumerate(capability.get("exact_checks", [])):
        check = _object(raw_check, f"exact_checks[{index}]")
        matching = [
            lane
            for lane in lanes
            if lane.get("provider") == check.get("provider_id")
            and lane.get("route") == check.get("route")
            and lane.get("source_format") == check.get("source_format")
            and lane.get("source_schema") == check.get("source_schema")
            and lane.get("producer_bound") == check.get("producer_bound")
            and lane.get("status", {}).get("kind") == "supported"
        ]
        if len(matching) != 1:
            _fail(f"exact_checks[{index}] does not resolve to one supported manifest lane")
        suite = check.get("conformance_suite")
        evidence = {
            (item.get("suite"), item.get("test"))
            for item in matching[0].get("evidence", [])
            if isinstance(item, dict)
        }
        for raw_test in check.get("tests", []):
            test = _object(raw_test, f"exact_checks[{index}] test")
            if (suite, test.get("id")) not in evidence:
                _fail(f"exact_checks[{index}] test is absent from the conformance manifest")
            if f'"{suite}"' not in suites_text or f'"{test.get("id")}"' not in suites_text:
                _fail(f"exact_checks[{index}] test is absent from the suite registry")
    digests = {
        "manifest_sha256": sha256(manifest_text.encode("utf-8")).hexdigest(),
        "suite_registry_sha256": sha256(suites_text.encode("utf-8")).hexdigest(),
    }
    for field, actual in digests.items():
        if actual != CONFORMANCE_AUTHORITY[field]:
            _fail(f"conformance authority content hash mismatch: {field}")
    return True
