#!/usr/bin/env python3
"""Validate the public provider support matrix.

This is a public truthfulness gate. It checks that documented provider support
has public docs, public tests, and any claimed fixture paths in the repository.
It intentionally does not require live provider runs, private fixture
provenance, release evidence, or network access.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = REPO_ROOT / "docs/provider-support-matrix.json"
ALLOWED_STATUSES = {
    "local_import",
    "local_import_when_supported",
    "fixture_only",
    "detected_unsupported",
    "blocked",
}
ALLOWED_PATH_KINDS = {
    "native_import",
    "fixture_import",
    "detected_unsupported",
    "blocked",
}
ALLOWED_FIDELITY = {
    "imported",
    "partial",
    "fixture_only",
    "detected_unsupported",
    "blocked",
}
REQUIRED_FIDELITY_FIELDS = {
    "user_prompts",
    "assistant_messages",
    "tool_calls",
    "tool_output",
    "command_output",
    "files_touched",
    "artifacts",
    "model_identity",
    "costs",
    "token_usage",
    "parent_child_session_edges",
}
PROVIDER_ID_RE = re.compile(r"^[a-z0-9][a-z0-9_]*$")
PRIVATE_TEXT_MARKERS = ("/home/", "ctx-" + "private", "ctx-multi" + "-repo-workspace")
SUPPORT_DOC_PATH = REPO_ROOT / "docs/provider-support.md"
PUBLIC_CLI_TEST_PATHS = {
    "crates/ctx-cli/tests/native_providers.rs",
    "crates/ctx-cli/tests/search_refresh.rs",
    "crates/ctx-cli/tests/search_show_locate_sql.rs",
    "crates/ctx-cli/tests/setup_sources_import.rs",
}


class MatrixError(Exception):
    pass


def fail(message: str) -> None:
    raise MatrixError(message)


def expect_type(value: Any, expected_type: type, field: str) -> Any:
    if not isinstance(value, expected_type):
        fail(f"{field} must be {expected_type.__name__}")
    return value


def require_non_empty_string(value: Any, field: str) -> str:
    text = expect_type(value, str, field)
    if not text.strip():
        fail(f"{field} must be non-empty")
    return text


def require_string_list(value: Any, field: str, *, allow_empty: bool = False) -> list[str]:
    items = expect_type(value, list, field)
    if not allow_empty and not items:
        fail(f"{field} must not be empty")
    for index, item in enumerate(items):
        require_non_empty_string(item, f"{field}[{index}]")
    return items


def require_repo_path(value: str, field: str) -> Path:
    if value.startswith("/") or ".." in Path(value).parts:
        fail(f"{field} must be a relative repository path")
    path = REPO_ROOT / value
    if not path.exists():
        fail(f"{field} does not exist: {value}")
    return path


def scan_private_text(value: Any, field: str) -> None:
    if isinstance(value, str):
        if any(token in value for token in PRIVATE_TEXT_MARKERS):
            fail(f"{field} contains private path wording")
        return
    if isinstance(value, list):
        for index, item in enumerate(value):
            scan_private_text(item, f"{field}[{index}]")
        return
    if isinstance(value, dict):
        for key, item in value.items():
            scan_private_text(item, f"{field}.{key}")


def text_mentions_provider(text: str, provider: dict[str, Any]) -> bool:
    needles = {
        str(provider["id"]),
        str(provider["capture_provider"]),
        str(provider["capture_provider"]).replace("_", "-"),
        str(provider["display_name"]),
        str(provider["display_name"]).lower(),
    }
    lowered = text.lower()
    return any(needle and needle.lower() in lowered for needle in needles)


def validate_implemented_path(path: Any, provider_id: str, index: int) -> None:
    label = f"providers[{provider_id}].implemented_paths[{index}]"
    expect_type(path, dict, label)

    kind = require_non_empty_string(path.get("kind"), f"{label}.kind")
    if kind not in ALLOWED_PATH_KINDS:
        fail(f"{label}.kind has unsupported value: {kind}")

    source_format = require_non_empty_string(path.get("source_format"), f"{label}.source_format")
    if any(token in source_format for token in PRIVATE_TEXT_MARKERS):
        fail(f"{label}.source_format contains private path wording")

    fidelity = require_non_empty_string(path.get("fidelity"), f"{label}.fidelity")
    if fidelity not in ALLOWED_FIDELITY:
        fail(f"{label}.fidelity has unsupported value: {fidelity}")

    proof = require_string_list(path.get("proof"), f"{label}.proof")
    if not any("ctx " in item or item.startswith("cargo test") or item == "ctx sources" for item in proof):
        fail(f"{label}.proof must name a public ctx command or cargo test")

    notes = require_string_list(path.get("notes", []), f"{label}.notes", allow_empty=True)
    for note_index, note in enumerate(notes):
        if any(token in note for token in PRIVATE_TEXT_MARKERS):
            fail(f"{label}.notes[{note_index}] contains private path wording")


def validate_provider(provider: Any, index: int, seen_ids: set[str]) -> None:
    label = f"providers[{index}]"
    expect_type(provider, dict, label)

    provider_id = require_non_empty_string(provider.get("id"), f"{label}.id")
    if not PROVIDER_ID_RE.fullmatch(provider_id):
        fail(f"{label}.id must use lowercase snake_case")
    if provider_id in seen_ids:
        fail(f"duplicate provider id: {provider_id}")
    seen_ids.add(provider_id)
    scan_private_text(provider, f"providers[{provider_id}]")

    require_non_empty_string(provider.get("display_name"), f"providers[{provider_id}].display_name")
    require_non_empty_string(provider.get("priority"), f"providers[{provider_id}].priority")
    require_non_empty_string(provider.get("capture_provider"), f"providers[{provider_id}].capture_provider")

    status = require_non_empty_string(provider.get("status"), f"providers[{provider_id}].status")
    if status not in ALLOWED_STATUSES:
        fail(f"providers[{provider_id}].status has unsupported value: {status}")

    public_docs = require_non_empty_string(provider.get("public_docs"), f"providers[{provider_id}].public_docs")
    public_doc_path = require_repo_path(public_docs, f"providers[{provider_id}].public_docs")
    public_doc_text = public_doc_path.read_text(encoding="utf-8")
    if provider["display_name"] not in public_doc_text and provider_id not in public_doc_text:
        fail(f"providers[{provider_id}].public_docs does not mention the provider")

    support_doc_text = SUPPORT_DOC_PATH.read_text(encoding="utf-8")
    support_row = f"| {provider['display_name']} | Supported |"
    if support_row not in support_doc_text:
        fail(f"docs/provider-support.md is missing supported row for {provider_id}")

    tests = require_string_list(provider.get("tests"), f"providers[{provider_id}].tests")
    provider_specific_test = False
    for test_index, test_path in enumerate(tests):
        resolved_test_path = require_repo_path(test_path, f"providers[{provider_id}].tests[{test_index}]")
        if resolved_test_path.is_file() and text_mentions_provider(
            resolved_test_path.read_text(encoding="utf-8", errors="ignore"),
            provider,
        ):
            provider_specific_test = True

    fixture_paths = require_string_list(
        provider.get("fixture_paths", []),
        f"providers[{provider_id}].fixture_paths",
        allow_empty=True,
    )
    for fixture_index, fixture_path in enumerate(fixture_paths):
        require_repo_path(fixture_path, f"providers[{provider_id}].fixture_paths[{fixture_index}]")

    implemented_paths = expect_type(
        provider.get("implemented_paths", []),
        list,
        f"providers[{provider_id}].implemented_paths",
    )
    if not implemented_paths and status not in {"detected_unsupported", "blocked"}:
        fail(f"providers[{provider_id}].implemented_paths must not be empty")
    for path_index, implemented_path in enumerate(implemented_paths):
        validate_implemented_path(implemented_path, provider_id, path_index)

    imports_existing_history = provider.get("imports_existing_history")
    if not isinstance(imports_existing_history, bool):
        fail(f"providers[{provider_id}].imports_existing_history must be boolean")
    if status.startswith("local_import") and not imports_existing_history:
        fail(f"providers[{provider_id}] is {status} but imports_existing_history is false")
    if imports_existing_history and not implemented_paths:
        fail(f"providers[{provider_id}] imports history but has no implemented_paths")

    for bool_field in ("captures_new_runs_passively", "child_sessions_supported"):
        if not isinstance(provider.get(bool_field), bool):
            fail(f"providers[{provider_id}].{bool_field} must be boolean")

    fidelity = expect_type(provider.get("fidelity"), dict, f"providers[{provider_id}].fidelity")
    missing_fidelity = REQUIRED_FIDELITY_FIELDS.difference(fidelity)
    if missing_fidelity:
        fail(f"providers[{provider_id}].fidelity missing fields: {', '.join(sorted(missing_fidelity))}")
    for field in REQUIRED_FIDELITY_FIELDS:
        if not isinstance(fidelity[field], bool):
            fail(f"providers[{provider_id}].fidelity.{field} must be boolean")

    if status == "local_import" and not fixture_paths:
        fail(f"providers[{provider_id}] is local_import but has no fixture_paths")
    if status.startswith("local_import") and not PUBLIC_CLI_TEST_PATHS.intersection(tests):
        paths = ", ".join(sorted(PUBLIC_CLI_TEST_PATHS))
        fail(f"providers[{provider_id}] needs public CLI coverage in one of: {paths}")
    if status.startswith("local_import") and not provider_specific_test:
        fail(f"providers[{provider_id}] has no provider-specific public test references")


def main() -> int:
    try:
        matrix = json.loads(MATRIX_PATH.read_text(encoding="utf-8"))
        expect_type(matrix, dict, "provider support matrix")
        scan_private_text(matrix, "provider support matrix")
        if matrix.get("schema_version") != 1:
            fail("schema_version must be 1")
        require_non_empty_string(matrix.get("scope"), "scope")
        providers = expect_type(matrix.get("providers"), list, "providers")
        if not providers:
            fail("providers must not be empty")

        seen_ids: set[str] = set()
        for index, provider in enumerate(providers):
            validate_provider(provider, index, seen_ids)
    except (OSError, json.JSONDecodeError, MatrixError) as exc:
        print(f"provider support matrix check failed: {exc}", file=sys.stderr)
        return 1

    print("provider support matrix ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
