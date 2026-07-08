from __future__ import annotations

import json
import os
import stat
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
)
from ctx_agent_history.errors import CtxAgentHistoryCliError, CtxAgentHistoryProtocolError
from ctx_agent_history.errors import CtxAgentHistoryTimeoutError, CtxAgentHistoryValidationError
from ctx_agent_history.types import AgentHistoryErrorCode
import dogfood_local


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
            self.assertEqual(client.locate_event("event-1")["operation"], "locateEvent")
            self.assertEqual(client.locateEvent("event-1")["operation"], "locateEvent")
            self.assertEqual(client.locate_session("session-1")["operation"], "locateSession")
            self.assertEqual(client.locateSession("session-1")["operation"], "locateSession")

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
                    "item_type": "event_window",
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
                    "item_type": "session_transcript",
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
                    "item_type": "event_location",
                    "ctx_event_id": args[2],
                    "ctx_session_id": "session-1",
                    "provider": "codex",
                    "source": {"path": "/tmp/session.jsonl", "exists": True},
                }
            )
        elif args[:2] == ["locate", "session"]:
            payload.update(
                {
                    "item_type": "session_location",
                    "ctx_session_id": args[2],
                    "provider": "codex",
                    "source": {"path": "/tmp/session.jsonl", "exists": True},
                }
            )
        elif command == "search":
            payload.update(
                {
                    "query": "sqlite",
                    "results": [{"result_scope": "event"}],
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
