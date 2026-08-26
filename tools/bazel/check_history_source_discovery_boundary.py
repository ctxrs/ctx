#!/usr/bin/env python3
"""Exact production dependency policy for ctx-history-source-discovery."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path
from typing import Any, Iterable


EXPECTED_DEPENDENCIES: dict[str, Any] = {
    "chrono": {"workspace": True},
    "ctx-history-capture-model": {"path": "../ctx-history-capture-model"},
    "ctx-history-core": {"path": "../ctx-history-core"},
    "ctx-history-openclaw-schema": {"path": "../ctx-history-openclaw-schema"},
    "ctx-history-platform": {"path": "../ctx-history-platform"},
    "ctx-history-source-io": {"path": "../ctx-history-source-io"},
    "ctx-history-source-sqlite": {"path": "../ctx-history-source-sqlite"},
    "directories": {"workspace": True},
    "json5": {"workspace": True},
    "jsonc-parser": {"workspace": True},
    "libc": {"workspace": True},
    "quick-xml": {"workspace": True},
    "rusqlite": {"workspace": True},
    "serde_json": {"workspace": True},
    "serde_yaml": {"workspace": True},
    "sha2": {"workspace": True},
    "same-file": "1.0.6",
    "thiserror": {"workspace": True},
    "toml_edit": {"workspace": True},
}
EXPECTED_DEV_DEPENDENCIES: dict[str, Any] = {
    "ctx-history-openclaw-schema": {
        "path": "../ctx-history-openclaw-schema",
        "features": ["test-support"],
    },
    "ctx-history-source-io": {
        "path": "../ctx-history-source-io",
        "features": ["test-support"],
    },
    "ctx-history-source-sqlite": {
        "path": "../ctx-history-source-sqlite",
        "features": ["test-support"],
    },
    "tempfile": {"workspace": True},
}
EXPECTED_INTERNAL_CARGO = {
    "ctx-history-capture-model",
    "ctx-history-core",
    "ctx-history-openclaw-schema",
    "ctx-history-platform",
    "ctx-history-source-io",
    "ctx-history-source-sqlite",
}
EXPECTED_INTERNAL_BAZEL = {
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-openclaw-schema:lib",
    "//crates/ctx-history-platform:lib",
    "//crates/ctx-history-source-discovery:lib",
    "//crates/ctx-history-source-io:lib",
    "//crates/ctx-history-source-sqlite:lib",
}


class BoundaryError(RuntimeError):
    pass


DISCOVERY_ENV_ALLOWLIST = "DISCOVERY_ENV_ALLOWLIST"
RUST_STR_SLICE_DECLARATION = re.compile(
    r"(?ms)^[ \t]*(?:pub(?:\([^\n)]*\))?[ \t]+)?const[ \t]+"
    r"(?P<name>[A-Z][A-Z0-9_]*)[ \t]*:[^\n=]+=[ \t]*&\[[ \t]*\n"
    r"(?P<body>.*?)^[ \t]*\];[ \t]*(?://[^\n]*)?$"
)
RUST_STR_SLICE_ITEM = re.compile(
    r'^[ \t]*"(?P<value>[A-Za-z_][A-Za-z0-9_]*)",[ \t]*(?://[^\n]*)?$'
)


def _describe_drift(expected: set[str], actual: set[str]) -> str:
    return f"missing={sorted(expected - actual)} extra={sorted(actual - expected)}"


def parse_rust_str_slice(source: str, constant: str, authority: str) -> tuple[str, ...]:
    declarations = [
        match
        for match in RUST_STR_SLICE_DECLARATION.finditer(source)
        if match.group("name") == constant
    ]
    if len(declarations) != 1:
        raise BoundaryError(
            f"{authority} must define exactly one parseable {constant} string slice; "
            f"found={len(declarations)}"
        )

    values: list[str] = []
    for line in declarations[0].group("body").splitlines():
        if not line.strip() or line.lstrip().startswith("//"):
            continue
        item = RUST_STR_SLICE_ITEM.fullmatch(line)
        if item is None:
            raise BoundaryError(
                f"{authority} {constant} contains an unsupported item: {line.strip()!r}"
            )
        values.append(item.group("value"))
    if not values:
        raise BoundaryError(f"{authority} {constant} must not be empty")
    duplicates = sorted({value for value in values if values.count(value) > 1})
    if duplicates:
        raise BoundaryError(
            f"{authority} {constant} contains duplicate values: {duplicates}"
        )
    return tuple(values)


def validate_discovery_environment_sources(
    supervisor_source: str,
    canonical_source: str,
) -> None:
    supervisor = parse_rust_str_slice(
        supervisor_source,
        DISCOVERY_ENV_ALLOWLIST,
        "ctx-daemon-application supervisor",
    )
    canonical = parse_rust_str_slice(
        canonical_source,
        DISCOVERY_ENV_ALLOWLIST,
        "ctx-history-source-discovery canonical policy",
    )
    if set(supervisor) != set(canonical):
        raise BoundaryError(
            "supervisor discovery environment allowlist set drifted from canonical policy: "
            + _describe_drift(set(canonical), set(supervisor))
        )
    if supervisor != canonical:
        mismatch = next(
            index
            for index, (supervisor_value, canonical_value) in enumerate(
                zip(supervisor, canonical, strict=True)
            )
            if supervisor_value != canonical_value
        )
        raise BoundaryError(
            "supervisor discovery environment allowlist order drifted from canonical policy: "
            f"index={mismatch} supervisor={supervisor[mismatch]!r} "
            f"canonical={canonical[mismatch]!r}"
        )


def validate_discovery_environment_parity(supervisor: Path, canonical: Path) -> None:
    validate_discovery_environment_sources(
        supervisor.read_text(encoding="utf-8"),
        canonical.read_text(encoding="utf-8"),
    )


def validate_manifest(path: Path) -> None:
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)

    dependencies = manifest.get("dependencies", {})
    if dependencies != EXPECTED_DEPENDENCIES:
        raise BoundaryError(
            "ctx-history-source-discovery Cargo production dependency inventory drifted: "
            + _describe_drift(set(EXPECTED_DEPENDENCIES), set(dependencies))
        )
    internal = {name for name in dependencies if name.startswith("ctx-")}
    if internal != EXPECTED_INTERNAL_CARGO:
        raise BoundaryError(
            "ctx-history-source-discovery Cargo internal allowlist drifted: "
            + _describe_drift(EXPECTED_INTERNAL_CARGO, internal)
        )
    if manifest.get("dev-dependencies", {}) != EXPECTED_DEV_DEPENDENCIES:
        raise BoundaryError(
            "ctx-history-source-discovery Cargo dev dependency inventory drifted"
        )
    if "features" in manifest:
        raise BoundaryError(
            "ctx-history-source-discovery must not expose production feature switches"
        )

    dependency_bypasses = sorted(
        name
        for name in manifest
        if name.endswith("dependencies")
        and name not in {"dependencies", "dev-dependencies"}
    )
    if dependency_bypasses or "target" in manifest:
        raise BoundaryError(
            "ctx-history-source-discovery Cargo dependency-table bypass: "
            f"{dependency_bypasses or ['target']}"
        )


def validate_bazel_inventory(labels: Iterable[str], scope: str) -> None:
    actual = {label.strip() for label in labels if label.strip()}
    if actual != EXPECTED_INTERNAL_BAZEL:
        raise BoundaryError(
            f"ctx-history-source-discovery Bazel {scope} internal allowlist drifted: "
            + _describe_drift(EXPECTED_INTERNAL_BAZEL, actual)
        )


def validate(
    manifest: Path,
    direct_labels: Path,
    closure_labels: Path,
    supervisor_discovery_environment: Path,
    canonical_discovery_environment: Path,
) -> None:
    validate_manifest(manifest)
    validate_bazel_inventory(direct_labels.read_text().splitlines(), "direct")
    validate_bazel_inventory(closure_labels.read_text().splitlines(), "transitive")
    validate_discovery_environment_parity(
        supervisor_discovery_environment,
        canonical_discovery_environment,
    )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("direct_labels", type=Path)
    parser.add_argument("closure_labels", type=Path)
    parser.add_argument("supervisor_discovery_environment", type=Path)
    parser.add_argument("canonical_discovery_environment", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        validate(
            args.manifest,
            args.direct_labels,
            args.closure_labels,
            args.supervisor_discovery_environment,
            args.canonical_discovery_environment,
        )
    except BoundaryError as error:
        print(error, file=sys.stderr)
        return 1
    print(
        "ctx-history-source-discovery exact Cargo/Bazel dependency and "
        "daemon supervisor environment parity boundary ok"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
