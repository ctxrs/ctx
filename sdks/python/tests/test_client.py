from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
import inspect
import typing
from unittest import mock
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "examples"))

from ctx_agent_history import (
    API_VERSION,
    HostedConfig,
    HostedTransportNotImplementedError,
    AgentHistoryClient,
    SearchContentScope as PublicSearchContentScope,
)
from ctx_agent_history.errors import CtxAgentHistoryCliError, CtxAgentHistoryProtocolError
from ctx_agent_history.errors import CtxAgentHistoryTimeoutError, CtxAgentHistoryValidationError
from ctx_agent_history.agent_history_v1 import normalize_event, normalize_import, normalize_sources, normalize_status
from ctx_agent_history.transport import LocalCliAdapter
from ctx_agent_history.types import (
    AgentHistoryErrorCode,
    Event,
    McpExchange,
    McpToolCall,
    SearchContentScope,
    SearchHit,
)
import dogfood_local


class LocalCliAdapterTests(unittest.TestCase):
    def test_sources_and_import_preserve_legitimate_nested_source_semantics(self) -> None:
        acquisition = {
            "source": "local_scan",
            "cursor": "opaque-checkpoint",
        }
        sources = normalize_sources(
            {
                "sources": [
                    {
                        "provider": "codex",
                        "path": "/configured/root",
                        "status": "available",
                        "importable": True,
                        "acquisition": acquisition,
                    }
                ]
            }
        )
        self.assertEqual(sources[0]["acquisition"], acquisition)

        imported = normalize_import(
            {
                "resume": False,
                "totals": {},
                "sources": [{"source": acquisition}],
            }
        )
        self.assertEqual(imported["sources"][0]["source"], acquisition)

    def test_public_aliases_have_typed_signatures(self) -> None:
        show_event = inspect.signature(AgentHistoryClient.showEvent)
        show_session = inspect.signature(AgentHistoryClient.showSession)

        for signature in (show_event, show_session):
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, {p.kind for p in signature.parameters.values()})

        show_event_hints = typing.get_type_hints(AgentHistoryClient.showEvent)
        show_session_hints = typing.get_type_hints(AgentHistoryClient.showSession)
        search_hints = typing.get_type_hints(AgentHistoryClient.search)
        transport_search_hints = typing.get_type_hints(LocalCliAdapter.search)
        search_hit_hints = typing.get_type_hints(SearchHit)
        event_hints = typing.get_type_hints(Event)
        mcp_tool_call_hints = typing.get_type_hints(McpToolCall)
        mcp_exchange_hints = typing.get_type_hints(McpExchange)
        self.assertEqual(show_event_hints["event_id"], str)
        self.assertEqual(show_event_hints["return"].__name__, "ShowEventResponse")
        self.assertEqual(show_session_hints["session_id"], str)
        self.assertEqual(show_session_hints["return"].__name__, "ShowSessionResponse")
        self.assertEqual(search_hints["content_scope"], typing.Optional[SearchContentScope])
        self.assertEqual(
            transport_search_hints["content_scope"],
            typing.Optional[SearchContentScope],
        )
        self.assertEqual(
            SearchContentScope.__args__,
            ("all", "transcript", "calls", "outputs"),
        )
        self.assertIs(PublicSearchContentScope, SearchContentScope)
        self.assertEqual(search_hit_hints["rank"], typing.Optional[float])
        self.assertEqual(search_hit_hints["retrievalScore"], typing.Optional[float])
        self.assertEqual(event_hints["mcpToolCall"], McpToolCall)
        self.assertEqual(event_hints["mcpExchange"], McpExchange)
        self.assertEqual(mcp_tool_call_hints, {"server": str, "tool": str})
        self.assertEqual(McpToolCall.__required_keys__, frozenset({"server", "tool"}))
        self.assertEqual(mcp_exchange_hints["providerCallId"], str)
        self.assertEqual(McpExchange.__required_keys__, frozenset({"providerCallId"}))

    def test_mcp_tool_call_metadata_is_exact_bounded_and_omits_absence(self) -> None:
        result = normalize_event(
            {
                "event": {
                    "mcp_tool_call": {
                        "server": "mcp-サーバー-🦀",
                        "tool": "検索/工具/🛠️",
                    },
                    "future_event_field": {"preserved": True},
                },
                "events": [{}],
            }
        )

        call = result["event"]["mcpToolCall"]
        self.assertEqual(call["server"], "mcp-サーバー-🦀")
        self.assertEqual(call["tool"], "検索/工具/🛠️")
        self.assertEqual(result["event"]["futureEventField"], {"preserved": True})
        self.assertNotIn("mcpToolCall", result["events"][0])
        self.assertEqual(json.loads(json.dumps(call, ensure_ascii=False)), call)

        exact = normalize_event(
            {"event": {"mcpToolCall": {"server": " ", "tool": "🦀" * 16_384}}, "events": []}
        )
        self.assertEqual(len(exact["event"]["mcpToolCall"]["tool"].encode()), 64 * 1024)

        for invalid in (
            {"server": "server"},
            {"server": "server", "tool": "tool", "futureLabel": True},
            {"server": "", "tool": "tool"},
            {"server": "server", "tool": "a" * (64 * 1024 + 1)},
            {"server": "server", "tool": 7},
            {"server": "server", "tool": "\ud800"},
            None,
        ):
            with self.subTest(invalid=invalid):
                with self.assertRaises(CtxAgentHistoryProtocolError):
                    normalize_event({"event": {"mcpToolCall": invalid}, "events": []})

    def test_raw_mcp_tool_call_duplicates_are_rejected_structurally(self) -> None:
        fixture_dir = (
            Path(__file__).resolve().parents[3]
            / "contracts"
            / "agent-history-v1"
            / "fixtures"
            / "adversarial"
        )
        client = AgentHistoryClient.local(ctx_binary="ctx-test")
        invalid_paths = sorted(fixture_dir.glob("duplicate-*.json")) + sorted(
            fixture_dir.glob("invalid-mcp-tool-call-transformed-*.json")
        )
        invalid_paths.extend(
            fixture_dir / name
            for name in (
                "invalid-mcp-tool-call-outer-alias-collision.json",
                "invalid-mcp-tool-call-outer-mixed-case.json",
                "invalid-mcp-tool-call-outer-repeated-separator.json",
                "invalid-mcp-tool-call-outer-trailing-separator.json",
                "invalid-mcp-tool-call-outer-camel-snake.json",
                "duplicate-event-mcp-exchange-snake.json",
                "duplicate-mcp-exchange-captured-value.json",
                "invalid-mcp-exchange-explicit-null.json",
                "invalid-mcp-exchange-outer-alias-collision.json",
                "invalid-mcp-exchange-unknown-field.json",
                "invalid-mcp-exchange-normalized-body-missing-event-text.json",
                "invalid-mcp-exchange-normalized-body-empty-event-text.json",
                "invalid-mcp-exchange-unsafe-duration-ns.json",
                "invalid-mcp-exchange-unsafe-observed-encoded-bytes.json",
            )
        )
        for path in invalid_paths:
            completed = subprocess.CompletedProcess(
                ["ctx-test", "show", "event"],
                0,
                stdout=path.read_text(encoding="utf-8"),
                stderr="",
            )
            with self.subTest(fixture=path.name):
                with mock.patch(
                    "ctx_agent_history.transport.run_local_cli",
                    return_value=completed,
                ):
                    with self.assertRaises(CtxAgentHistoryProtocolError):
                        client.show_event("event-1")

        repeated = fixture_dir / "valid-repeated-string-contents.json"
        completed = subprocess.CompletedProcess(
            ["ctx-test", "show", "event"],
            0,
            stdout=repeated.read_text(encoding="utf-8"),
            stderr="",
        )
        with mock.patch("ctx_agent_history.transport.run_local_cli", return_value=completed):
            event = client.show_event("event-1")["event"]["event"]
        self.assertEqual(event["mcpToolCall"], {"server": "server server", "tool": "tool tool"})
        self.assertEqual(
            event["text"],
            "server tool mcpToolCall mcp_tool_call server tool mcpToolCall mcp_tool_call",
        )

        aliases = fixture_dir / "valid-mcp-tool-call-outer-aliases.json"
        completed = subprocess.CompletedProcess(
            ["ctx-test", "show", "event"],
            0,
            stdout=aliases.read_text(encoding="utf-8"),
            stderr="",
        )
        with mock.patch("ctx_agent_history.transport.run_local_cli", return_value=completed):
            result = client.show_event("event-1")["event"]
        self.assertEqual(result["event"]["mcpToolCall"]["server"], "snake-server")
        self.assertEqual(result["event"]["mcpToolCalls"], {"note": "ordinary unknown"})
        self.assertEqual(result["event"]["futureEventField"], "snake-extra")
        self.assertEqual(result["events"][0]["mcpToolCall"]["server"], "camel-server")
        self.assertEqual(result["events"][0]["mcpToolCalls"], {"note": "ordinary unknown"})
        self.assertEqual(result["events"][0]["futureEventField"], "camel-extra")

    def test_mcp_exchange_is_typed_lossless_and_preserves_captured_json_keys(self) -> None:
        fixture_root = (
            Path(__file__).resolve().parents[3]
            / "contracts"
            / "agent-history-v1"
            / "fixtures"
        )
        fixture = json.loads(
            (fixture_root / "show-event.mcp-tool-call.json").read_text(encoding="utf-8")
        )
        result = normalize_event(fixture["event"])
        exchange = result["event"]["mcpExchange"]
        self.assertEqual(exchange["providerCallId"], "native-call-呼び出し-🦀")
        self.assertEqual(exchange["response"]["durationNs"], (1 << 53) - 1)
        arguments = exchange["invocation"]["arguments"]["value"]
        self.assertIn("snake_key", arguments)
        self.assertNotIn("snakeKey", arguments)
        self.assertIsNone(arguments["nested"]["items"][1]["deep_null"])
        self.assertEqual(
            result["events"][2]["mcpExchange"]["response"]["text"]["observedEncodedBytes"],
            (1 << 53) - 1,
        )
        self.assertNotIn("mcpExchange", result["events"][3])

        normalized = normalize_event(
            {
                "event": {
                    "text": "body",
                    "mcp_exchange": {
                        "provider_call_id": "call",
                        "invocation": {
                            "server": "server",
                            "tool": "tool",
                            "arguments": {
                                "capture_status": "present",
                                "value": {"snake_key": {"deep_null": None}},
                            },
                        },
                        "response": {
                            "status": "succeeded",
                            "text": {"capture_status": "normalized_body"},
                            "payload": {
                                "capture_status": "present",
                                "value": {"result_key": ["雪", None]},
                            },
                        },
                    },
                },
                "events": [],
            }
        )
        self.assertEqual(
            normalized["event"]["mcpExchange"]["response"]["payload"]["value"],
            {"result_key": ["雪", None]},
        )

    def test_non_finite_json_constants_are_rejected(self) -> None:
        client = AgentHistoryClient.local(ctx_binary="ctx-test")
        for constant in ("NaN", "Infinity", "-Infinity"):
            completed = subprocess.CompletedProcess(
                ["ctx-test", "status"],
                0,
                stdout=f'{{"initialized":true,"future":{constant}}}',
                stderr="",
            )
            with self.subTest(constant=constant):
                with mock.patch(
                    "ctx_agent_history.transport.run_local_cli",
                    return_value=completed,
                ):
                    with self.assertRaises(CtxAgentHistoryProtocolError) as raised:
                        client.status()
                self.assertEqual(raised.exception.code, "decode_error")
                self.assertEqual(raised.exception.message, "ctx returned invalid JSON")

    def test_status_uses_local_cli_json(self) -> None:
        with fake_ctx() as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli), data_root="/tmp/ctx-data")

            result = client.status()

        self.assertEqual(result["contractVersion"], "agent-history-v1")
        self.assertEqual(result["schemaVersion"], 1)
        self.assertEqual(result["operation"], "status")
        self.assertEqual(result["backend"], {"kind": "local", "dataRoot": "/tmp/ctx-data"})
        self.assertTrue(result["status"]["initialized"])
        self.assertTrue(result["status"]["localOnly"])
        self.assertEqual(result["status"]["lexical"]["generationId"], "gen-1")
        self.assertNotIn("futureField", result["status"])

    def test_status_counters_use_the_exact_cross_sdk_integer_domain(self) -> None:
        maximum = (1 << 53) - 1
        normalized = normalize_status(
            {
                "initialized": True,
                "indexed_items": maximum,
                "indexed_sessions": maximum,
                "indexed_events": maximum,
                "indexed_sources": maximum,
            }
        )
        self.assertEqual(normalized["indexedItems"], maximum)
        self.assertEqual(normalized["indexedSessions"], maximum)
        self.assertEqual(normalized["indexedEvents"], maximum)
        self.assertEqual(normalized["indexedSources"], maximum)

        for rejected in ((1 << 53) + 1, (1 << 64) - 1):
            with self.subTest(rejected=rejected):
                with self.assertRaises(CtxAgentHistoryProtocolError) as raised:
                    normalize_status({"initialized": True, "indexed_items": rejected})
                self.assertEqual(raised.exception.code, "decode_error")
                self.assertEqual(raised.exception.details["field"], "indexedItems")

    def test_local_cli_forces_analytics_off_after_ambient_and_user_env(self) -> None:
        completed = subprocess.CompletedProcess(
            ["ctx", "status", "--format=json"],
            0,
            stdout='{"initialized":true,"local_only":true}',
            stderr="",
        )
        with mock.patch.dict(os.environ, {"CTX_ANALYTICS_ENABLED": "true"}):
            with mock.patch(
                "ctx_agent_history.transport.run_local_cli",
                return_value=completed,
            ) as run:
                client = AgentHistoryClient.local(
                    env={"CTX_ANALYTICS_ENABLED": "true"},
                )

                client.status()

        child_env = run.call_args.kwargs["env"]
        self.assertEqual(child_env["CTX_ANALYTICS_ENABLED"], "false")

    def test_init_sources_import_sync_search_and_inspect_methods(self) -> None:
        with fake_ctx() as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli))

            self.assertEqual(client.init()["operation"], "init")
            self.assertEqual(client.sources()["operation"], "sources")
            self.assertEqual(client.import_(provider="codex", resume=True)["operation"], "import")
            self.assertEqual(
                client.sync(provider="codex", path="/tmp/history.jsonl")["operation"],
                "sync",
            )
            self.assertEqual(
                client.search(
                    "sqlite",
                    provider="codex",
                    workspace="repo",
                    since="30d",
                    event_type="message",
                    file="src/lib.rs",
                    session="session-1",
                    terms=["storage", "fts"],
                    events=True,
                    primary_only=True,
                    include_subagents=True,
                    limit=3,
                    refresh="off",
                    include_current_session=True,
                )["operation"],
                "search",
            )
            self.assertEqual(client.show_event("event-1", window=2)["operation"], "showEvent")
            self.assertEqual(client.showEvent("event-1")["operation"], "showEvent")
            self.assertEqual(
                client.show_session("session-1", mode="full")["operation"],
                "showSession",
            )
            self.assertEqual(client.showSession("session-1")["operation"], "showSession")

    def test_search_requires_query_term_or_file_before_cli(self) -> None:
        with fake_ctx(fail=True) as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli))

            for call in (
                lambda: client.search(),
                lambda: client.search(refresh="off", limit=5),
                lambda: client.search("   "),
            ):
                with self.subTest(call=call):
                    with self.assertRaises(CtxAgentHistoryValidationError) as raised:
                        call()
                    self.assertEqual(raised.exception.code, "invalid_request")

    def test_search_backend_and_semantic_weight_flags_are_optional(self) -> None:
        adapter = RecordingSearchAdapter()
        client = AgentHistoryClient(adapter)

        client.search("semantic defaults")
        client.search("semantic override", backend="hybrid", semantic_weight=0.8, refresh="off")

        self.assertNotIn("--backend", adapter.calls[0])
        self.assertNotIn("--semantic-weight", adapter.calls[0])
        self.assertNotIn("--content-scope", adapter.calls[0])
        self.assertEqual(
            adapter.calls[1],
            [
                "search",
                "--format=json",
                "semantic override",
                "--backend",
                "hybrid",
                "--semantic-weight",
                "0.8",
                "--refresh",
                "off",
            ],
        )

    def test_search_forwards_exactly_one_class_aware_content_scope(self) -> None:
        adapter = RecordingSearchAdapter()
        client = AgentHistoryClient(adapter)

        client.search("tool calls", content_scope="calls")

        args = adapter.calls[0]
        self.assertEqual(args.count("--content-scope"), 1)
        self.assertEqual(args[args.index("--content-scope") + 1], "calls")

    def test_search_rejects_content_scope_with_event_type_before_transport_or_spawn(self) -> None:
        adapter = RecordingSearchAdapter()
        client = AgentHistoryClient(adapter)

        for call in (
            lambda: client.search("messages", content_scope="all", event_type="message"),
            lambda: adapter.search("messages", content_scope="all", event_type="message"),
        ):
            with self.subTest(call=call):
                with self.assertRaises(CtxAgentHistoryValidationError) as raised:
                    call()
                self.assertEqual(raised.exception.code, "invalid_request")
                self.assertEqual(
                    str(raised.exception),
                    "search content_scope and event_type are mutually exclusive",
                )
                self.assertEqual(
                    raised.exception.details,
                    {"content_scope": "all", "event_type": "message"},
                )

        self.assertEqual(adapter.calls, [])

    def test_search_rejects_invalid_content_scope_before_transport_or_spawn(self) -> None:
        adapter = RecordingSearchAdapter()
        client = AgentHistoryClient(adapter)

        for content_scope in ("messages", "All", "outputs ", 1, {}):
            for call in (
                lambda value=content_scope: client.search("messages", content_scope=value),
                lambda value=content_scope: adapter.search("messages", content_scope=value),
            ):
                with self.subTest(content_scope=content_scope, call=call):
                    with self.assertRaises(CtxAgentHistoryValidationError) as raised:
                        call()
                    self.assertEqual(raised.exception.code, "invalid_request")
                    self.assertEqual(
                        str(raised.exception),
                        "search content_scope must be one of all, transcript, calls, outputs",
                    )
                    self.assertEqual(
                        raised.exception.details,
                        {"content_scope": content_scope},
                    )

        self.assertEqual(adapter.calls, [])

    def test_search_normalization_camelizes_retrieval_json(self) -> None:
        adapter = RecordingSearchAdapter(
            {
                "payloadType": "search_results",
                "query": "semantic retrieval",
                "retrieval": {
                    "requested_mode": "hybrid",
                    "effective_mode": "lexical",
                    "semantic_weight": 0.0,
                    "semantic_status": "fallback",
                    "semantic_fallback_code": "semantic_retrieval_failed",
                    "semantic_fallback": "semantic_retrieval_failed",
                    "coverage": {
                        "embedded_items": 4,
                        "embedded_chunks": 9,
                        "searchable_items": 12,
                        "indexed_now": 1,
                    },
                    "diagnostics": {"query_embed_ms": 2, "vector_scan_ms": 3},
                },
                "results": [
                    {
                        "result_type": "event",
                        "recordType": "event",
                        "itemType": "event",
                        "result_scope": "event",
                        "provider": "codex",
                        "provider_session_id": "codex-resume-uuid",
                        "source_format": "codex_session_jsonl",
                        "rank": 1,
                        "retrieval_score": 0.98,
                        "citations": [{"target_type": "event", "label": "codex event"}],
                    }
                ],
                "result_window": {
                    "limit": 1,
                    "returned": 1,
                    "more_available": True,
                },
            }
        )
        client = AgentHistoryClient(adapter)

        result = client.search("semantic retrieval")

        retrieval = result["search"]["retrieval"]
        self.assertEqual(retrieval["requestedMode"], "hybrid")
        self.assertEqual(retrieval["effectiveMode"], "lexical")
        self.assertEqual(retrieval["semanticWeight"], 0.0)
        self.assertEqual(retrieval["semanticFallbackCode"], "semantic_retrieval_failed")
        self.assertEqual(retrieval["semanticFallback"], "semantic_retrieval_failed")
        self.assertEqual(retrieval["coverage"]["embeddedItems"], 4)
        self.assertEqual(retrieval["coverage"]["indexedNow"], 1)
        self.assertEqual(retrieval["diagnostics"]["queryEmbedMs"], 2)
        hit = result["search"]["results"][0]
        self.assertNotIn("payloadType", result["search"])
        self.assertNotIn("recordType", hit)
        self.assertNotIn("itemType", hit)
        self.assertEqual(hit["resultType"], "event")
        self.assertEqual(hit["provider"], "codex")
        self.assertEqual(hit["providerSessionId"], "codex-resume-uuid")
        self.assertEqual(hit["sourceFormat"], "codex_session_jsonl")
        self.assertEqual(hit["rank"], 1)
        self.assertEqual(hit["retrievalScore"], 0.98)
        self.assertEqual(hit["citations"][0]["targetType"], "event")
        self.assertEqual(
            result["search"]["resultWindow"],
            {"limit": 1, "returned": 1, "moreAvailable": True},
        )
        self.assertEqual(result["search"]["pagination"], {"limit": 1, "hasMore": True})
        self.assertNotIn("nextCursor", result["search"]["pagination"])

    def test_versioning_reports_sdk_api_transport_and_ctx_version(self) -> None:
        with fake_ctx() as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli))

            version = client.version()

        self.assertEqual(version.api_version, API_VERSION)
        self.assertEqual(version.transport, "local-cli")
        self.assertEqual(version.ctx_version, "ctx 9.9.9")
        self.assertEqual(client.versioning()["api_version"], API_VERSION)

    def test_cli_failure_raises_structured_error(self) -> None:
        with fake_ctx(fail=True) as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli))

            with self.assertRaises(CtxAgentHistoryCliError) as raised:
                client.status()

        self.assertEqual(raised.exception.code, "adapter_error")
        self.assertEqual(raised.exception.exit_code, 42)
        self.assertIn("boom", raised.exception.stderr)
        self.assertIn("command", raised.exception.details)

    def test_invalid_json_raises_protocol_error(self) -> None:
        with fake_ctx(invalid_json=True) as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli))

            with self.assertRaises(CtxAgentHistoryProtocolError) as raised:
                client.status()

        self.assertEqual(raised.exception.code, "decode_error")

    def test_invalid_utf8_raises_protocol_error(self) -> None:
        with fake_ctx(invalid_utf8=True) as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli))

            with self.assertRaises(CtxAgentHistoryProtocolError) as raised:
                client.status()

        self.assertEqual(raised.exception.code, "decode_error")
        self.assertEqual(raised.exception.message, "ctx returned invalid UTF-8")
        self.assertIsInstance(raised.exception.cause, UnicodeDecodeError)
        self.assertIn("command", raised.exception.details)

    def test_invalid_utf8_stderr_on_failed_cli_preserves_cli_error(self) -> None:
        with fake_ctx(invalid_utf8_stderr=True) as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli))

            with self.assertRaises(CtxAgentHistoryCliError) as raised:
                client.status()

        self.assertEqual(raised.exception.code, "adapter_error")
        self.assertEqual(raised.exception.exit_code, 42)
        self.assertIn("\ufffd", raised.exception.stderr)

    def test_invalid_utf8_ctx_version_returns_none(self) -> None:
        with fake_ctx(invalid_utf8=True) as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli))

            version = client.version()

        self.assertIsNone(version.ctx_version)

    def test_timeout_raises_contract_timeout_error(self) -> None:
        with fake_ctx(sleep=True) as cli:
            client = AgentHistoryClient.local(
                ctx_binary=str(cli),
                timeout=0.001,
            )

            with self.assertRaises(CtxAgentHistoryTimeoutError) as raised:
                client.status()

        self.assertEqual(raised.exception.code, "timeout")
        self.assertTrue(raised.exception.retryable)

    def test_hosted_config_is_placeholder(self) -> None:
        client = AgentHistoryClient.hosted(HostedConfig(base_url="https://example.invalid"))

        with self.assertRaises(HostedTransportNotImplementedError) as raised:
            client.status()

        self.assertEqual(raised.exception.code, "not_supported")
        self.assertEqual(raised.exception.details["method"], "status")
        self.assertEqual(raised.exception.details["backend"], "hosted")
        self.assertIsNone(client.version().ctx_version)
        self.assertEqual(client.version().transport, "hosted")

    def test_agent_history_v1_error_codes_are_all_represented(self) -> None:
        codes = {
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

        self.assertEqual(codes, set(AgentHistoryErrorCode.__args__))


class ContractFixtureSmokeTests(unittest.TestCase):
    def test_agent_history_v1_fixtures_conform_to_operation_envelopes(self) -> None:
        root = Path(__file__).resolve().parents[3]
        fixture_dir = root / "contracts" / "agent-history-v1" / "fixtures"
        fixtures = sorted(fixture_dir.glob("*.json")) if fixture_dir.exists() else []
        if not fixtures:
            self.skipTest("contracts/agent-history-v1/fixtures has no JSON fixtures yet")

        for fixture in fixtures:
            with self.subTest(fixture=fixture.name):
                with fixture.open("r", encoding="utf-8") as handle:
                    payload = json.load(handle)
                assert_agent_history_v1_envelope(self, payload)

    def test_old_and_new_event_fixtures_expose_optional_mcp_tool_call(self) -> None:
        root = Path(__file__).resolve().parents[3]
        fixture_dir = root / "contracts" / "agent-history-v1" / "fixtures"
        old = json.loads((fixture_dir / "show-event.window.json").read_text(encoding="utf-8"))
        new = json.loads((fixture_dir / "show-event.mcp-tool-call.json").read_text(encoding="utf-8"))

        self.assertNotIn("mcpToolCall", old["event"]["event"])
        self.assertEqual(new["event"]["event"]["mcpToolCall"]["server"], "mcp-サーバー-🦀")
        self.assertEqual(new["event"]["event"]["mcpToolCall"]["tool"], "検索/工具/🛠️")


class DogfoodExampleTests(unittest.TestCase):
    def test_dogfood_local_example_runs_against_fake_ctx(self) -> None:
        with mock.patch.dict(os.environ, {"CTX_AGENT_HISTORY_CTX": "", "CTX_AGENT_HISTORY_DATA_ROOT": ""}):
            snapshot = dogfood_local.run()

        self.assertEqual(snapshot.status["operation"], "status")
        self.assertEqual(snapshot.init["operation"], "init")
        self.assertEqual(snapshot.imported["operation"], "import")
        self.assertEqual(snapshot.synced["operation"], "sync")
        self.assertEqual(snapshot.search["operation"], "search")
        self.assertEqual(snapshot.event["operation"], "showEvent")
        self.assertEqual(snapshot.session["operation"], "showSession")
        self.assertEqual(snapshot.search["search"]["results"][0]["resultScope"], "event")
        self.assertEqual(
            snapshot.search["search"]["resultWindow"],
            {"limit": 1, "returned": 1, "moreAvailable": True},
        )


class RecordingSearchAdapter(LocalCliAdapter):
    def __init__(self, raw: dict[str, object] | None = None) -> None:
        super().__init__()
        self.raw = raw or {"query": "semantic defaults", "results": []}
        self.calls: list[list[str]] = []

    def _json(self, args: typing.Sequence[str]) -> dict[str, object]:
        self.calls.append(list(args))
        return self.raw


class fake_ctx:
    def __init__(
        self,
        *,
        fail: bool = False,
        invalid_json: bool = False,
        invalid_utf8: bool = False,
        invalid_utf8_stderr: bool = False,
        sleep: bool = False,
    ) -> None:
        self.fail = fail
        self.invalid_json = invalid_json
        self.invalid_utf8 = invalid_utf8
        self.invalid_utf8_stderr = invalid_utf8_stderr
        self.sleep = sleep
        self._tmp: tempfile.TemporaryDirectory[str] | None = None
        self.path: Path | None = None

    def __enter__(self) -> Path:
        self._tmp = tempfile.TemporaryDirectory()
        self.path = Path(self._tmp.name) / "ctx"
        script = _fake_ctx_script(
            fail=self.fail,
            invalid_json=self.invalid_json,
            invalid_utf8=self.invalid_utf8,
            invalid_utf8_stderr=self.invalid_utf8_stderr,
            sleep=self.sleep,
        )
        self.path.write_text(script, encoding="utf-8")
        self.path.chmod(self.path.stat().st_mode | stat.S_IXUSR)
        return self.path

    def __exit__(self, exc_type, exc, tb) -> None:  # type: ignore[no-untyped-def]
        if self._tmp is not None:
            self._tmp.cleanup()


def _fake_ctx_script(
    *,
    fail: bool,
    invalid_json: bool,
    invalid_utf8: bool,
    invalid_utf8_stderr: bool,
    sleep: bool,
) -> str:
    if fail:
        return "#!/usr/bin/env python3\nimport sys\nsys.stderr.write('boom\\n')\nsys.exit(42)\n"
    if invalid_json:
        return "#!/usr/bin/env python3\nprint('not json')\n"
    if invalid_utf8:
        return "#!/usr/bin/env python3\nimport sys\nsys.stdout.buffer.write(b'\\xff\\xfe')\n"
    if invalid_utf8_stderr:
        return "#!/usr/bin/env python3\nimport sys\nsys.stderr.buffer.write(b'\\xff\\xfe')\nsys.exit(42)\n"
    if sleep:
        return "#!/usr/bin/env python3\nimport time\ntime.sleep(1)\nprint('{}')\n"

    return textwrap.dedent(
        """\
        #!/usr/bin/env python3
        import json
        import sys

        args = sys.argv[1:]
        if args == ["--version"]:
            print("ctx 9.9.9")
            raise SystemExit(0)
        if args[:2] == ["--data-root", "/tmp/ctx-data"]:
            args = args[2:]

        command = args[0] if args else ""
        payload = {"schema_version": 1, "command": command, "argv": args}
        if args[:2] == ["show", "event"]:
            payload.update(
                {
                    "payload_type": "event_window",
                    "ctx_event_id": args[2],
                    "ctx_session_id": "session-1",
                    "event": {
                        "ctx_event_id": args[2],
                        "ctx_session_id": "session-1",
                        "event_type": "message",
                        "role": "assistant",
                    },
                    "events": [],
                }
            )
        elif args[:2] == ["show", "session"]:
            payload.update(
                {
                    "payload_type": "session_transcript",
                    "ctx_session_id": args[2],
                    "provider": "codex",
                    "provider_session_id": "provider-session-1",
                    "session": {"provider": "codex"},
                    "events": [],
                    "mode": "lite",
                    "format": "json",
                }
            )
        elif command == "search":
            payload.update(
                {
                    "query": "sqlite",
                    "payload_type": "search_results",
                    "results": [
                        {
                            "result_type": "event",
                            "result_scope": "event",
                            "citations": [{"target_type": "event", "label": "codex event"}],
                        }
                    ],
                    "freshness": {"mode": "off", "status": "skipped"},
                }
            )
        elif command == "sources":
            payload.update({"sources": []})
        elif command == "status":
            payload.update(
                {
                    "lexical": {"status": "ready", "generation_id": "gen-1"},
                    "refresh": {"status": "ready", "generation_id": "gen-1"},
                    "future_field": "preserved",
                }
            )
        elif command == "setup":
            payload.update({"lexical": {"status": "ready", "generation_id": "gen-1"}})
        elif command == "import":
            payload.update({"totals": {}, "sources": []})
        print(json.dumps(payload))
        """
    )


def assert_agent_history_v1_envelope(test: unittest.TestCase, payload: object) -> None:
    test.assertIsInstance(payload, dict)
    if not isinstance(payload, dict):
        return

    test.assertEqual(payload["contractVersion"], "agent-history-v1")
    test.assertEqual(payload["schemaVersion"], 1)
    operation = payload["operation"]
    test.assertIn(operation, EXPECTED_PAYLOAD_KEYS)
    test.assertIn("backend", payload)
    _assert_public_keys_are_camel_case(test, payload)

    payload_key = EXPECTED_PAYLOAD_KEYS[operation]
    test.assertIn(payload_key, payload)
    value = payload[payload_key]
    test.assertIsInstance(value, list if operation == "sources" else dict)

    if operation in {"status", "init"}:
        _assert_required_keys(test, value, {"initialized", "localOnly"})
    elif operation == "sources":
        for source in value:
            _assert_required_keys(test, source, {"provider", "path", "status", "importable"})
    elif operation in {"import", "sync"}:
        _assert_required_keys(test, value, {"resume", "totals"})
    elif operation == "search":
        _assert_required_keys(test, value, {"query", "results", "resultWindow", "pagination"})
        test.assertEqual(value["resultWindow"]["returned"], len(value["results"]))
        test.assertEqual(value["resultWindow"]["limit"], value["pagination"]["limit"])
        test.assertEqual(value["resultWindow"]["moreAvailable"], value["pagination"]["hasMore"])
        test.assertNotIn("nextCursor", value["pagination"])
        for hit in value["results"]:
            _assert_required_keys(
                test,
                hit,
                {"resultScope", "provider", "providerSessionId", "sourceFormat"},
            )
            test.assertEqual(hit["rank"], 1)
            test.assertEqual(hit["retrievalScore"], 0.98)
    elif operation == "showEvent":
        _assert_required_keys(test, value, {"events"})
        _assert_typed_show_events(test, value)
    elif operation == "showSession":
        _assert_required_keys(test, value, {"session", "events"})
        _assert_required_keys(
            test,
            value["session"],
            {"ctxSessionId", "provider", "providerSessionId", "sourceFormat"},
        )
        _assert_typed_show_events(test, value)
    elif operation == "error":
        _assert_required_keys(test, value, {"code", "message", "retryable"})


def _assert_typed_show_events(test: unittest.TestCase, value: object) -> None:
    if not isinstance(value, dict):
        return
    events = value.get("events")
    test.assertIsInstance(events, list)
    if not isinstance(events, list):
        return
    for event in events:
        _assert_required_keys(
            test,
            event,
            {"provider", "providerSessionId", "sourceFormat", "content"},
        )
        test.assertEqual(event["content"]["complete"], True)
        test.assertEqual(event["content"]["policyStatus"], "selected")
        for forbidden in ("source", "sourcePath", "sourceExists", "cursor", "preview"):
            test.assertNotIn(forbidden, event)


def _assert_required_keys(test: unittest.TestCase, payload: object, keys: set[str]) -> None:
    test.assertIsInstance(payload, dict)
    if isinstance(payload, dict):
        missing = keys.difference(payload)
        test.assertFalse(missing, f"missing required keys: {sorted(missing)}")


def _assert_public_keys_are_camel_case(test: unittest.TestCase, payload: object) -> None:
    if isinstance(payload, dict):
        for key, value in payload.items():
            test.assertNotIn("_", str(key), f"non-canonical snake_case key: {key}")
            if key == "value" and payload.get("captureStatus") == "present":
                continue
            _assert_public_keys_are_camel_case(test, value)
    elif isinstance(payload, list):
        for value in payload:
            _assert_public_keys_are_camel_case(test, value)


EXPECTED_PAYLOAD_KEYS = {
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


if __name__ == "__main__":
    unittest.main()
