#!/usr/bin/env python3
"""Fixture OpenAI-compatible chat server for real Qwen MCP E2E tests."""

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
        if self.path.endswith("/chat/completions") and payload.get("stream"):
            self._send_stream(payload)
            return
        if self.path.endswith("/chat/completions"):
            self._send_json(payload)
            return
        self.send_error(404)

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

    def _send_stream(self, payload: dict[str, Any]) -> None:
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

    def _send_json(self, payload: dict[str, Any]) -> None:
        body = self._json_completion(payload)
        encoded = json.dumps(body, separators=(",", ":")).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def _stream_chunks(self, payload: dict[str, Any]) -> list[dict[str, Any] | str]:
        model = payload.get("model") or "test-model"
        if self.state.request_count == 1:
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
                                    "name": "mcp__ctx__status",
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
            chat_chunk(model, {"content": "fixture-qwen-mcp-ok"}, None),
            chat_chunk(model, {}, "stop"),
            "[DONE]",
        ]

    def _json_completion(self, payload: dict[str, Any]) -> dict[str, Any]:
        model = payload.get("model") or "test-model"
        if self.state.request_count == 1:
            message = {
                "role": "assistant",
                "content": None,
                "tool_calls": [
                    {
                        "id": "call_ctx_status",
                        "type": "function",
                        "function": {
                            "name": "mcp__ctx__status",
                            "arguments": "{}",
                        },
                    }
                ],
            }
            finish_reason = "tool_calls"
        else:
            message = {"role": "assistant", "content": "fixture-qwen-mcp-ok"}
            finish_reason = "stop"
        return {
            "id": f"chatcmpl-{self.state.request_count}",
            "object": "chat.completion",
            "created": int(time.time()),
            "model": model,
            "choices": [
                {
                    "index": 0,
                    "message": message,
                    "finish_reason": finish_reason,
                }
            ],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2,
            },
        }


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
        print("usage: real-harness-qwen-mcp-fixture-server.py PORT_FILE LOG_FILE", file=sys.stderr)
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
