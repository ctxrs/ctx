from __future__ import annotations

import json
import os
import stat
import sys
import tempfile
import textwrap
import time
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
    LocalConfig,
    HostedTransportNotImplementedError,
    AgentHistoryClient,
    SearchQueryV1,
    serialize_search_query,
)
from ctx_agent_history.errors import CtxAgentHistoryCliError, CtxAgentHistoryProtocolError
from ctx_agent_history.errors import CtxAgentHistoryTimeoutError, CtxAgentHistoryValidationError
from ctx_agent_history.transport import LocalCliAdapter
from ctx_agent_history.types import AgentHistoryErrorCode
import dogfood_local


SEARCH_QUERY: SearchQueryV1 = {
    "version": "ctx-search-v1",
    "any": [
        {"all": "disk io pressure"},
        {"phrase": "storage latency"},
        {"literal": "logs_2.db"},
        {"semantic": "the indexing job made the workstation sluggish"},
    ],
    "must": [{"all": "codex"}],
    "must_not": [{"phrase": "postgres vacuum"}],
}


def search_execution() -> dict[str, object]:
    return {
        "query_version": "ctx-search-v1",
        "candidate_strategy": "bounded_rrf_v1",
        "resolved": {
            "query_bytes": 8192,
            "clauses": 32,
            "analyzed_tokens_per_clause": 32,
            "candidates_per_positive_seed": 1024,
            "candidate_rows": 16384,
            "retained_candidate_ids": 8192,
            "residual_rows": 8192,
            "verification_bytes": 16777216,
            "verification_lookup_bytes": 16384,
            "hydrated_rows": 256,
            "hydration_input_bytes": 8388608,
            "hydration_input_bytes_per_event": 65536,
            "snippet_input_bytes": 8388608,
            "returned_text_bytes": 524288,
            "serialized_response_bytes": 2097152,
            "results": 5,
            "elapsed_ms": 2500,
        },
        "consumed": {
            "query_bytes": 96,
            "clauses": 7,
            "analyzed_tokens": 18,
            "largest_analyzed_tokens_per_clause": 6,
            "largest_positive_seed_candidates": 20,
            "candidate_rows": 48,
            "retained_candidate_ids": 31,
            "residual_rows": 12,
            "verification_bytes": 4096,
            "largest_verification_lookup_bytes": 512,
            "hydrated_rows": 5,
            "hydration_input_bytes": 2048,
            "largest_hydration_input_bytes": 800,
            "snippet_input_bytes": 1200,
            "returned_results": 1,
            "returned_text_bytes": 128,
            "serialized_response_bytes": 2048,
            "elapsed_ms": 12,
        },
        "semantic": {
            "attempted": True,
            "required": True,
            "readiness": "ready",
            "effective_backend": "hybrid",
            "requested_candidates": 20,
            "eligible_candidates": 18,
            "candidates_supplied": 20,
            "candidates_consumed": 18,
            "candidates_used": 4,
            "coverage": {"indexed_documents": 990, "searchable_documents": 1000},
            "completeness": "partial",
            "incompleteness_reasons": ["semantic_coverage_incomplete"],
            "positive_text_rule_version": "ctx-search-positive-text-v1",
        },
        "requested_result_limit": 5,
        "result_limit": 5,
        "max_result_limit": 200,
        "rrf_k": 60,
        "per_branch_candidate_rows": 1024,
        "clauses_executed": 7,
        "verification_dropped": 0,
        "filter_verification_dropped": 0,
        "candidate_budget_exhausted": False,
        "timed_out": False,
        "truncated": True,
        "truncation_reasons": ["semantic_coverage_incomplete"],
    }


