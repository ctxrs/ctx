"""Deterministic graph reporting and parity violations for the Rust crate gate."""

from __future__ import annotations

import hashlib
import json
from typing import Any


class GateError(RuntimeError):
    pass


def canonical_bytes(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def hash_value(value: Any) -> str:
    return hashlib.sha256(canonical_bytes(value)).hexdigest()


def empty_report(detail: str) -> dict[str, Any]:
    return {
        "schema_version": 2,
        "status": "fail",
        "metric": None,
        "platforms": [],
        "targets": [],
        "source_digest": None,
        "cloc": {"hard_limit": None, "packages": []},
        "graph": {"cargo_edges": [], "bazel_edges": [], "cycles": []},
        "violations": [{"code": "gate_error", "detail": detail}],
    }


def edge_records(edges: dict[str, set[tuple[str, str]]]) -> list[dict[str, Any]]:
    grouped: dict[tuple[str, str], list[str]] = {}
    for platform, values in edges.items():
        for edge in values:
            grouped.setdefault(edge, []).append(platform)
    return [
        {"from": edge[0], "to": edge[1], "platforms": sorted(platforms)}
        for edge, platforms in sorted(grouped.items())
    ]


def canonical_cycle(nodes: list[str]) -> tuple[str, ...]:
    body = nodes[:-1]
    if len(body) <= 1:
        return tuple(nodes)
    rotations = [tuple(body[index:] + body[:index] + [body[index]]) for index in range(len(body))]
    return min(rotations)


def graph_cycles(edges: dict[str, set[tuple[str, str]]]) -> list[dict[str, Any]]:
    grouped: dict[tuple[str, ...], list[str]] = {}
    for platform, platform_edges in sorted(edges.items()):
        graph: dict[str, set[str]] = {}
        for source, target in platform_edges:
            graph.setdefault(source, set()).add(target)
            graph.setdefault(target, set())
        found: set[tuple[str, ...]] = set()
        stack: list[str] = []
        active: set[str] = set()

        def visit(node: str) -> None:
            stack.append(node)
            active.add(node)
            for target in sorted(graph.get(node, set())):
                if target in active:
                    start = stack.index(target)
                    found.add(canonical_cycle(stack[start:] + [target]))
                elif target not in completed:
                    visit(target)
            active.remove(node)
            stack.pop()
            completed.add(node)

        completed: set[str] = set()
        for node in sorted(graph):
            if node not in completed:
                visit(node)
        for cycle in sorted(found):
            grouped.setdefault(cycle, []).append(platform)
    return [
        {"cycle": list(cycle), "platforms": sorted(platforms)}
        for cycle, platforms in sorted(grouped.items())
    ]


def violation(code: str, detail: str, **fields: Any) -> dict[str, Any]:
    value = {"code": code, "detail": detail}
    value.update(fields)
    return value


def graph_edge_violations(
    cargo_edges: dict[str, set[tuple[str, str]]],
    expected_edges: dict[str, set[tuple[str, str]]],
    bazel_edges: dict[str, set[tuple[str, str]]] | None = None,
) -> list[dict[str, Any]]:
    violations: list[dict[str, Any]] = []
    for platform in sorted(cargo_edges):
        for edge in sorted(cargo_edges[platform] - expected_edges[platform]):
            violations.append(violation("forbidden_edge", f"Cargo has an unapproved workspace edge on {platform}: {edge[0]} -> {edge[1]}", platform=platform, source=edge[0], target=edge[1]))
        for edge in sorted(expected_edges[platform] - cargo_edges[platform]):
            violations.append(violation("stale_edge", f"expected workspace edge is stale on {platform}: {edge[0]} -> {edge[1]}", platform=platform, source=edge[0], target=edge[1]))
        if bazel_edges is not None:
            for edge in sorted(bazel_edges[platform] - cargo_edges[platform]):
                violations.append(violation("extra_bazel_edge", f"Bazel has a direct workspace edge absent from Cargo on {platform}: {edge[0]} -> {edge[1]}", platform=platform, source=edge[0], target=edge[1]))
            for edge in sorted(cargo_edges[platform] - bazel_edges[platform]):
                violations.append(violation("missing_bazel_edge", f"Bazel lacks a direct Cargo workspace edge on {platform}: {edge[0]} -> {edge[1]}", platform=platform, source=edge[0], target=edge[1]))
    return violations


def source_action_violations(
    cargo_sources: set[str],
    bazel_sources: set[str],
    *,
    platform: str,
    label: str,
) -> list[dict[str, Any]]:
    missing = sorted(cargo_sources - bazel_sources)
    if not missing:
        return []
    return [
        violation(
            "missing_bazel_sources",
            f"configured Bazel action omits Cargo-reachable sources for {label} on {platform}",
            platform=platform,
            label=label,
            sources=missing,
        )
    ]


def policy_edges(policy: dict[str, Any], platforms: list[Any]) -> dict[str, set[tuple[str, str]]]:
    values = policy.get("expected_edges")
    if not isinstance(values, list):
        raise GateError("expected_edges must be an array")
    platform_ids = {platform.id for platform in platforms}
    result = {platform.id: set() for platform in platforms}
    canonical: list[tuple[str, str, tuple[str, ...]]] = []
    for value in values:
        if not isinstance(value, dict) or set(value) != {"from", "to", "platforms"}:
            raise GateError("expected edge record is malformed")
        edge_platforms = value["platforms"]
        if (
            not isinstance(edge_platforms, list)
            or edge_platforms != sorted(edge_platforms)
            or len(edge_platforms) != len(set(edge_platforms))
            or not edge_platforms
            or not set(edge_platforms) <= platform_ids
        ):
            raise GateError("expected edge platforms are invalid")
        if not isinstance(value["from"], str) or not isinstance(value["to"], str):
            raise GateError("expected edge endpoints must be strings")
        for platform_id in edge_platforms:
            edge = (value["from"], value["to"])
            if edge in result[platform_id]:
                raise GateError(f"duplicate expected edge: {platform_id} {edge}")
            result[platform_id].add(edge)
        canonical.append((value["from"], value["to"], tuple(edge_platforms)))
    if canonical != sorted(canonical):
        raise GateError("expected_edges must be sorted")
    return result


def validate_policy_packages(
    policy: dict[str, Any],
    package_results: list[dict[str, Any]],
    target_keys: dict[str, list[str]],
) -> list[dict[str, Any]]:
    values = policy.get("packages")
    if not isinstance(values, list):
        raise GateError("policy packages must be an array")
    records: dict[str, dict[str, Any]] = {}
    order: list[str] = []
    for value in values:
        if not isinstance(value, dict) or set(value) != {"package", "manifest", "production_targets", "source_digest"}:
            raise GateError("policy package record is malformed")
        package = value["package"]
        if not isinstance(package, str) or package in records:
            raise GateError("policy package names must be unique strings")
        manifest = value["manifest"]
        if not isinstance(manifest, str) or not manifest or manifest.startswith("/") or ".." in manifest.split("/"):
            raise GateError(f"policy manifest is invalid: {package}")
        targets = value["production_targets"]
        if not isinstance(targets, list) or targets != sorted(targets) or len(targets) != len(set(targets)):
            raise GateError(f"policy production_targets must be sorted and unique: {package}")
        digest = value["source_digest"]
        if not isinstance(digest, str) or len(digest) != 64 or any(character not in "0123456789abcdef" for character in digest):
            raise GateError(f"policy source_digest is invalid: {package}")
        records[package] = value
        order.append(package)
    if order != sorted(order):
        raise GateError("policy packages must be sorted")
    actual = {item["package"]: item for item in package_results}
    violations: list[dict[str, Any]] = []
    for package in sorted(set(records) | set(actual)):
        expected = records.get(package)
        observed = actual.get(package)
        if expected is None:
            violations.append(violation("missing_package_record", f"new workspace package lacks an exact policy record: {package}", package=package))
            continue
        if observed is None:
            violations.append(violation("stale_package_record", f"policy package no longer exists: {package}", package=package))
            continue
        if expected["manifest"] != observed["manifest"]:
            violations.append(violation("manifest_drift", f"{package} manifest changed", package=package, expected=expected["manifest"], actual=observed["manifest"]))
        if expected["production_targets"] != target_keys[package]:
            violations.append(violation("target_drift", f"{package} production target set changed", package=package, expected=expected["production_targets"], actual=target_keys[package]))
        if expected["source_digest"] != observed["source_digest"]:
            violations.append(violation("source_drift", f"{package} reachable checked-in Rust source digest changed", package=package, expected=expected["source_digest"], actual=observed["source_digest"]))
    return violations
