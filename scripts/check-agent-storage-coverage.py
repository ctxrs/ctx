#!/usr/bin/env python3
"""Validate the npx skills to ctx storage coverage ledger."""

from __future__ import annotations

import json
import re
import sys
from collections import Counter
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
LEDGER_PATH = REPO_ROOT / "docs" / "agent-storage-coverage.md"
FIXTURE_PATH = REPO_ROOT / "tests" / "fixtures" / "npx-skills-agent-ids-1.5.14.txt"
PROVIDER_MATRIX_PATH = REPO_ROOT / "docs" / "provider-support-matrix.json"

UPSTREAM_PACKAGE = "skills@1.5.14"
UPSTREAM_COMMIT = "2adcfe5a4cce0ce5f4d5547a997b2a161ec5d127"
EXPECTED_COUNTS = {
    "native-auto": 63,
    "native-explicit": 2,
    "native-preview": 0,
    "candidate-family": 0,
    "webapp-boundary": 3,
    "unknown": 2,
    "install-target": 2,
}
ALLOWED_STATUSES = set(EXPECTED_COUNTS)
REQUIRED_SCHEMA_FAMILIES = {
    "opencode sqlite family",
    "Cline/Roo/Bob task JSON",
    "JSONL CLI event logs",
    "project task JSON",
    "filesystem event JSON",
    "generic sqlite messages",
    "OpenLoaf chat JSONL",
    "Forge conversation SQLite",
    "Junie event-sourced UI stream",
    "LangGraph checkpoint SQLite",
    "LiveStore SQLite state DB",
    "Warp restoration SQLite",
    "Workflow local-world streams",
    "per-agent history JSON",
    "explicit ATIF export JSON",
    "VS Code/Electron storage",
    "webapp/object-store boundary",
}
ALLOWED_SCHEMA_FAMILIES = REQUIRED_SCHEMA_FAMILIES | {
    "CLI session JSON",
    "unknown native history",
    "agent skills aggregate",
}
PRIVATE_TEXT_MARKERS = ("/home/", "ctx-private", "ctx-multi-repo-workspace")
TABLE_HEADER = (
    "| npx skills agent id | ctx storage ingestion status | schema family | "
    "evidence source | blocked reason / gap |"
)
ROW_RE = re.compile(
    r"^\| `(?P<agent_id>[^`]+)` \| `(?P<status>[^`]+)` \| "
    r"`(?P<schema_family>[^`]+)` \| (?P<evidence>.+) \| (?P<gap>.+) \|$"
)


class CoverageError(Exception):
    pass


def fail(message: str) -> None:
    raise CoverageError(message)


def load_fixture_ids() -> list[str]:
    ids: list[str] = []
    for line in FIXTURE_PATH.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        ids.append(line)
    if len(ids) != 72:
        fail(f"expected 72 fixture ids, found {len(ids)}")
    if len(ids) != len(set(ids)):
        fail("fixture contains duplicate ids")
    return ids


def load_ctx_source_formats() -> set[str]:
    matrix = json.loads(PROVIDER_MATRIX_PATH.read_text(encoding="utf-8"))
    formats: set[str] = set()
    for provider in matrix.get("providers", []):
        for implemented_path in provider.get("implemented_paths", []):
            source_format = implemented_path.get("source_format")
            if isinstance(source_format, str):
                formats.add(source_format)
    if not formats:
        fail("provider support matrix has no source formats")
    return formats


def parse_ledger_rows() -> list[dict[str, str]]:
    text = LEDGER_PATH.read_text(encoding="utf-8")
    for required in (UPSTREAM_PACKAGE, UPSTREAM_COMMIT, "src/types.ts", "src/agents.ts"):
        if required not in text:
            fail(f"ledger is missing upstream evidence marker: {required}")
    if any(marker in text for marker in PRIVATE_TEXT_MARKERS):
        fail("ledger contains private host or workspace path text")

    lines = text.splitlines()
    try:
        start = lines.index(TABLE_HEADER)
    except ValueError:
        fail("ledger table header changed or is missing")

    separator_index = start + 1
    if separator_index >= len(lines) or not lines[separator_index].startswith("| --- |"):
        fail("ledger table separator is missing")

    rows: list[dict[str, str]] = []
    for line in lines[separator_index + 1 :]:
        if not line.startswith("|"):
            break
        match = ROW_RE.match(line)
        if not match:
            fail(f"malformed ledger row: {line}")
        rows.append(match.groupdict())

    if not rows:
        fail("ledger table has no rows")
    return rows


