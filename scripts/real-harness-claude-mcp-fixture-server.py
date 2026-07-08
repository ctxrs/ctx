#!/usr/bin/env python3
"""Fixture Anthropic Messages server for real Claude Code MCP E2E tests."""

from __future__ import annotations

import http.server
import json
import socketserver
import sys
import time
from pathlib import Path
from typing import Any
from urllib.parse import urlparse


class FixtureState:
    def __init__(self, port_path: Path, log_path: Path) -> None:
        self.port_path = port_path
        self.log_path = log_path
        self.message_requests = 0
        self.real_turns = 0


class Handler(http.server.BaseHTTPRequestHandler):
    state: FixtureState

    def do_HEAD(self) -> None:
        self.send_response(200)
        self.end_headers()

    def do_POST(self) -> None:
        parsed = urlparse(self.path)
        if parsed.path != "/v1/messages":
            self.send_error(404)
            return
        self.state.message_requests += 1
        payload = json.loads(
            self.rfile.read(int(self.headers.get("content-length") or 0))
        )
        is_real_turn = bool(payload.get("tools"))
        if is_real_turn:
            self.state.real_turns += 1
        self._append_log(payload, is_real_turn)
        if payload.get("stream"):
            self._send_stream(payload, is_real_turn)
        else:
            self._send_json(payload, is_real_turn)

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def _append_log(self, payload: dict[str, Any], is_real_turn: bool) -> None:
        tools = payload.get("tools") or []
        entry = {
            "request": self.state.message_requests,
            "real_turn": is_real_turn,
            "model": payload.get("model"),
            "stream": payload.get("stream"),
            "tool_names": [
                tool.get("name") for tool in tools if isinstance(tool, dict)
            ],
            "message_tail": payload.get("messages", [])[-5:]
            if isinstance(payload.get("messages"), list)
            else payload.get("messages"),
        }
        with self.state.log_path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry, separators=(",", ":")))
            handle.write("\n")

    def _send_stream(self, payload: dict[str, Any], is_real_turn: bool) -> None:
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        for name, data in self._events(payload, is_real_turn):
            self.wfile.write(f"event: {name}\n".encode())
            self.wfile.write(f"data: {json.dumps(data, separators=(',', ':'))}\n\n".encode())
            self.wfile.flush()

    def _send_json(self, payload: dict[str, Any], is_real_turn: bool) -> None:
        body = self._message(payload, is_real_turn)
        encoded = json.dumps(body, separators=(",", ":")).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def _events(
        self, payload: dict[str, Any], is_real_turn: bool
    ) -> list[tuple[str, dict[str, Any]]]:
        message = self._message(payload, is_real_turn)
        events: list[tuple[str, dict[str, Any]]] = [
            ("message_start", {"type": "message_start", "message": message}),
        ]
        for index, block in enumerate(message["content"]):
            events.append(
                (
                    "content_block_start",
                    {
                        "type": "content_block_start",
                        "index": index,
                        "content_block": block,
                    },
                )
            )
            if block["type"] == "text" and block.get("text"):
                events.append(
                    (
                        "content_block_delta",
                        {
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {"type": "text_delta", "text": block["text"]},
                        },
                    )
                )
            events.append(
                (
                    "content_block_stop",
                    {"type": "content_block_stop", "index": index},
                )
            )
        events.extend(
            [
                (
                    "message_delta",
                    {
                        "type": "message_delta",
                        "delta": {"stop_reason": message["stop_reason"]},
                        "usage": {"output_tokens": 1},
                    },
                ),
                ("message_stop", {"type": "message_stop"}),
            ]
        )
        return events

    def _message(self, payload: dict[str, Any], is_real_turn: bool) -> dict[str, Any]:
        model = payload.get("model") or "mock-model"
        if not is_real_turn:
            content = [{"type": "text", "text": "ctx harness"}]
            stop_reason = "end_turn"
        elif self.state.real_turns == 1:
            content = [
                {
                    "type": "tool_use",
                    "id": "toolu_ctx_sources",
                    "name": "mcp__ctx__sources",
                    "input": {},
                }
            ]
            stop_reason = "tool_use"
        else:
            content = [{"type": "text", "text": "CALLED_CTX_SOURCES"}]
            stop_reason = "end_turn"
        return {
            "id": f"msg_fixture_{int(time.time())}_{self.state.message_requests}",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": content,
            "stop_reason": stop_reason,
            "stop_sequence": None,
            "usage": {
                "input_tokens": 1,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "output_tokens": 1,
                "server_tool_use": None,
                "service_tier": "standard",
            },
        }


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: real-harness-claude-mcp-fixture-server.py PORT_FILE LOG_FILE", file=sys.stderr)
        return 2
    state = FixtureState(Path(sys.argv[1]), Path(sys.argv[2]))
    Handler.state = state
    with socketserver.TCPServer(("127.0.0.1", 0), Handler) as httpd:
        state.port_path.write_text(str(httpd.server_address[1]), encoding="utf-8")
        while state.real_turns < 2:
            httpd.handle_request()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
