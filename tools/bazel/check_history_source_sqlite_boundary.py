#!/usr/bin/env python3
"""Exact dependency and ownership policy for SQLite source acquisition."""

from __future__ import annotations

import argparse
import sys
import tomllib
from pathlib import Path
from typing import Any, Iterable


EXPECTED_DEPENDENCIES: dict[str, Any] = {
    "ctx-history-core": {"path": "../ctx-history-core"},
    "ctx-history-platform": {"path": "../ctx-history-platform"},
    "ctx-history-source-io": {"path": "../ctx-history-source-io"},
    "fs2": "0.4.3",
    "libc": {"workspace": True},
    "rusqlite": {"workspace": True},
    "serde_json": {"workspace": True},
    "sha2": {"workspace": True},
    "tempfile": {"workspace": True},
    "thiserror": {"workspace": True},
    "url": {"workspace": True},
}
EXPECTED_WINDOWS_DEPENDENCIES: dict[str, Any] = {
    "windows-sys": {
        "version": "0.61",
        "features": [
            "Win32_Security",
            "Win32_Storage_FileSystem",
            "Win32_System_Memory",
        ],
    }
}
EXPECTED_INTERNAL_BAZEL = {
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-platform:lib",
    "//crates/ctx-history-source-io:lib",
    "//crates/ctx-history-source-sqlite:lib",
}
FORBIDDEN_SOURCE_IO_DEPENDENCIES = {"fs2", "rusqlite"}
RETIRED_SOURCE_IO_FILES = {
    "progress.rs",
    "sqlite.rs",
    "sqlite_source.rs",
    "sqlite_source",
}


class BoundaryError(RuntimeError):
    pass


def _describe_drift(expected: set[str], actual: set[str]) -> str:
    return f"missing={sorted(expected - actual)} extra={sorted(actual - expected)}"


def load_manifest(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def validate_sqlite_manifest(path: Path) -> None:
    manifest = load_manifest(path)
    dependencies = manifest.get("dependencies", {})
    if dependencies != EXPECTED_DEPENDENCIES:
        raise BoundaryError(
            "ctx-history-source-sqlite dependency inventory drifted: "
            + _describe_drift(set(EXPECTED_DEPENDENCIES), set(dependencies))
        )
    targets = manifest.get("target", {})
    expected_targets = {
        "cfg(windows)": {"dependencies": EXPECTED_WINDOWS_DEPENDENCIES}
    }
    if targets != expected_targets:
        raise BoundaryError("ctx-history-source-sqlite target dependencies drifted")
    internal = {name for name in dependencies if name.startswith("ctx-")}
    if internal != {
        "ctx-history-core",
        "ctx-history-platform",
        "ctx-history-source-io",
    }:
        raise BoundaryError("ctx-history-source-sqlite gained an upward internal dependency")
    bypasses = sorted(
        name
        for name in manifest
        if name.endswith("dependencies") and name != "dependencies"
    )
    if bypasses or "dev-dependencies" in manifest:
        raise BoundaryError("ctx-history-source-sqlite dependency-table bypass")


def validate_source_io_manifest(path: Path) -> None:
    dependencies = load_manifest(path).get("dependencies", {})
    forbidden = FORBIDDEN_SOURCE_IO_DEPENDENCIES & set(dependencies)
    if forbidden:
        raise BoundaryError(
            "ctx-history-source-io retains SQLite physical dependencies: "
            + ", ".join(sorted(forbidden))
        )


def validate_source_io_tree(path: Path) -> None:
    retained = sorted(
        name
        for name in RETIRED_SOURCE_IO_FILES
        if (path / name).is_file()
        or (
            (path / name).is_dir()
            and any(child.is_file() for child in (path / name).rglob("*"))
        )
    )
    if retained:
        raise BoundaryError(
            "ctx-history-source-io retains SQLite production bodies: "
            + ", ".join(retained)
        )


def validate_bazel_inventory(labels: Iterable[str], scope: str) -> None:
    actual = {label.strip() for label in labels if label.strip()}
    if actual != EXPECTED_INTERNAL_BAZEL:
        raise BoundaryError(
            f"ctx-history-source-sqlite Bazel {scope} internal allowlist drifted: "
            + _describe_drift(EXPECTED_INTERNAL_BAZEL, actual)
        )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("sqlite_manifest", type=Path)
    parser.add_argument("source_io_manifest", type=Path)
    parser.add_argument("source_io_tree", type=Path)
    parser.add_argument("direct_labels", type=Path)
    parser.add_argument("closure_labels", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        validate_sqlite_manifest(args.sqlite_manifest)
        validate_source_io_manifest(args.source_io_manifest)
        validate_source_io_tree(args.source_io_tree)
        validate_bazel_inventory(args.direct_labels.read_text().splitlines(), "direct")
        validate_bazel_inventory(args.closure_labels.read_text().splitlines(), "transitive")
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
