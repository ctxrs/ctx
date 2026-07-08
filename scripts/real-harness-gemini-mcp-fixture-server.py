#!/usr/bin/env python3
"""Fixture Google GenAI SSE server for real Gemini CLI MCP E2E tests."""

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
        for data in self._events():
            self.wfile.write(
                f"data: {json.dumps(data, separators=(',', ':'))}\n\n".encode()
            )
            self.wfile.flush()

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def _append_log(self, payload: dict[str, Any]) -> None:
        tools = payload.get("tools") or []
        function_names: list[str] = []
        for tool in tools:
            for declaration in tool.get("functionDeclarations", []):
                name = declaration.get("name")
                if name:
                    function_names.append(name)
        entry = {
            "request": self.state.request_count,
            "path": self.path,
            "tool_names": function_names,
            "contents_tail": payload.get("contents", [])[-5:]
            if isinstance(payload.get("contents"), list)
            else payload.get("contents"),
        }
        with self.state.log_path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry, separators=(",", ":")))
            handle.write("\n")

    def _events(self) -> list[dict[str, Any]]:
        if self.state.request_count == 1:
            return [
                {
                    "candidates": [
                        {
                            "content": {
                                "role": "model",
                                "parts": [
                                    {
                                        "functionCall": {
                                            "id": "call_ctx_status",
                                            "name": "mcp_ctx_status",
                                            "args": {},
                                        }
                                    }
                                ],
                            },
                            "finishReason": "STOP",
                            "index": 0,
                        }
                    ],
                    "usageMetadata": usage(),
                }
            ]
        return [
            {
                "candidates": [
                    {
                        "content": {
                            "role": "model",
                            "parts": [{"text": "fixture-gemini-mcp-ok"}],
                        },
                        "finishReason": "STOP",
                        "index": 0,
                    }
                ],
                "usageMetadata": usage(),
            }
        ]


def usage() -> dict[str, int]:
    return {
        "promptTokenCount": 1,
        "candidatesTokenCount": 1,
        "totalTokenCount": 2,
    }


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: real-harness-gemini-mcp-fixture-server.py PORT_FILE LOG_FILE", file=sys.stderr)
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
