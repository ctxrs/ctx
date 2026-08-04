#!/usr/bin/env python3
"""Fail when a maintained Bazel test is neither routed nor explicitly manual."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


ALLOWED_CLASSIFICATIONS = {
    "external-harness",
    "platform-manual",
    "test-fixture",
}


class InventoryError(ValueError):
    pass


def _labels(path: Path) -> set[str]:
    labels = {line.strip() for line in path.read_text(encoding="utf-8").splitlines()}
    labels.discard("")
    invalid = sorted(label for label in labels if not label.startswith("//"))
    if invalid:
        raise InventoryError(f"invalid Bazel labels in {path}: {invalid}")
    return labels


def _inventory(path: Path) -> dict[str, Any]:
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise InventoryError(f"could not parse test tier inventory: {error}") from error
    if not isinstance(document, dict):
        raise InventoryError("test tier inventory root must be an object")
    return document


def validate_inventory(
    all_tests: set[str],
    release_tests: set[str],
    manual_tests: set[str],
    inventory: dict[str, Any],
) -> tuple[int, int]:
    if inventory.get("schema_version") != 1 or inventory.get("aggregate") != "//:release":
        raise InventoryError("test tier inventory schema or aggregate is invalid")
    entries = inventory.get("intentional_exclusions")
    if not isinstance(entries, list):
        raise InventoryError("intentional_exclusions must be a list")

    labels: list[str] = []
    for entry in entries:
        if not isinstance(entry, dict):
            raise InventoryError("intentional exclusion entries must be objects")
        label = entry.get("label")
        classification = entry.get("classification")
        reason = entry.get("reason")
        if not isinstance(label, str) or not label.startswith("//"):
            raise InventoryError("intentional exclusion has an invalid label")
        if classification not in ALLOWED_CLASSIFICATIONS:
            raise InventoryError(f"{label} has an invalid exclusion classification")
        if not isinstance(reason, str) or not reason.strip():
            raise InventoryError(f"{label} lacks an intentional exclusion reason")
        labels.append(label)
    if labels != sorted(labels) or len(labels) != len(set(labels)):
        raise InventoryError("intentional exclusions must be unique and label-sorted")

    unknown_release = release_tests - all_tests
    if unknown_release:
        raise InventoryError(
            f"release aggregate expanded to non-test labels: {sorted(unknown_release)}"
        )
    actual_exclusions = all_tests - release_tests
    reviewed_exclusions = set(labels)
    unclassified = actual_exclusions - reviewed_exclusions
    stale = reviewed_exclusions - actual_exclusions
    if unclassified:
        raise InventoryError(
            "maintained tests are neither routed nor intentionally excluded: "
            f"{sorted(unclassified)}"
        )
    if stale:
        raise InventoryError(
            f"intentional test exclusions are stale or routed: {sorted(stale)}"
        )
    not_manual = reviewed_exclusions - manual_tests
    if not_manual:
        raise InventoryError(
            "intentional exclusions must carry the Bazel manual tag: "
            f"{sorted(not_manual)}"
        )
    return len(release_tests), len(reviewed_exclusions)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--all-tests", required=True, type=Path)
    parser.add_argument("--release-tests", required=True, type=Path)
    parser.add_argument("--manual-tests", required=True, type=Path)
    parser.add_argument("--inventory", required=True, type=Path)
    args = parser.parse_args()
    try:
        routed, excluded = validate_inventory(
            _labels(args.all_tests),
            _labels(args.release_tests),
            _labels(args.manual_tests),
            _inventory(args.inventory),
        )
    except InventoryError as error:
        parser.error(str(error))
    print(
        "public Bazel test tier inventory: OK "
        f"routed={routed} intentional_manual={excluded}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
