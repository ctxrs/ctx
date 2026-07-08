#!/usr/bin/env python3
"""Local model fixture server for real slash-command CLI harnesses."""

from __future__ import annotations

import http.server
import json
import os
import socketserver
import sys
from pathlib import Path
from typing import Any


class FixtureState:
    def __init__(self, provider: str, port_path: Path, log_path: Path) -> None:
        self.provider = provider
        self.port_path = port_path
        self.log_path = log_path
        self.request_count = 0
        self.expected_query = os.environ.get(
            "CTX_SLASH_EXPECTED_QUERY", "needle topic with spaces"
        )


class Handler(http.server.BaseHTTPRequestHandler):
    state: FixtureState

    def do_POST(self) -> None:
        self.state.request_count += 1
        payload = json.loads(
            self.rfile.read(int(self.headers.get("content-length") or 0))
        )
        self._append_log(payload)
        if self.state.provider == "gemini":
            self._send_gemini_stream()
        elif self.state.provider == "qwen":
            self._send_openai_response(payload)
        else:
            self.send_error(500, f"unknown provider: {self.state.provider}")

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def _append_log(self, payload: dict[str, Any]) -> None:
        text = json.dumps(payload, separators=(",", ":"), ensure_ascii=False)
        expected_user_request = f"User request: {self.state.expected_query}"
        entry = {
            "provider": self.state.provider,
            "request": self.state.request_count,
            "path": self.path,
            "model": payload.get("model"),
            "stream": payload.get("stream"),
            "has_ctx_history_expansion": "# ctx History" in text
            and "Use ctx to search local coding-agent history" in text,
            "has_expected_user_request": expected_user_request in text,
            "has_ctx_citations_instruction": "ctx citations" in text,
            "has_raw_slash_invocation": f"/ctx-history {self.state.expected_query}"
            in text,
        }
        with self.state.log_path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry, separators=(",", ":")))
            handle.write("\n")

    def _send_gemini_stream(self) -> None:
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        data = {
            "candidates": [
                {
                    "content": {
                        "parts": [{"text": "fixture-gemini-slash-ok"}],
                        "role": "model",
                    },
                    "finishReason": "STOP",
                    "index": 0,
                }
            ],
            "usageMetadata": {
                "promptTokenCount": 1,
                "candidatesTokenCount": 1,
                "totalTokenCount": 2,
            },
        }
        self._write_sse_data(data)

    def _send_openai_response(self, payload: dict[str, Any]) -> None:
        if payload.get("stream"):
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.end_headers()
            for chunk in [
                {
                    "id": "chatcmpl-ctx-slash-fixture",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": payload.get("model"),
                    "choices": [
                        {
                            "index": 0,
                            "delta": {
                                "role": "assistant",
                                "content": "fixture-qwen-slash-ok",
                            },
                            "finish_reason": None,
                        }
                    ],
                },
                {
                    "id": "chatcmpl-ctx-slash-fixture",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": payload.get("model"),
                    "choices": [
                        {"index": 0, "delta": {}, "finish_reason": "stop"}
                    ],
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1,
                        "total_tokens": 2,
                    },
                },
            ]:
                self._write_sse_data(chunk)
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
            return

        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.end_headers()
        response = {
            "id": "chatcmpl-ctx-slash-fixture",
            "object": "chat.completion",
            "created": 0,
            "model": payload.get("model"),
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "fixture-qwen-slash-ok",
                    },
                    "finish_reason": "stop",
                }
            ],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2,
            },
        }
        self.wfile.write(json.dumps(response, separators=(",", ":")).encode())

    def _write_sse_data(self, data: dict[str, Any]) -> None:
        self.wfile.write(
            f"data: {json.dumps(data, separators=(',', ':'))}\n\n".encode()
        )
        self.wfile.flush()


class ReusableTCPServer(socketserver.TCPServer):
    allow_reuse_address = True


def main() -> int:
    if len(sys.argv) != 4:
        print(
            "usage: real-harness-slash-fixture-server.py PROVIDER PORT_FILE LOG_FILE",
            file=sys.stderr,
        )
        return 2
    provider = sys.argv[1]
    if provider not in {"gemini", "qwen"}:
        print(f"unsupported provider: {provider}", file=sys.stderr)
        return 2
    state = FixtureState(provider, Path(sys.argv[2]), Path(sys.argv[3]))
    Handler.state = state
    with ReusableTCPServer(("127.0.0.1", 0), Handler) as httpd:
        state.port_path.write_text(str(httpd.server_address[1]), encoding="utf-8")
        httpd.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