def validate_rows(rows: list[dict[str, str]], expected_ids: list[str], ctx_formats: set[str]) -> None:
    actual_ids = [row["agent_id"] for row in rows]
    if actual_ids != expected_ids:
        missing = sorted(set(expected_ids).difference(actual_ids))
        extra = sorted(set(actual_ids).difference(expected_ids))
        first_diff = next(
            (
                (index, expected, actual)
                for index, (expected, actual) in enumerate(zip(expected_ids, actual_ids), start=1)
                if expected != actual
            ),
            None,
        )
        fail(
            "ledger ids do not match the pinned upstream order"
            f"; missing={missing}; extra={extra}; first_diff={first_diff}"
        )

    if len(actual_ids) != len(set(actual_ids)):
        fail("ledger contains duplicate agent ids")

    counts = Counter(row["status"] for row in rows)
    actual_counts = {status: counts.get(status, 0) for status in EXPECTED_COUNTS}
    unexpected_statuses = sorted(set(counts).difference(EXPECTED_COUNTS))
    if actual_counts != EXPECTED_COUNTS or unexpected_statuses:
        fail(
            f"status counts changed: expected {EXPECTED_COUNTS}, "
            f"found {actual_counts}, unexpected={unexpected_statuses}"
        )

    schema_families = {row["schema_family"] for row in rows}
    unknown_families = schema_families.difference(ALLOWED_SCHEMA_FAMILIES)
    if unknown_families:
        fail(f"unknown schema families: {sorted(unknown_families)}")
    missing_required_families = REQUIRED_SCHEMA_FAMILIES.difference(schema_families)
    if missing_required_families:
        fail(f"required schema families missing: {sorted(missing_required_families)}")

    for row in rows:
        agent_id = row["agent_id"]
        status = row["status"]
        family = row["schema_family"]
        evidence = row["evidence"]
        gap = row["gap"]

        if status not in ALLOWED_STATUSES:
            fail(f"{agent_id}: unsupported status {status}")
        if family not in ALLOWED_SCHEMA_FAMILIES:
            fail(f"{agent_id}: unsupported schema family {family}")
        if "npx" not in evidence:
            fail(f"{agent_id}: evidence must cite npx upstream config")
        if status in {"native-auto", "native-explicit", "native-preview"}:
            cited_formats = set(re.findall(r"`([^`]+)`", evidence))
            if not cited_formats.intersection(ctx_formats):
                fail(f"{agent_id}: native row must cite a ctx source format")
            if "ctx" not in evidence:
                fail(f"{agent_id}: native row must cite ctx evidence")
        elif "no ctx provider" not in evidence:
            fail(f"{agent_id}: non-native row must state that no ctx provider exists")
        if status not in {"native-auto"} and gap == "-":
            fail(f"{agent_id}: non-auto row needs a blocked reason or gap")
        if status == "native-explicit" and "explicit" not in f"{evidence} {gap}".lower():
            fail(f"{agent_id}: explicit row must explain the explicit import/export contract")
        if status == "native-preview" and "Preview" not in gap:
            fail(f"{agent_id}: preview row must explain preview status")


def main() -> int:
    try:
        expected_ids = load_fixture_ids()
        ctx_formats = load_ctx_source_formats()
        rows = parse_ledger_rows()
        validate_rows(rows, expected_ids, ctx_formats)
    except (OSError, json.JSONDecodeError, CoverageError) as exc:
        print(f"agent storage coverage check failed: {exc}", file=sys.stderr)
        return 1

    print(
        "agent storage coverage ok: "
        f"{len(rows)} npx ids, "
        + ", ".join(f"{status}={EXPECTED_COUNTS[status]}" for status in EXPECTED_COUNTS)
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
