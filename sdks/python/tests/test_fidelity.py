import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from ctx_agent_history import AgentHistoryClient
from ctx_agent_history.errors import CtxAgentHistoryCliError

FIXTURES = Path(__file__).resolve().parents[3] / "contracts/agent-history-v1/fixtures/cli"


class FidelityTests(unittest.TestCase):
    def test_current_event_and_session_payloads(self):
        event = json.loads((FIXTURES / "opaque-event.json").read_text())
        for value in [event["structured_content"], None, "absent"]:
            current = dict(event)
            if value == "absent":
                current.pop("structured_content")
            else:
                current["structured_content"] = value
            raw = {"event": current, "events": [current], "session": {"ctx_session_id": "session-1"}}
            result = subprocess.CompletedProcess([], 0, stdout=json.dumps(raw), stderr="")
            with mock.patch("ctx_agent_history.transport.run_local_cli", return_value=result):
                client = AgentHistoryClient.local()
                for actual in [client.show_event("event-1")["event"]["event"], client.show_session("session-1")["session"]["events"][0]]:
                    self.assertEqual(actual["activity"], event["activity"])
                    self.assertEqual("structuredContent" in actual, value != "absent")
                    if value != "absent":
                        self.assertEqual(actual["structuredContent"], value)
                    self.assertEqual(actual["content"]["policyStatus"], "selected")

    def test_literal_query_argv(self):
        result = subprocess.CompletedProcess([], 0, stdout='{"results":[]}', stderr="")
        for query in ["--help", "--refresh=off", "-needle", "two words", "a'雪"]:
            with mock.patch("ctx_agent_history.transport.run_local_cli", return_value=result) as run:
                AgentHistoryClient.local().search(query, refresh="off", terms=["--help"])
                args = run.call_args.args[0]
                self.assertEqual(args[-2:], ["--", query])
                self.assertIn("--term=--help", args)

    def test_producer_errors(self):
        for producer in json.loads((FIXTURES / "producer-errors.json").read_text()):
            result = subprocess.CompletedProcess([], 1, stdout="", stderr=json.dumps(producer))
            with mock.patch("ctx_agent_history.transport.run_local_cli", return_value=result):
                with self.assertRaises(CtxAgentHistoryCliError) as caught:
                    AgentHistoryClient.local().show_event("event-1")
            self.assertEqual(caught.exception.retryable, producer["retryable"])
            self.assertEqual(caught.exception.details["producerError"], producer)
        result = subprocess.CompletedProcess([], 1, stdout="", stderr="not JSON")
        with mock.patch("ctx_agent_history.transport.run_local_cli", return_value=result):
            with self.assertRaises(CtxAgentHistoryCliError) as caught:
                AgentHistoryClient.local().show_event("event-1")
        self.assertFalse(caught.exception.retryable)
        self.assertEqual(caught.exception.stderr, "not JSON")