class LocalCliAdapterTests(unittest.TestCase):
    def test_public_aliases_have_typed_signatures(self) -> None:
        show_event = inspect.signature(AgentHistoryClient.showEvent)
        show_session = inspect.signature(AgentHistoryClient.showSession)

        for signature in (show_event, show_session):
            self.assertNotIn(inspect.Parameter.VAR_KEYWORD, {p.kind for p in signature.parameters.values()})

        show_event_hints = typing.get_type_hints(AgentHistoryClient.showEvent)
        show_session_hints = typing.get_type_hints(AgentHistoryClient.showSession)
        self.assertEqual(show_event_hints["event_id"], str)
        self.assertEqual(show_event_hints["return"].__name__, "ShowEventResponse")
        self.assertEqual(show_session_hints["session_id"], str)
        self.assertEqual(show_session_hints["return"].__name__, "ShowSessionResponse")

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
        self.assertEqual(result["status"]["freshness"], {"mode": "off", "status": "skipped"})
        self.assertEqual(result["status"]["futureField"], "preserved")

    def test_init_sources_import_sync_search_and_inspect_methods(self) -> None:
        with fake_ctx() as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli))

            self.assertEqual(client.init(catalog_only=True)["operation"], "init")
            self.assertEqual(client.sources()["operation"], "sources")
            self.assertEqual(client.import_(provider="codex", resume=True)["operation"], "import")
            self.assertEqual(
                client.sync(provider="codex", path="/tmp/history.jsonl")["operation"],
                "sync",
            )
            self.assertEqual(
                client.search(
                    SEARCH_QUERY,
                    provider="custom",
                    history_source="dorkos/default",
                    provider_key="dorkos",
                    source_id="default",
                    source_format="dorkos-history-v1",
                    workspace="repo",
                    since="30d",
                    event_type="message",
                    file="src/lib.rs",
                    session="session-1",
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
            self.assertEqual(client.locate_event("event-1")["operation"], "locateEvent")
            self.assertEqual(client.locateEvent("event-1")["operation"], "locateEvent")
            self.assertEqual(client.locate_session("session-1")["operation"], "locateSession")
            self.assertEqual(client.locateSession("session-1")["operation"], "locateSession")

    def test_search_requires_structured_query_or_file_before_cli(self) -> None:
        with fake_ctx(fail=True) as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli))

            for call in (
                lambda: client.search(),
                lambda: client.search(refresh="off", limit=5),
                lambda: client.search(typing.cast(typing.Any, "   ")),
            ):
                with self.subTest(call=call):
                    with self.assertRaises(CtxAgentHistoryValidationError) as raised:
                        call()
                    self.assertEqual(raised.exception.code, "invalid_request")

            with self.assertRaises(TypeError):
                client.search(SEARCH_QUERY, semantic_weight=0.5)  # type: ignore[call-arg]

    def test_search_serializes_ctx_search_v1_and_optional_backend(self) -> None:
        adapter = RecordingSearchAdapter()
        client = AgentHistoryClient(adapter)

        client.search(SEARCH_QUERY)
        client.search(
            SEARCH_QUERY,
            provider="custom",
            history_source="dorkos/default",
            provider_key="dorkos",
            source_id="default",
            source_format="dorkos-history-v1",
            backend="hybrid",
            refresh="off",
        )

        self.assertEqual(adapter.calls[0][:2], ["search", "--query-json"])
        self.assertEqual(json.loads(adapter.calls[0][2]), SEARCH_QUERY)
        self.assertNotIn("--backend", adapter.calls[0])
        self.assertEqual(
            adapter.calls[1][3:],
            [
                "--provider",
                "custom",
                "--history-source",
                "dorkos/default",
                "--provider-key",
                "dorkos",
                "--source-id",
                "default",
                "--source-format",
                "dorkos-history-v1",
                "--backend",
                "hybrid",
                "--refresh",
                "off",
                "--json",
            ],
        )

    def test_search_query_canonicalizes_before_enforcing_bounds(self) -> None:
        query = {
            "version": "ctx-search-v1",
            "any": [
                {"all": "  disk\t io  pressure "},
                {"all": "disk io pressure"},
                {"literal": "  logs_2.db  raw  "},
            ],
            "must": [],
            "must_not": [{"phrase": " postgres\n vacuum "}],
        }

        self.assertEqual(
            json.loads(serialize_search_query(typing.cast(SearchQueryV1, query))),
            {
                "version": "ctx-search-v1",
                "any": [{"all": "disk io pressure"}, {"literal": "logs_2.db  raw"}],
                "must_not": [{"phrase": "postgres vacuum"}],
            },
        )
        deduped = {
            "version": "ctx-search-v1",
            "any": [{"all": "  cafe\u0301\u00a0\u4e16\u754c  "}] * 33,
        }
        self.assertEqual(
            json.loads(serialize_search_query(typing.cast(SearchQueryV1, deduped)))["any"],
            [{"all": "cafe\u0301 \u4e16\u754c"}],
        )

    def test_search_limit_is_an_integer_from_one_to_two_hundred(self) -> None:
        adapter = RecordingSearchAdapter()
        client = AgentHistoryClient(adapter)

        for limit in (0, 201, 1.5, True):
            with self.subTest(limit=limit):
                with self.assertRaises(CtxAgentHistoryValidationError):
                    client.search(SEARCH_QUERY, limit=typing.cast(typing.Any, limit))
        self.assertEqual(adapter.calls, [])

        client.search(SEARCH_QUERY, limit=1)
        client.search(SEARCH_QUERY, limit=200)
        self.assertIn("1", adapter.calls[0])
        self.assertIn("200", adapter.calls[1])

    def test_search_query_validation_rejects_ambiguous_or_unbounded_shapes(self) -> None:
        invalid = [
            {"version": "ctx-search-v1", "must_not": [{"all": "only negative"}]},
            {"version": "ctx-search-v1", "any": [{"semantic": "one"}, {"semantic": "two"}]},
            {"version": "ctx-search-v1", "must": [{"semantic": "wrong placement"}]},
            {"version": "ctx-search-v1", "any": [{"all": "x", "phrase": "x"}]},
            {"version": "ctx-search-v1", "any": [{"literal": "x"}]},
            {"version": "ctx-search-v1", "any": [{"all": "x"}], "unknown": True},
            {"version": "ctx-search-v1", "any": [{"all": "x"}], "mustNot": []},
            {
                "version": "ctx-search-v1",
                "any": [{"all": f"term-{index}"} for index in range(33)],
            },
            {"version": "ctx-search-v1", "any": [{"all": "x" * 1025}]},
            {"version": "ctx-search-v1", "any": [{"all": "!!!"}]},
            {"version": "ctx-search-v1", "any": [{"all": " ".join(["x"] * 33)}]},
        ]
        for query in invalid:
            with self.subTest(query=query):
                with self.assertRaises(CtxAgentHistoryValidationError):
                    serialize_search_query(typing.cast(typing.Any, query))

    def test_search_normalization_camelizes_retrieval_json(self) -> None:
        adapter = RecordingSearchAdapter(
            {
                "payloadType": "search_results",
                "schema_version": 2,
                "query": SEARCH_QUERY,
                "query_execution": search_execution(),
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
                        "citations": [{"target_type": "event", "label": "codex event"}],
                    }
                ],
            }
        )
        client = AgentHistoryClient(adapter)

        result = client.search(SEARCH_QUERY)

        retrieval = result["search"]["retrieval"]
        self.assertEqual(retrieval["requestedMode"], "hybrid")
        self.assertEqual(retrieval["effectiveMode"], "lexical")
        self.assertNotIn("semanticWeight", retrieval)
        self.assertNotIn("semanticFallbackCode", retrieval)
        self.assertNotIn("semanticFallback", retrieval)
        self.assertEqual(retrieval["coverage"]["embeddedItems"], 4)
        self.assertEqual(retrieval["coverage"]["indexedNow"], 1)
        self.assertEqual(retrieval["diagnostics"]["queryEmbedMs"], 2)
        self.assertEqual(result["search"]["schema_version"], 2)
        self.assertEqual(result["search"]["query"]["must_not"], SEARCH_QUERY["must_not"])
        execution = result["search"]["query_execution"]
        self.assertEqual(execution["resolved"]["verification_bytes"], 16777216)
        self.assertEqual(execution["consumed"]["snippet_input_bytes"], 1200)
        self.assertEqual(execution["requested_result_limit"], 5)
        self.assertEqual(execution["consumed"]["candidate_rows"], 48)
        self.assertEqual(execution["semantic"]["readiness"], "ready")
        self.assertEqual(execution["semantic"]["coverage"]["indexed_documents"], 990)
        self.assertEqual(execution["semantic"]["completeness"], "partial")
        self.assertNotIn("queryExecution", result["search"])
        self.assertNotIn("verificationBytes", execution["resolved"])
        hit = result["search"]["results"][0]
        self.assertNotIn("payloadType", result["search"])
        self.assertNotIn("recordType", hit)
        self.assertNotIn("itemType", hit)
        self.assertEqual(hit["resultType"], "event")
        self.assertEqual(hit["citations"][0]["targetType"], "event")

    def test_search_rejects_pre_v2_response_shape(self) -> None:
        client = AgentHistoryClient(
            RecordingSearchAdapter({"schema_version": 1, "query": "old ambiguous query", "results": []})
        )

        with self.assertRaises(CtxAgentHistoryProtocolError):
            client.search(SEARCH_QUERY)

        for payload, field in (
            ({"schema_version": 2, "query_execution": {}, "results": []}, "query"),
            ({"schema_version": 2, "query": None, "results": []}, "query_execution"),
            ({"schema_version": 2, "query": None, "query_execution": {}}, "results"),
            (
                {
                    "schema_version": 2,
                    "query": None,
                    "query_execution": {},
                    "results": {},
                },
                "results",
            ),
        ):
            with self.subTest(field=field):
                with self.assertRaises(CtxAgentHistoryProtocolError) as raised:
                    AgentHistoryClient(RecordingSearchAdapter(payload)).search(SEARCH_QUERY)
                self.assertEqual(raised.exception.details["field"], field)

        client = AgentHistoryClient(
            RecordingSearchAdapter(
                {
                    "schema_version": 2,
                    "query": SEARCH_QUERY,
                    "queryExecution": search_execution(),
                    "results": [],
                }
            )
        )
        with self.assertRaises(CtxAgentHistoryProtocolError):
            client.search(SEARCH_QUERY)

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

    def test_invalid_utf8_stderr_on_failed_cli_raises_protocol_error(self) -> None:
        with fake_ctx(invalid_utf8_stderr=True) as cli:
            client = AgentHistoryClient.local(ctx_binary=str(cli))

            with self.assertRaises(CtxAgentHistoryProtocolError) as raised:
                client.status()

        self.assertEqual(raised.exception.code, "decode_error")
        self.assertEqual(raised.exception.message, "ctx returned invalid UTF-8")

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

    def test_local_cli_capture_is_concurrent_and_bounded(self) -> None:
        adapter = LocalCliAdapter(LocalConfig(ctx_binary=sys.executable, timeout=2))
        completed = adapter._run(
            [
                "-c",
                "import os,threading; a=threading.Thread(target=lambda:os.write(1,b'a'*200000)); b=threading.Thread(target=lambda:os.write(2,b'b'*200000)); a.start(); b.start(); a.join(); b.join()",
            ]
        )
        self.assertEqual(len(completed.stdout), 200000)
        self.assertEqual(len(completed.stderr), 200000)

        for stream, descriptor, size, cap in (
            ("stdout", 1, 2 * 1024 * 1024 + 1, 2 * 1024 * 1024),
            ("stderr", 2, 256 * 1024 + 1, 256 * 1024),
        ):
            with self.subTest(stream=stream):
                with self.assertRaises(CtxAgentHistoryProtocolError) as raised:
                    adapter._run(
                        [
                            "-c",
                            f"import os; os.write({descriptor}, b'x'*{size})",
                        ]
                    )
                self.assertEqual(raised.exception.details["stream"], stream)
                self.assertEqual(raised.exception.details["cap_bytes"], cap)
                self.assertNotIn("stdout", raised.exception.details)
                self.assertNotIn("stderr", raised.exception.details)

        started = time.monotonic()
        with self.assertRaises(CtxAgentHistoryProtocolError) as raised:
            adapter._run(
                [
                    "-c",
                    "import os,time; os.write(2, b'x'*(256*1024+1)); time.sleep(60)",
                ]
            )
        self.assertEqual(raised.exception.details["stream"], "stderr")
        self.assertLess(time.monotonic() - started, 2)

    @unittest.skipIf(os.name == "nt", "POSIX process-group lifecycle test")
    def test_local_cli_kills_inherited_pipe_descendant_on_capture_failure(self) -> None:
        adapter = LocalCliAdapter(LocalConfig(ctx_binary=sys.executable, timeout=2))
        with tempfile.TemporaryDirectory() as directory:
            pid_path = Path(directory) / "child.pid"
            started = time.monotonic()
            with self.assertRaises(CtxAgentHistoryProtocolError) as raised:
                adapter._run(
                    [
                        "-c",
                        "import subprocess,sys; child=subprocess.Popen([sys.executable,'-c','import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(60)'],stdout=sys.stdout,stderr=sys.stderr); open(sys.argv[1],'w').write(str(child.pid))",
                        str(pid_path),
                    ]
                )
            self.assertEqual(raised.exception.details["stream"], "pipe")
            self.assertLess(time.monotonic() - started, 2)
            child_pid = int(pid_path.read_text(encoding="utf-8"))
            self._assert_process_exited(child_pid)

    @unittest.skipIf(os.name == "nt", "POSIX detached-daemon lifecycle test")
    def test_local_cli_success_does_not_kill_detached_child(self) -> None:
        adapter = LocalCliAdapter(LocalConfig(ctx_binary=sys.executable, timeout=2))
        with tempfile.TemporaryDirectory() as directory:
            pid_path = Path(directory) / "child.pid"
            completed = adapter._run(
                [
                    "-c",
                    "import subprocess,sys; child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True); open(sys.argv[1],'w').write(str(child.pid)); print('{}')",
                    str(pid_path),
                ]
            )
            self.assertEqual(completed.stdout.strip(), "{}")
            child_pid = int(pid_path.read_text(encoding="utf-8"))
            os.kill(child_pid, 0)
            os.kill(child_pid, 9)
            self._assert_process_exited(child_pid)

    def _assert_process_exited(self, pid: int) -> None:
        deadline = time.monotonic() + 1
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.01)
        self.fail(f"owned process {pid} survived bounded teardown")

    def test_hosted_config_is_placeholder(self) -> None:
        client = AgentHistoryClient.hosted(HostedConfig(base_url="https://example.invalid"))

        with self.assertRaises(HostedTransportNotImplementedError) as raised:
            client.status()

        self.assertEqual(raised.exception.code, "not_supported")
        self.assertEqual(raised.exception.details["method"], "status")
        self.assertEqual(raised.exception.details["backend"], "hosted")
        with self.assertRaises(HostedTransportNotImplementedError):
            client.search(SEARCH_QUERY)
        with self.assertRaises(CtxAgentHistoryValidationError):
            client.search(
                typing.cast(
                    typing.Any,
                    {"version": "ctx-search-v1", "must_not": [{"all": "negative"}]},
                )
            )
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
        self.assertEqual(snapshot.event_location["operation"], "locateEvent")
        self.assertEqual(snapshot.session_location["operation"], "locateSession")
        self.assertEqual(snapshot.search["search"]["results"][0]["resultScope"], "event")


