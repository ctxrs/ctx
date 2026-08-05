#!/usr/bin/env python3
"""Require every Bazel test to be release-routed or tagged manual, exactly."""

from __future__ import annotations

import argparse
from pathlib import Path


class InventoryError(ValueError):
    pass


def _labels(path: Path) -> set[str]:
    labels = {line.strip() for line in path.read_text(encoding="utf-8").splitlines()}
    labels.discard("")
    invalid = sorted(label for label in labels if not label.startswith("//"))
    if invalid:
        raise InventoryError(f"invalid Bazel labels in {path}: {invalid}")
    return labels


def validate_inventory(
    all_tests: set[str],
    release_tests: set[str],
    manual_tests: set[str],
) -> tuple[int, int]:
    unknown_release = release_tests - all_tests
    if unknown_release:
        raise InventoryError(
            f"release aggregate expanded to non-test labels: {sorted(unknown_release)}"
        )
    actual_exclusions = all_tests - release_tests
    untagged = actual_exclusions - manual_tests
    if untagged:
        raise InventoryError(
            "maintained tests are neither release-routed nor tagged manual: "
            f"{sorted(untagged)}"
        )
    routed_manual = manual_tests - actual_exclusions
    if routed_manual:
        raise InventoryError(
            "manual tests must be excluded from the release aggregate: "
            f"{sorted(routed_manual)}"
        )
    return len(release_tests), len(actual_exclusions)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--all-tests", required=True, type=Path)
    parser.add_argument("--release-tests", required=True, type=Path)
    parser.add_argument("--manual-tests", required=True, type=Path)
    args = parser.parse_args()
    try:
        routed, excluded = validate_inventory(
            _labels(args.all_tests),
            _labels(args.release_tests),
            _labels(args.manual_tests),
        )
    except InventoryError as error:
        parser.error(str(error))
    print(
        "public Bazel test tier inventory: OK "
        f"routed={routed} manual={excluded}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
