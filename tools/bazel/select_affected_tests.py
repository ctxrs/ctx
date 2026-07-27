#!/usr/bin/env python3
"""Fail-closed filtering for bazel-diff's complete-content result."""

from __future__ import annotations

import pathlib
import sys

FULL_SUITE = "//:presubmit"
GLOBAL_FILES = {
    ".bazelrc",
    ".bazelversion",
    "Cargo.lock",
    "Cargo.toml",
    "MODULE.bazel",
    "MODULE.bazel.lock",
}


def is_graph_global(path: str) -> bool:
    item = pathlib.PurePosixPath(path)
    return (
        path in GLOBAL_FILES
        or item.name in {"BUILD", "BUILD.bazel"}
        or item.suffix == ".bzl"
        or path.startswith("tools/bazel/")
    )


def is_runnable_test(label: str) -> bool:
    name = label.rsplit(":", 1)[-1]
    return label.startswith("//") and (
        name.endswith(("_test", "_tests", "_check", "_e2e", "_smoke"))
        or name in {"fast", "presubmit", "ci", "smoke", "native_rust", "native_rust_smoke"}
        or "audit" in name
    )


def select(changed: list[str], impacted: list[str], diff_succeeded: bool = True) -> list[str]:
    if not diff_succeeded or any(is_graph_global(path) for path in changed):
        return [FULL_SUITE]
    tests = sorted({label.strip() for label in impacted if is_runnable_test(label.strip())})
    if changed and not tests:
        return [FULL_SUITE]
    return tests


def lines(path: str) -> list[str]:
    return [line.strip() for line in pathlib.Path(path).read_text().splitlines() if line.strip()]


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit("usage: select_affected_tests.py CHANGED IMPACTED OUTPUT")
    selected = select(lines(sys.argv[1]), lines(sys.argv[2]))
    pathlib.Path(sys.argv[3]).write_text("".join(f"{label}\n" for label in selected))


if __name__ == "__main__":
    main()
