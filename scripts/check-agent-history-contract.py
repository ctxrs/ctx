#!/usr/bin/env python3
"""Validate the shared agent-history-v1 golden fixtures without third-party packages."""

from __future__ import annotations

import json
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CONTRACT = ROOT / "contracts" / "agent-history-v1"
FIXTURES = CONTRACT / "fixtures"
ADVERSARIAL_FIXTURES = FIXTURES / "adversarial"

VALID_OPERATIONS = {
    "status",
    "init",
    "sources",
    "import",
    "sync",
    "search",
    "showEvent",
    "showSession",
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
    "error": "error",
}

KNOWN_PAYLOAD_KEYS = set(PAYLOAD_BY_OPERATION.values())
FORBIDDEN_EVENT_LOCATOR_FIELDS = {
    "source",
    "sourcePath",
    "sourceExists",
    "cursor",
    "preview",
}
MCP_TOOL_CALL_OUTER_ALIASES = {"mcp_tool_call", "mcpToolCall"}
MCP_TOOL_CALL_OUTER_REJECTION_FIXTURES = {
    "invalid-mcp-tool-call-outer-alias-collision.json": MCP_TOOL_CALL_OUTER_ALIASES,
    "invalid-mcp-tool-call-outer-mixed-case.json": {"mcp_toolCall"},
    "invalid-mcp-tool-call-outer-repeated-separator.json": {"mcp__tool_call"},
    "invalid-mcp-tool-call-outer-trailing-separator.json": {"mcp_tool_call_"},
    "invalid-mcp-tool-call-outer-camel-snake.json": {"mcpTool_call"},
}
SDK_MCP_TOOL_CALL_REJECTION_TESTS = {
    "Rust": ROOT / "crates" / "ctx-sdk" / "src" / "tests.rs",
    "Go": ROOT / "sdks" / "go" / "client_test.go",
    "Python": ROOT / "sdks" / "python" / "tests" / "test_client.py",
    "TypeScript": ROOT / "sdks" / "typescript" / "test" / "client.test.js",
    "JVM": ROOT
    / "sdks"
    / "jvm"
    / "src"
    / "test"
    / "java"
    / "rs"
    / "ctx"
    / "agenthistory"
    / "AgentHistoryClientTest.java",
    ".NET": ROOT
    / "sdks"
    / "dotnet"
    / "tests"
    / "Ctx.AgentHistory.Tests"
    / "Program.cs",
    "Swift": ROOT
    / "sdks"
    / "swift"
    / "Tests"
    / "CtxAgentHistoryTests"
    / "CtxAgentHistoryTests.swift",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


class DuplicateJSONMemberError(ValueError):
    """Raised before Python's JSON object construction can collapse a member."""


def reject_duplicate_object_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateJSONMemberError(f"duplicate JSON object member {key!r}")
        result[key] = value
    return result


def load_json_exact(path: Path):
    try:
        source = path.read_bytes().decode("utf-8", errors="strict")
    except UnicodeDecodeError as exc:
        raise AssertionError(f"{path}: invalid UTF-8") from exc
    return json.loads(source, object_pairs_hook=reject_duplicate_object_pairs)


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

    if isinstance(value, (int, float)) and not isinstance(value, bool):
        if "minimum" in schema:
            require(value >= schema["minimum"], f"{path}: below minimum")
        if "maximum" in schema:
            require(value <= schema["maximum"], f"{path}: above maximum")

    if isinstance(value, str):
        if "minLength" in schema:
            require(len(value) >= schema["minLength"], f"{path}: below minimum length")
        if "maxLength" in schema:
            require(len(value) <= schema["maxLength"], f"{path}: above maximum length")

    if isinstance(value, dict):
        forbidden_property_names = schema.get("propertyNames", {}).get("not", {}).get("enum", [])
        for key in value:
            require(
                key not in forbidden_property_names,
                f"{path}: forbidden property name {key!r}",
            )
        for key in schema.get("required", []):
            require(key in value, f"{path}: missing required key {key!r}")
        properties = schema.get("properties", {})
        for key, item in value.items():
            if key in properties:
                validate_schema(item, properties[key], root, f"{path}.{key}")
            elif schema.get("additionalProperties") is False:
                raise AssertionError(f"{path}: unknown property {key!r}")

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
    data = load_json_exact(path)
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
            for key in ("provider", "providerSessionId", "sourceFormat"):
                require(isinstance(result.get(key), str), f"{path}: bad search result {key}")
            for forbidden in FORBIDDEN_EVENT_LOCATOR_FIELDS:
                require(forbidden not in result, f"{path}: forbidden search field {forbidden}")
            for citation in result.get("citations", []):
                for forbidden in FORBIDDEN_EVENT_LOCATOR_FIELDS:
                    require(
                        forbidden not in citation,
                        f"{path}: forbidden citation field {forbidden}",
                    )
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
        event_result = data.get("event")
        require(isinstance(event_result, dict), f"{path}: missing event envelope")
        validate_show_events(event_result, path)

    if operation == "showSession":
        session_result = data.get("session")
        require(isinstance(session_result, dict), f"{path}: missing session envelope")
        summary = session_result.get("session")
        require(isinstance(summary, dict), f"{path}: missing typed session summary")
        for key in ("ctxSessionId", "provider", "providerSessionId", "sourceFormat"):
            require(isinstance(summary.get(key), str), f"{path}: bad session.{key}")
        validate_show_events(session_result, path)


def validate_status_counter_domain(schema: dict) -> None:
    status_schema = schema["$defs"]["status"]
    maximum = 9_007_199_254_740_991
    for key in ("indexedItems", "indexedSessions", "indexedEvents", "indexedSources"):
        counter_schema = status_schema["properties"][key]
        require(counter_schema.get("minimum") == 0, f"status.{key}: bad minimum")
        require(counter_schema.get("maximum") == maximum, f"status.{key}: bad maximum")
        validate_schema(maximum, counter_schema, schema, f"status.{key}")
        for rejected in (maximum + 2, 18_446_744_073_709_551_615):
            try:
                validate_schema(rejected, counter_schema, schema, f"status.{key}")
            except AssertionError:
                continue
            raise AssertionError(f"status.{key}: accepted out-of-domain value {rejected}")


def validate_show_events(result: dict, path: Path) -> None:
    events = result.get("events")
    require(isinstance(events, list) and events, f"{path}: missing events[]")
    selected = result.get("event")
    if selected is not None:
        require(isinstance(selected, dict), f"{path}: bad selected event")
    for event in events + ([selected] if selected is not None else []):
        for key in ("provider", "providerSessionId", "sourceFormat"):
            require(isinstance(event.get(key), str), f"{path}: bad event.{key}")
        content = event.get("content")
        require(isinstance(content, dict), f"{path}: missing event.content")
        require(isinstance(content.get("complete"), bool), f"{path}: bad content.complete")
        require(
            content.get("policyStatus") in {"selected", "redacted", "omitted"},
            f"{path}: bad content.policyStatus",
        )
        if "mcpToolCall" in event:
            validate_mcp_tool_call(event["mcpToolCall"], f"{path}: event.mcpToolCall")
        for forbidden in FORBIDDEN_EVENT_LOCATOR_FIELDS:
            require(forbidden not in event, f"{path}: forbidden event field {forbidden}")
        for citation in event.get("citations", []):
            for forbidden in FORBIDDEN_EVENT_LOCATOR_FIELDS:
                require(
                    forbidden not in citation,
                    f"{path}: forbidden citation field {forbidden}",
                )


def validate_mcp_tool_call(value, path: str) -> None:
    require(isinstance(value, dict), f"{path}: expected object")
    require(set(value) == {"server", "tool"}, f"{path}: expected exactly server and tool")
    for key in ("server", "tool"):
        component = value[key]
        require(isinstance(component, str), f"{path}.{key}: expected string")
        try:
            decoded_bytes = component.encode("utf-8", errors="strict")
        except UnicodeEncodeError as exc:
            raise AssertionError(f"{path}.{key}: invalid Unicode string") from exc
        require(decoded_bytes, f"{path}.{key}: empty string")
        require(len(decoded_bytes) <= 64 * 1024, f"{path}.{key}: exceeds 64 KiB")


def validate_mcp_tool_call_schema(schema: dict) -> None:
    definition = schema["$defs"]["mcpToolCall"]
    require(definition.get("additionalProperties") is False, "mcpToolCall must be closed")
    require(set(definition.get("required", [])) == {"server", "tool"}, "bad MCP required fields")
    require(set(definition.get("properties", {})) == {"server", "tool"}, "bad MCP properties")
    for key in ("server", "tool"):
        component = definition["properties"][key]
        require(component.get("type") == "string", f"mcpToolCall.{key}: bad type")
        require(component.get("minLength") == 1, f"mcpToolCall.{key}: bad minimum")
        require(component.get("maxLength") == 64 * 1024, f"mcpToolCall.{key}: bad maximum")

    validate_mcp_tool_call({"server": " ", "tool": "🦀" * 16_384}, "mcpToolCall")
    for invalid in (
        {"server": "", "tool": "tool"},
        {"server": "server", "tool": "a" * (64 * 1024 + 1)},
        {"server": "server", "tool": "tool", "future": True},
    ):
        try:
            validate_mcp_tool_call(invalid, "mcpToolCall")
        except AssertionError:
            continue
        raise AssertionError(f"mcpToolCall accepted invalid value: {invalid.keys()}")


def snake_to_camel(value: str) -> str:
    parts = value.split("_")
    if len(parts) == 1:
        return value
    return parts[0] + "".join(part[:1].upper() + part[1:] for part in parts[1:])


def validate_mcp_outer_rejection_fixtures() -> int:
    for name, protected_members in MCP_TOOL_CALL_OUTER_REJECTION_FIXTURES.items():
        path = ADVERSARIAL_FIXTURES / name
        data = load_json_exact(path)
        event = data.get("event")
        require(isinstance(event, dict), f"{path}: missing event object")
        require(
            event.get("future_event_field") == "unrelated-extension",
            f"{path}: missing unrelated unknown-field control",
        )
        require(
            protected_members <= set(event),
            f"{path}: missing protected outer member(s) {sorted(protected_members)}",
        )
        for member in protected_members:
            validate_mcp_tool_call(event[member], f"{path}: event.{member}")

        if protected_members == MCP_TOOL_CALL_OUTER_ALIASES:
            continue
        for member in protected_members:
            require(
                member not in MCP_TOOL_CALL_OUTER_ALIASES,
                f"{path}: transformed preimage is an exact alias",
            )
            require(
                snake_to_camel(member) == "mcpToolCall",
                f"{path}: {member!r} is not a transformed mcpToolCall preimage",
            )
    return len(MCP_TOOL_CALL_OUTER_REJECTION_FIXTURES)


def validate_seven_sdk_rejection_wiring() -> int:
    require(
        len(SDK_MCP_TOOL_CALL_REJECTION_TESTS) == 7,
        "outer MCP rejection matrix must cover exactly seven SDKs",
    )
    for sdk, path in SDK_MCP_TOOL_CALL_REJECTION_TESTS.items():
        source = path.read_text(encoding="utf-8")
        for fixture in MCP_TOOL_CALL_OUTER_REJECTION_FIXTURES:
            require(
                fixture in source,
                f"{sdk} SDK rejection test is not wired to {fixture}",
            )
    return len(SDK_MCP_TOOL_CALL_REJECTION_TESTS)


def validate_adversarial_mcp_fixtures() -> int:
    duplicate_paths = sorted(ADVERSARIAL_FIXTURES.glob("duplicate-*.json"))
    require(duplicate_paths, "no duplicate MCP tool-call fixtures found")
    for path in duplicate_paths:
        try:
            load_json_exact(path)
        except DuplicateJSONMemberError:
            continue
        raise AssertionError(f"{path}: duplicate JSON object member was accepted")

    transformed_paths = sorted(ADVERSARIAL_FIXTURES.glob("invalid-mcp-tool-call-transformed-*.json"))
    require(transformed_paths, "no transformed MCP tool-call fixtures found")
    for path in transformed_paths:
        data = load_json_exact(path)
        event = data["event"]
        call = event.get("mcp_tool_call", event.get("mcpToolCall"))
        try:
            validate_mcp_tool_call(call, f"{path}: event.mcpToolCall")
        except AssertionError:
            continue
        raise AssertionError(f"{path}: transformed nested MCP member was accepted")

    repeated = load_json_exact(ADVERSARIAL_FIXTURES / "valid-repeated-string-contents.json")
    validate_mcp_tool_call(
        repeated["event"]["mcp_tool_call"],
        "valid-repeated-string-contents.json: event.mcp_tool_call",
    )
    require(
        repeated["event"]["text"].count("server tool mcpToolCall mcp_tool_call") == 2,
        "repeated-string control did not preserve harmless repeated contents",
    )

    aliases = load_json_exact(ADVERSARIAL_FIXTURES / "valid-mcp-tool-call-outer-aliases.json")
    validate_mcp_tool_call(
        aliases["event"]["mcp_tool_call"],
        "valid-mcp-tool-call-outer-aliases.json: event.mcp_tool_call",
    )
    validate_mcp_tool_call(
        aliases["events"][0]["mcpToolCall"],
        "valid-mcp-tool-call-outer-aliases.json: events[0].mcpToolCall",
    )
    require(
        aliases["event"]["future_event_field"] == "snake-extra"
        and aliases["events"][0]["futureEventField"] == "camel-extra",
        "outer event extension control was not preserved",
    )
    require(
        aliases["event"]["mcp_tool_calls"] == {"note": "ordinary unknown"}
        and aliases["events"][0]["mcpToolCalls"] == {"note": "ordinary unknown"},
        "nearby unknown outer members did not remain ordinary extensions",
    )
    outer_rejection_count = validate_mcp_outer_rejection_fixtures()
    return len(duplicate_paths) + len(transformed_paths) + outer_rejection_count + 2


def main() -> int:
    schema = load_json_exact(CONTRACT / "schema.json")
    require(schema.get("$id"), "schema missing $id")
    validate_status_counter_domain(schema)
    validate_mcp_tool_call_schema(schema)
    for definition in ("searchHit", "eventResult", "sessionResult", "event", "citation"):
        forbidden = set(schema["$defs"][definition]["propertyNames"]["not"]["enum"])
        require(
            forbidden == FORBIDDEN_EVENT_LOCATOR_FIELDS,
            f"schema {definition} does not forbid exactly the retired event locators",
        )
    fixture_paths = sorted(FIXTURES.glob("*.json"))
    require(fixture_paths, "no fixtures found")
    for path in fixture_paths:
        validate_fixture(path, schema)
    adversarial_count = validate_adversarial_mcp_fixtures()
    sdk_count = validate_seven_sdk_rejection_wiring()
    print(
        f"validated {len(fixture_paths)} agent-history-v1 fixtures "
        f"and {adversarial_count} adversarial MCP fixtures; "
        f"required outer-alias rejection in {sdk_count} SDKs"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"agent history contract validation failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
