#!/usr/bin/env python3
"""Validate the shared agent-history-v1 golden fixtures without third-party packages."""

from __future__ import annotations

import json
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CONTRACT = ROOT / "contracts" / "agent-history-v1"
FIXTURES = CONTRACT / "fixtures"

VALID_OPERATIONS = {
    "status",
    "init",
    "sources",
    "import",
    "sync",
    "search",
    "showEvent",
    "showSession",
    "locateEvent",
    "locateSession",
    "error",
}

VALID_ERRORS = {
    "invalid_request",
    "not_found",
    "not_initialized",
    "backend_unavailable",
    "timeout",
    "cancelled",
    "not_supported",
    "adapter_error",
    "decode_error",
    "unknown",
}

PAYLOAD_BY_OPERATION = {
    "status": "status",
    "init": "status",
    "sources": "sources",
    "import": "import",
    "sync": "import",
    "search": "search",
    "showEvent": "event",
    "showSession": "session",
    "locateEvent": "location",
    "locateSession": "location",
    "error": "error",
}

KNOWN_PAYLOAD_KEYS = set(PAYLOAD_BY_OPERATION.values())


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def validate_schema(value, schema, root, path: str) -> None:
    if "$ref" in schema:
        ref = schema["$ref"]
        require(ref.startswith("#/$defs/"), f"{path}: unsupported ref {ref}")
        schema = root["$defs"][ref.removeprefix("#/$defs/")]

    if "const" in schema:
        require(value == schema["const"], f"{path}: expected const {schema['const']!r}")

    if "enum" in schema:
        require(value in schema["enum"], f"{path}: expected one of {schema['enum']!r}")

    expected_type = schema.get("type")
    if expected_type is not None:
        expected = expected_type if isinstance(expected_type, list) else [expected_type]
        require(any(json_type_matches(value, item) for item in expected), f"{path}: bad type")

    if isinstance(value, dict):
        for key in schema.get("required", []):
            require(key in value, f"{path}: missing required key {key!r}")
        properties = schema.get("properties", {})
        for key, item in value.items():
            if key in properties:
                validate_schema(item, properties[key], root, f"{path}.{key}")

    if isinstance(value, list) and "items" in schema:
        for index, item in enumerate(value):
            validate_schema(item, schema["items"], root, f"{path}[{index}]")


def json_type_matches(value, expected: str) -> bool:
    if expected == "object":
        return isinstance(value, dict)
    if expected == "array":
        return isinstance(value, list)
    if expected == "string":
        return isinstance(value, str)
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "null":
        return value is None
    return True


def validate_fixture(path: Path, schema: dict) -> None:
    data = json.loads(path.read_text())
    validate_schema(data, schema, schema, str(path))
    require(data.get("contractVersion") == "agent-history-v1", f"{path}: bad contractVersion")
    require(data.get("schemaVersion") == 1, f"{path}: bad schemaVersion")
    operation = data.get("operation")
    require(operation in VALID_OPERATIONS, f"{path}: bad operation {operation!r}")
    expected_payload = PAYLOAD_BY_OPERATION[operation]
    require(expected_payload in data, f"{path}: missing {expected_payload!r} payload")
    unexpected_payloads = sorted(
        key for key in KNOWN_PAYLOAD_KEYS if key != expected_payload and key in data
    )
    require(
        not unexpected_payloads,
        f"{path}: unexpected payload(s) for {operation}: {unexpected_payloads}",
    )

    backend = data.get("backend")
    if backend is not None:
        require(backend.get("kind") in {"local", "hosted"}, f"{path}: bad backend kind")

    if operation == "error":
        error = data.get("error")
        require(isinstance(error, dict), f"{path}: missing error")
        require(error.get("code") in VALID_ERRORS, f"{path}: bad error code")
        require(isinstance(error.get("message"), str), f"{path}: bad error message")
        require(isinstance(error.get("retryable"), bool), f"{path}: bad retryable")

    if operation == "sources":
        require(isinstance(data.get("sources"), list), f"{path}: missing sources[]")

    if operation == "search":
        search = data.get("search")
        require(isinstance(search, dict), f"{path}: missing search")
        require(isinstance(search.get("results"), list), f"{path}: missing search.results[]")
        for result in search["results"]:
            require("resultScope" in result, f"{path}: result missing resultScope")
        result_window = search.get("resultWindow")
        require(isinstance(result_window, dict), f"{path}: missing search.resultWindow")
        limit = result_window.get("limit")
        returned = result_window.get("returned")
        more_available = result_window.get("moreAvailable")
        require(
            isinstance(limit, int) and not isinstance(limit, bool) and limit >= 0,
            f"{path}: bad search.resultWindow.limit",
        )
        require(
            isinstance(returned, int) and not isinstance(returned, bool) and returned >= 0,
            f"{path}: bad search.resultWindow.returned",
        )
        require(
            isinstance(more_available, bool),
            f"{path}: bad search.resultWindow.moreAvailable",
        )
        require(
            returned == len(search["results"]),
            f"{path}: search.resultWindow.returned does not match search.results",
        )
        require(
            returned <= limit,
            f"{path}: search.resultWindow.returned exceeds limit",
        )
        if more_available:
            require(
                returned == limit,
                f"{path}: search.resultWindow.moreAvailable requires a full window",
            )

        pagination = search.get("pagination")
        require(isinstance(pagination, dict), f"{path}: missing compatibility search.pagination")
        require(
            pagination.get("limit") == limit,
            f"{path}: search.pagination.limit disagrees with resultWindow",
        )
        require(
            pagination.get("hasMore") == more_available,
            f"{path}: search.pagination.hasMore disagrees with resultWindow",
        )
        require(
            "nextCursor" not in pagination,
            f"{path}: search result windows must not invent cursor pagination",
        )

    if operation == "showEvent":
        require(isinstance(data.get("event"), dict), f"{path}: missing event envelope")

    if operation == "showSession":
        require(isinstance(data.get("session"), dict), f"{path}: missing session envelope")

    if operation in {"locateEvent", "locateSession"}:
        location = data.get("location")
        require(isinstance(location, dict), f"{path}: missing location")
        require(isinstance(location.get("source"), dict), f"{path}: missing source location")


def main() -> int:
    schema = json.loads((CONTRACT / "schema.json").read_text())
    require(schema.get("$id"), "schema missing $id")
    fixture_paths = sorted(FIXTURES.glob("*.json"))
    require(fixture_paths, "no fixtures found")
    for path in fixture_paths:
        validate_fixture(path, schema)
    print(f"validated {len(fixture_paths)} agent-history-v1 fixtures")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"agent history contract validation failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
