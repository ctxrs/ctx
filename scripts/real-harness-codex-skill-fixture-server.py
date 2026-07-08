#!/usr/bin/env python3
"""Fixture OpenAI Responses server for real Codex skill E2E tests."""

from __future__ import annotations

import http.server
import json
import socketserver
import sys
from pathlib import Path
from typing import Any


class FixtureState:
    def __init__(self, port_path: Path, log_path: Path) -> None:
        self.port_path = port_path
        self.log_path = log_path
        self.request_count = 0


class Handler(http.server.BaseHTTPRequestHandler):
    state: FixtureState

    def do_POST(self) -> None:
        self.state.request_count += 1
        payload = json.loads(
            self.rfile.read(int(self.headers.get("content-length") or 0))
        )
        self._append_log(payload)
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        for name, data in self._events(payload):
            self.wfile.write(f"event: {name}\n".encode())
            self.wfile.write(f"data: {json.dumps(data, separators=(',', ':'))}\n\n".encode())
            self.wfile.flush()

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def _append_log(self, payload: dict[str, Any]) -> None:
        text = json.dumps(payload, separators=(",", ":"))
        tools = payload.get("tools") or []
        entry = {
            "request": self.state.request_count,
            "model": payload.get("model"),
            "has_ctx_skill": "ctx-agent-history-search" in text,
            "has_ctx_skill_description": "Use ctx to search local coding-agent history"
            in text,
            "has_ctx_skill_path": "/skills/ctx-agent-history-search/SKILL.md" in text,
            "tool_names": [
                tool.get("name") or tool.get("type")
                for tool in tools
                if isinstance(tool, dict)
            ],
            "input_tail": payload.get("input", [])[-5:]
            if isinstance(payload.get("input"), list)
            else payload.get("input"),
        }
        with self.state.log_path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry, separators=(",", ":")))
            handle.write("\n")

    def _events(self, payload: dict[str, Any]) -> list[tuple[str, dict[str, Any]]]:
        model = payload.get("model")
        if self.state.request_count == 1:
            item = {
                "type": "function_call",
                "call_id": "call_ctx_version",
                "name": "exec_command",
                "arguments": json.dumps(
                    {"cmd": "ctx --version", "yield_time_ms": 1000},
                    separators=(",", ":"),
                ),
            }
            return response_with_item("resp_exec_ctx_version", model, item)
        message = {
            "id": "msg_done",
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "fixture-ctx-skill-ok"}],
        }
        return response_with_item("resp_done", model, message)


def response_with_item(
    response_id: str, model: str | None, item: dict[str, Any]
) -> list[tuple[str, dict[str, Any]]]:
    return [
        (
            "response.output_item.done",
            {"type": "response.output_item.done", "output_index": 0, "item": item},
        ),
        (
            "response.completed",
            {
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "status": "completed",
                    "model": model,
                    "output": [item],
                    "usage": {
                        "input_tokens": 1,
                        "output_tokens": 1,
                        "total_tokens": 2,
                    },
                },
            },
        ),
    ]


def main() -> int:
    if len(sys.argv) != 3:
        print(
            "usage: real-harness-codex-skill-fixture-server.py PORT_FILE LOG_FILE",
            file=sys.stderr,
        )
        return 2
    state = FixtureState(Path(sys.argv[1]), Path(sys.argv[2]))
    Handler.state = state
    with socketserver.TCPServer(("127.0.0.1", 0), Handler) as httpd:
        state.port_path.write_text(str(httpd.server_address[1]), encoding="utf-8")
        while state.request_count < 2:
            httpd.handle_request()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