class RecordingSearchAdapter(LocalCliAdapter):
    def __init__(self, raw: dict[str, object] | None = None) -> None:
        super().__init__()
        self.raw = raw or {
            "schema_version": 2,
            "query": SEARCH_QUERY,
            "query_execution": search_execution(),
            "results": [],
        }
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
        elif args[:2] == ["locate", "event"]:
            payload.update(
                {
                    "payload_type": "event_location",
                    "ctx_event_id": args[2],
                    "ctx_session_id": "session-1",
                    "provider": "codex",
                    "source": {"path": "/tmp/session.jsonl", "exists": True},
                }
            )
        elif args[:2] == ["locate", "session"]:
            payload.update(
                {
                    "payload_type": "session_location",
                    "ctx_session_id": args[2],
                    "provider": "codex",
                    "source": {"path": "/tmp/session.jsonl", "exists": True},
                }
            )
        elif command == "search":
            query = json.loads(args[args.index("--query-json") + 1])
            payload.update(
                {
                    "schema_version": 2,
                    "query": query,
                    "query_execution": {
                        "query_version": "ctx-search-v1",
                        "candidate_strategy": "bounded_fts",
                        "resolved": {
                            "query_bytes": 8192,
                            "clauses": 32,
                            "analyzed_tokens_per_clause": 32,
                            "candidates_per_positive_seed": 1024,
                            "candidate_rows": 16384,
                            "retained_candidate_ids": 8192,
                            "residual_rows": 8192,
                            "verification_bytes": 16777216,
                            "verification_lookup_bytes": 16384,
                            "hydrated_rows": 256,
                            "hydration_input_bytes": 8388608,
                            "hydration_input_bytes_per_event": 65536,
                            "snippet_input_bytes": 8388608,
                            "returned_text_bytes": 524288,
                            "serialized_response_bytes": 2097152,
                            "results": 3,
                            "elapsed_ms": 1000,
                        },
                        "consumed": {
                            "query_bytes": 96,
                            "clauses": 7,
                            "analyzed_tokens": 18,
                            "largest_analyzed_tokens_per_clause": 6,
                            "largest_positive_seed_candidates": 20,
                            "candidate_rows": 48,
                            "retained_candidate_ids": 31,
                            "residual_rows": 12,
                            "verification_bytes": 4096,
                            "largest_verification_lookup_bytes": 512,
                            "hydrated_rows": 3,
                            "hydration_input_bytes": 2048,
                            "largest_hydration_input_bytes": 800,
                            "snippet_input_bytes": 1200,
                            "returned_results": 1,
                            "returned_text_bytes": 128,
                            "serialized_response_bytes": 2048,
                            "elapsed_ms": 12,
                        },
                        "semantic": {
                            "attempted": True,
                            "required": True,
                            "readiness": "ready",
                            "effective_backend": "hybrid",
                            "requested_candidates": 20,
                            "eligible_candidates": 18,
                            "candidates_supplied": 20,
                            "candidates_consumed": 18,
                            "candidates_used": 4,
                            "coverage": {
                                "indexed_documents": 990,
                                "searchable_documents": 1000,
                            },
                            "completeness": "partial",
                            "incompleteness_reasons": ["semantic_coverage_incomplete"],
                            "positive_text_rule_version": "ctx-search-positive-text-v1",
                        },
                        "rrf_k": 60,
                        "per_branch_candidate_rows": 1024,
                        "requested_result_limit": 3,
                        "result_limit": 3,
                        "max_result_limit": 200,
                        "clauses_executed": 7,
                        "verification_dropped": 0,
                        "filter_verification_dropped": 0,
                        "candidate_budget_exhausted": False,
                        "timed_out": False,
                        "truncated": True,
                        "truncation_reasons": ["semantic_coverage_incomplete"],
                    },
                    "payload_type": "search_results",
                    "results": [
                        {
                            "result_type": "event",
                            "result_scope": "event",
                            "citations": [{"target_type": "event", "label": "codex event"}],
                        }
                    ],
                    "truncation": {
                        "truncated": True,
                        "reason": "semantic_coverage_incomplete",
                        "omitted_results": 1,
                    },
                    "freshness": {"mode": "off", "status": "skipped"},
                }
            )
        elif command == "sources":
            payload.update({"sources": []})
        elif command == "status":
            payload.update(
                {
                    "initialized": True,
                    "freshness": {"mode": "off", "status": "skipped"},
                    "future_field": "preserved",
                }
            )
        elif command == "setup":
            payload.update({"mode": "ready"})
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
        _assert_required_keys(test, value, {"query", "results"})
        for hit in value["results"]:
            _assert_required_keys(test, hit, {"resultScope"})
    elif operation == "showEvent":
        _assert_required_keys(test, value, {"events"})
    elif operation in {"locateEvent", "locateSession"}:
        _assert_required_keys(test, value, {"ctxSessionId", "provider", "source"})
    elif operation == "error":
        _assert_required_keys(test, value, {"code", "message", "retryable"})


def _assert_required_keys(test: unittest.TestCase, payload: object, keys: set[str]) -> None:
    test.assertIsInstance(payload, dict)
    if isinstance(payload, dict):
        missing = keys.difference(payload)
        test.assertFalse(missing, f"missing required keys: {sorted(missing)}")


def _assert_public_keys_are_camel_case(test: unittest.TestCase, payload: object) -> None:
    if isinstance(payload, dict):
        for key, value in payload.items():
            test.assertNotIn("_", str(key), f"non-canonical snake_case key: {key}")
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
    "locateEvent": "location",
    "locateSession": "location",
    "error": "error",
}


if __name__ == "__main__":
    unittest.main()
