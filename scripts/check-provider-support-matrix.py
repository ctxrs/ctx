#!/usr/bin/env python3
"""Validate the public provider support matrix.

This is a public truthfulness gate. It checks that documented provider support
has public docs, local capability metadata, and local test coverage. It does not
require live provider runs, real user history, or network access.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = REPO_ROOT / "docs/provider-support-matrix.json"
ALLOWED_STATUSES = {"supported"}
EXPECTED_SUPPORTED_PROVIDER_COUNT = 42
ALLOWED_PATH_KINDS = {"native_import"}
ALLOWED_FIDELITY = {
    "imported",
    "partial",
}
ALLOWED_SESSION_RELATIONSHIP_SUPPORT = {"exact_relationship", "unknown"}
ALLOWED_EVENT_ORIGIN_SUPPORT = {
    "exact_copy",
    "certified_prefix",
    "explicit_no_copy",
    "unknown",
}
ALLOWED_CONFIGURED_ROOT_STATES = {"enabled", "intentional_automatic_exact"}
ALLOWED_CONFIGURED_ROOT_PATH_KINDS = {"directory", "file"}
ALLOWED_CONFIGURED_ROOT_EXPANDERS = {
    "exact_source",
    "claude_home_v1",
    "codex_home_v1",
    "openclaw_state_root_v1",
    "cline_common_data_root_v1",
    "openhands_kind_v1",
}
OPENHANDS_CONFIGURED_ROOT_KINDS = {
    "current-conversations",
    "legacy-persistence",
}
EXPECTED_PROVIDER_LINEAGE_SUPPORT = {
    "codex": ("exact_relationship", "unknown"),
    "pi": ("exact_relationship", "unknown"),
    "open_code": ("exact_relationship", "explicit_no_copy"),
    "crush": ("exact_relationship", "explicit_no_copy"),
    "goose": ("exact_relationship", "unknown"),
    "openclaw": ("exact_relationship", "explicit_no_copy"),
    "hermes": ("exact_relationship", "unknown"),
    "gemini_cli": ("exact_relationship", "explicit_no_copy"),
    "zed": ("exact_relationship", "explicit_no_copy"),
    "mistral_vibe": ("exact_relationship", "unknown"),
    "mux": ("exact_relationship", "explicit_no_copy"),
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
PUBLIC_DOCS_WITH_SELF_CONTAINED_CLAIMS = (
    REPO_ROOT / "README.md",
    REPO_ROOT / "docs/providers.md",
    REPO_ROOT / "docs/provider-support.md",
    REPO_ROOT / "docs/security-checks.md",
)
PUBLIC_PRIVATE_BOUNDARY_SCAN_PATHS = PUBLIC_DOCS_WITH_SELF_CONTAINED_CLAIMS
CODEX_PUBLIC_CLAIM_TEST_SUITE = (
    REPO_ROOT
    / "crates/ctx-history-capture-composition-qualification/tests/provider_lifecycle/codex_child_independence.rs"
)
FORBIDDEN_PUBLIC_CLAIM_RE = re.compile(
    r"ctx-" + r"private|private\s+conformance|conformance\s+evidence|"
    r"proof\s+packet|fixture-backed|source-backed|schema\s+confidence|Full\s+GA",
    re.IGNORECASE,
)
FORBIDDEN_PUBLIC_WORDS = (
    "pro" + "of",
    "evi" + "dence",
    "promo" + "tion",
    "pro" + "mote",
    "ti" + "er",
    "fix" + "ture",
    "fix" + "tures",
    "con" + "formance",
    "pro" + "ven",
)
FORBIDDEN_PUBLIC_TEXT_RE = re.compile(
    r"\b(" + "|".join(FORBIDDEN_PUBLIC_WORDS) + r")\b|"
    r"Full " + "GA|source-" + "backed|fixture-" + "backed|schema " + "confidence",
    re.IGNORECASE,
)
FORBIDDEN_PROVIDER_FIELDS = {"prio" + "rity", "te" + "sts", "fix" + "ture_paths", "block" + "ers"}
FORBIDDEN_PATH_FIELDS = {"pro" + "of"}
REDUNDANT_DEFAULT_CAPABILITY_FIELDS = {
    "default_auto_discovery",
    "default_auto_discovery_scope",
    "supports_default_location",
}
SUPPORT_DOC_PATH = REPO_ROOT / "docs/provider-support.md"
PUBLIC_COVERAGE_PATHS = {
    "crates/ctx-cli-contract-tests/tests/contracts/native_providers.rs",
    "crates/ctx-history-read-application/tests/contracts/search_refresh.rs",
    "crates/ctx-history-read-application/tests/support/search_refresh/core_behaviors.rs",
    "crates/ctx-history-read-application/tests/support/search_refresh/generation_lifecycle.rs",
    "crates/ctx-cli-contract-tests/tests/contracts/support/native_providers/workspace_sources.rs",
    "crates/ctx-cli-contract-tests/tests/contracts/setup_sources_import.rs",
    "crates/ctx-history-capture/src/lib.rs",
    "crates/ctx-history-source-discovery-qualification/tests/configured_roots.rs",
    "crates/ctx-history-source-discovery-qualification/tests/default_discovery.rs",
    "crates/ctx-history-source-discovery-qualification/tests/env_discovery.rs",
    "crates/ctx-history-source-discovery-qualification/tests/manual_unsupported.rs",
    "crates/ctx-history-source-discovery/src/provider_sources/resolvers/simple_tests.rs",
}


def public_coverage_paths() -> list[str]:
    """Return coverage roots plus any recursively split companion modules."""
    paths = set(PUBLIC_COVERAGE_PATHS)
    for relative in PUBLIC_COVERAGE_PATHS:
        companion_root = (REPO_ROOT / relative).with_suffix("")
        if companion_root.is_dir():
            paths.update(
                path.relative_to(REPO_ROOT).as_posix()
                for path in companion_root.rglob("*.rs")
                if path.is_file()
            )
    return sorted(paths)


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


def scan_public_text(value: Any, field: str) -> None:
    if isinstance(value, str):
        if FORBIDDEN_PUBLIC_TEXT_RE.search(value):
            fail(f"{field} contains non-public provider-support wording")
        return
    if isinstance(value, list):
        for index, item in enumerate(value):
            scan_public_text(item, f"{field}[{index}]")
        return
    if isinstance(value, dict):
        for key, item in value.items():
            scan_public_text(item, f"{field}.{key}")


def reject_redundant_default_capability_fields(value: Any, field: str) -> None:
    if isinstance(value, list):
        for index, item in enumerate(value):
            reject_redundant_default_capability_fields(item, f"{field}[{index}]")
        return
    if isinstance(value, dict):
        for key, item in value.items():
            if key in REDUNDANT_DEFAULT_CAPABILITY_FIELDS:
                fail(
                    f"{field}.{key} is redundant; "
                    "every supported provider has automatic discovery",
                )
            reject_redundant_default_capability_fields(item, f"{field}.{key}")


def codex_public_claim_scan_paths(
    suite_path: Path = CODEX_PUBLIC_CLAIM_TEST_SUITE,
) -> tuple[Path, ...]:
    if suite_path.is_file():
        return (suite_path,)
    if not suite_path.is_dir():
        fail(f"public claim test suite does not exist: {suite_path}")
    scan_paths = tuple(sorted(suite_path.glob("*.rs")))
    if not scan_paths:
        fail(f"public claim test suite contains no Rust sources: {suite_path}")
    return scan_paths


def validate_public_claim_docs() -> None:
    scan_paths = PUBLIC_PRIVATE_BOUNDARY_SCAN_PATHS + codex_public_claim_scan_paths()
    for doc_path in scan_paths:
        if not doc_path.exists():
            fail(f"public claim doc does not exist: {doc_path.relative_to(REPO_ROOT)}")
        text = doc_path.read_text(encoding="utf-8")
        match = FORBIDDEN_PUBLIC_CLAIM_RE.search(text)
        if match:
            fail(
                f"{doc_path.relative_to(REPO_ROOT)} contains non-public support-claim wording: "
                f"{match.group(0)}",
            )


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
    if FORBIDDEN_PATH_FIELDS.intersection(path):
        fail(f"{label} contains a non-public field")

    kind = require_non_empty_string(path.get("kind"), f"{label}.kind")
    if kind not in ALLOWED_PATH_KINDS:
        fail(f"{label}.kind has unsupported value: {kind}")

    source_format = require_non_empty_string(path.get("source_format"), f"{label}.source_format")
    if any(token in source_format for token in PRIVATE_TEXT_MARKERS):
        fail(f"{label}.source_format contains private path wording")

    fidelity = require_non_empty_string(path.get("fidelity"), f"{label}.fidelity")
    if fidelity not in ALLOWED_FIDELITY:
        fail(f"{label}.fidelity has unsupported value: {fidelity}")

    notes = require_string_list(path.get("notes", []), f"{label}.notes", allow_empty=True)
    for note_index, note in enumerate(notes):
        if any(token in note for token in PRIVATE_TEXT_MARKERS):
            fail(f"{label}.notes[{note_index}] contains private path wording")


def validate_configured_root(value: Any, provider_id: str) -> None:
    label = f"providers[{provider_id}].configured_root"
    configured_root = expect_type(value, dict, label)
    state = require_non_empty_string(configured_root.get("state"), f"{label}.state")
    if state not in ALLOWED_CONFIGURED_ROOT_STATES:
        fail(f"{label}.state has unsupported value: {state}")
    if state == "intentional_automatic_exact":
        if set(configured_root) != {"state"}:
            fail(f"{label} intentional state must contain exactly state")
        return

    if set(configured_root) != {"state", "expected_path_kind", "expander"}:
        fail(
            f"{label} enabled state must contain exactly state, expected_path_kind, and expander"
        )
    path_kind = require_non_empty_string(
        configured_root.get("expected_path_kind"),
        f"{label}.expected_path_kind",
    )
    if path_kind not in ALLOWED_CONFIGURED_ROOT_PATH_KINDS:
        fail(f"{label}.expected_path_kind has unsupported value: {path_kind}")

    expander = expect_type(configured_root.get("expander"), dict, f"{label}.expander")
    expander_kind = require_non_empty_string(
        expander.get("kind"),
        f"{label}.expander.kind",
    )
    if expander_kind not in ALLOWED_CONFIGURED_ROOT_EXPANDERS:
        fail(f"{label}.expander.kind has unsupported value: {expander_kind}")
    expected_expander_fields = {"kind"}
    if expander_kind == "exact_source":
        expected_expander_fields.update({"source_format", "route_role"})
        require_non_empty_string(
            expander.get("source_format"),
            f"{label}.expander.source_format",
        )
        require_non_empty_string(
            expander.get("route_role"),
            f"{label}.expander.route_role",
        )
    elif expander_kind == "openhands_kind_v1":
        expected_expander_fields.add("root_kinds")
        root_kinds = require_string_list(
            expander.get("root_kinds"),
            f"{label}.expander.root_kinds",
        )
        if (
            len(root_kinds) != len(set(root_kinds))
            or set(root_kinds) != OPENHANDS_CONFIGURED_ROOT_KINDS
        ):
            fail(f"{label}.expander.root_kinds must list the exact OpenHands root kinds")
    if set(expander) != expected_expander_fields:
        fail(f"{label}.expander contains fields that do not match {expander_kind}")


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
    scan_public_text(provider, f"providers[{provider_id}]")
    reject_redundant_default_capability_fields(provider, f"providers[{provider_id}]")
    if FORBIDDEN_PROVIDER_FIELDS.intersection(provider):
        fail(f"providers[{provider_id}] contains a non-public field")

    require_non_empty_string(provider.get("display_name"), f"providers[{provider_id}].display_name")
    require_non_empty_string(provider.get("capture_provider"), f"providers[{provider_id}].capture_provider")
    validate_configured_root(provider.get("configured_root"), provider_id)

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

    provider_specific_test = False
    for test_index, test_path in enumerate(public_coverage_paths()):
        resolved_test_path = require_repo_path(test_path, f"public_coverage_paths[{test_index}]")
        if text_mentions_provider(
            resolved_test_path.read_text(encoding="utf-8", errors="ignore"),
            provider,
        ):
            provider_specific_test = True

    implemented_paths = expect_type(
        provider.get("implemented_paths", []),
        list,
        f"providers[{provider_id}].implemented_paths",
    )
    if not implemented_paths:
        fail(f"providers[{provider_id}].implemented_paths must not be empty")
    for path_index, implemented_path in enumerate(implemented_paths):
        validate_implemented_path(implemented_path, provider_id, path_index)

    require_string_list(
        provider.get("history_locations"),
        f"providers[{provider_id}].history_locations",
    )

    imports_existing_history = provider.get("imports_existing_history")
    if not isinstance(imports_existing_history, bool):
        fail(f"providers[{provider_id}].imports_existing_history must be boolean")
    if not imports_existing_history:
        fail(f"providers[{provider_id}] is supported but imports_existing_history is false")
    if imports_existing_history and not implemented_paths:
        fail(f"providers[{provider_id}] imports history but has no implemented_paths")

    for bool_field in ("captures_new_runs_passively", "child_sessions_supported"):
        if not isinstance(provider.get(bool_field), bool):
            fail(f"providers[{provider_id}].{bool_field} must be boolean")

    lineage = expect_type(
        provider.get("lineage_support"),
        dict,
        f"providers[{provider_id}].lineage_support",
    )
    if set(lineage) != {"session_relationship", "event_origin"}:
        fail(
            f"providers[{provider_id}].lineage_support must contain exactly "
            "session_relationship and event_origin"
        )
    relationship = require_non_empty_string(
        lineage.get("session_relationship"),
        f"providers[{provider_id}].lineage_support.session_relationship",
    )
    if relationship not in ALLOWED_SESSION_RELATIONSHIP_SUPPORT:
        fail(
            f"providers[{provider_id}].lineage_support.session_relationship "
            f"has unsupported value: {relationship}"
        )
    event_origin = require_non_empty_string(
        lineage.get("event_origin"),
        f"providers[{provider_id}].lineage_support.event_origin",
    )
    if event_origin not in ALLOWED_EVENT_ORIGIN_SUPPORT:
        fail(
            f"providers[{provider_id}].lineage_support.event_origin "
            f"has unsupported value: {event_origin}"
        )

    fidelity = expect_type(provider.get("fidelity"), dict, f"providers[{provider_id}].fidelity")
    missing_fidelity = REQUIRED_FIDELITY_FIELDS.difference(fidelity)
    if missing_fidelity:
        fail(f"providers[{provider_id}].fidelity missing fields: {', '.join(sorted(missing_fidelity))}")
    for field in REQUIRED_FIDELITY_FIELDS:
        if not isinstance(fidelity[field], bool):
            fail(f"providers[{provider_id}].fidelity.{field} must be boolean")

    require_string_list(
        provider.get("limitations", []),
        f"providers[{provider_id}].limitations",
        allow_empty=True,
    )
    if not provider_specific_test:
        fail(f"providers[{provider_id}] has no provider-specific public test references")


def validate_provider_lineage_claims(providers: list[dict[str, Any]]) -> None:
    for provider in providers:
        provider_id = str(provider.get("id"))
        expected_relationship, expected_origin = EXPECTED_PROVIDER_LINEAGE_SUPPORT.get(
            provider_id,
            ("unknown", "unknown"),
        )
        lineage = provider.get("lineage_support", {})
        observed = (
            lineage.get("session_relationship"),
            lineage.get("event_origin"),
        )
        expected = (expected_relationship, expected_origin)
        if observed != expected:
            fail(
                f"providers[{provider_id}].lineage_support must be "
                f"{expected_relationship}/{expected_origin}, got "
                f"{observed[0]}/{observed[1]}"
            )


def validate_detected_unsupported_sources(
    entries: Any, supported_ids: set[str]
) -> None:
    entries = expect_type(entries, list, "detected_unsupported_sources")
    seen: set[str] = set()
    for index, entry in enumerate(entries):
        label = f"detected_unsupported_sources[{index}]"
        expect_type(entry, dict, label)
        expected_fields = {
            "id",
            "display_name",
            "capture_provider",
            "source_format",
            "history_locations",
            "importable",
            "reason",
            "public_docs",
        }
        if set(entry) != expected_fields:
            fail(f"{label} must contain the exact unsupported-source fields")
        provider_id = require_non_empty_string(entry["id"], f"{label}.id")
        if provider_id in seen or provider_id in supported_ids:
            fail(f"{label}.id duplicates another support classification")
        seen.add(provider_id)
        require_non_empty_string(entry["display_name"], f"{label}.display_name")
        require_non_empty_string(entry["capture_provider"], f"{label}.capture_provider")
        require_non_empty_string(entry["source_format"], f"{label}.source_format")
        require_string_list(entry["history_locations"], f"{label}.history_locations")
        if entry["importable"] is not False:
            fail(f"{label}.importable must be false")
        require_non_empty_string(entry["reason"], f"{label}.reason")
        public_docs = require_repo_path(entry["public_docs"], f"{label}.public_docs")
        if entry["display_name"] not in public_docs.read_text(encoding="utf-8"):
            fail(f"{label}.public_docs does not mention the detected provider")


def main() -> int:
    try:
        matrix = json.loads(MATRIX_PATH.read_text(encoding="utf-8"))
        expect_type(matrix, dict, "provider support matrix")
        scan_private_text(matrix, "provider support matrix")
        if matrix.get("schema_version") != 2:
            fail("schema_version must be 2")
        require_non_empty_string(matrix.get("scope"), "scope")
        capability_values = expect_type(
            matrix.get("lineage_capability_values"),
            dict,
            "lineage_capability_values",
        )
        relationship_values = require_string_list(
            capability_values.get("session_relationship"),
            "lineage_capability_values.session_relationship",
        )
        event_origin_values = require_string_list(
            capability_values.get("event_origin"),
            "lineage_capability_values.event_origin",
        )
        if (
            set(capability_values) != {"session_relationship", "event_origin"}
            or set(relationship_values) != ALLOWED_SESSION_RELATIONSHIP_SUPPORT
            or set(event_origin_values) != ALLOWED_EVENT_ORIGIN_SUPPORT
        ):
            fail("lineage_capability_values must list the exact supported values")
        if matrix.get("custom_history_lineage_support") != {
            "legacy": {
                "session_relationship": "unknown",
                "event_origin": "unknown",
            },
            "provider_native_v1": {
                "session_relationship": "exact_relationship",
                "event_origin": "exact_copy",
            },
            "command_only_plugin": {
                "session_relationship": "unknown",
                "event_origin": "unknown",
            },
        }:
            fail("custom_history_lineage_support does not match admitted custom contracts")
        providers = expect_type(matrix.get("providers"), list, "providers")
        if len(providers) != EXPECTED_SUPPORTED_PROVIDER_COUNT:
            fail(
                "providers must contain exactly "
                f"{EXPECTED_SUPPORTED_PROVIDER_COUNT} supported rows, found {len(providers)}"
            )
        validate_public_claim_docs()

        seen_ids: set[str] = set()
        for index, provider in enumerate(providers):
            validate_provider(provider, index, seen_ids)
        if len(seen_ids) != EXPECTED_SUPPORTED_PROVIDER_COUNT:
            fail("supported provider classification must not be vacuous")
        validate_provider_lineage_claims(providers)
        validate_detected_unsupported_sources(
            matrix.get("detected_unsupported_sources"), seen_ids
        )
    except (OSError, json.JSONDecodeError, MatrixError) as exc:
        print(f"provider support matrix check failed: {exc}", file=sys.stderr)
        return 1

    print("provider support matrix ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
