#!/usr/bin/env python3
"""Fixture OpenAI-compatible chat server for real OpenCode MCP E2E tests."""

from __future__ import annotations

import http.server
import json
import socketserver
import sys
import time
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
        for data in self._stream_chunks(payload):
            if data == "[DONE]":
                self.wfile.write(b"data: [DONE]\n\n")
            else:
                self.wfile.write(
                    f"data: {json.dumps(data, separators=(',', ':'))}\n\n".encode()
                )
            self.wfile.flush()

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def _append_log(self, payload: dict[str, Any]) -> None:
        tools = payload.get("tools") or []
        messages = payload.get("messages") or []
        entry = {
            "request": self.state.request_count,
            "path": self.path,
            "model": payload.get("model"),
            "stream": payload.get("stream"),
            "tool_names": [
                tool.get("function", {}).get("name") or tool.get("name")
                for tool in tools
                if isinstance(tool, dict)
            ],
            "message_tail": messages[-5:] if isinstance(messages, list) else messages,
        }
        with self.state.log_path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry, separators=(",", ":")))
            handle.write("\n")

    def _stream_chunks(self, payload: dict[str, Any]) -> list[dict[str, Any] | str]:
        model = payload.get("model") or "test-model"
        if self.state.request_count == 1:
            return [
                chat_chunk(model, {"content": "ctx MCP status"}, None),
                chat_chunk(model, {}, "stop"),
                "[DONE]",
            ]
        if self.state.request_count == 2:
            return [
                chat_chunk(
                    model,
                    {
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": "call_ctx_status",
                                "type": "function",
                                "function": {
                                    "name": "ctx_status",
                                    "arguments": "{}",
                                },
                            }
                        ]
                    },
                    None,
                ),
                chat_chunk(model, {}, "tool_calls"),
                "[DONE]",
            ]
        return [
            chat_chunk(model, {"content": "fixture-opencode-mcp-ok"}, None),
            chat_chunk(model, {}, "stop"),
            "[DONE]",
        ]


def chat_chunk(
    model: str, delta: dict[str, Any], finish_reason: str | None
) -> dict[str, Any]:
    return {
        "id": "chatcmpl-fixture",
        "object": "chat.completion.chunk",
        "created": int(time.time()),
        "model": model,
        "choices": [
            {
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason,
            }
        ],
    }


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: real-harness-opencode-mcp-fixture-server.py PORT_FILE LOG_FILE", file=sys.stderr)
        return 2
    state = FixtureState(Path(sys.argv[1]), Path(sys.argv[2]))
    Handler.state = state
    with socketserver.TCPServer(("127.0.0.1", 0), Handler) as httpd:
        state.port_path.write_text(str(httpd.server_address[1]), encoding="utf-8")
        while state.request_count < 3:
            httpd.handle_request()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
