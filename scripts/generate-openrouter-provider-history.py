#!/usr/bin/env python3
"""Generate temporary provider histories for credential-gated live E2E.

This script is a test harness. It may use OpenRouter credentials to generate
synthetic assistant text, but the generated histories are then imported by ctx
as ordinary local files. Do not call this from product setup/import/search.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
from pathlib import Path
import sys
import urllib.error
import urllib.request


PROVIDER_ALIASES = {
    "claude_code": "claude",
    "claude-code": "claude",
    "open_code": "opencode",
    "open-code": "opencode",
    "antigravity_cli": "antigravity",
    "antigravity-cli": "antigravity",
    "gemini_cli": "gemini",
    "gemini-cli": "gemini",
}

DEFAULT_FREE_MODEL = "meta-llama/llama-3.1-8b-instruct:free"


def env_first(*names: str) -> str | None:
    for name in names:
        value = os.environ.get(name)
        if value:
            return value
    return None


def provider_env_name(provider: str) -> str:
    return provider.upper().replace("-", "_").replace("_CLI", "")


def model_for(provider: str, explicit: str | None) -> str:
    if explicit:
        return explicit
    provider_key = provider_env_name(provider)
    model = env_first(
        f"CTX_E2E_{provider_key}_OPENROUTER_MODEL_OVERRIDE",
        "CTX_LIVE_PROVIDER_OPENROUTER_MODEL",
        "CTX_E2E_OPENROUTER_MODEL_OVERRIDE",
        "CTX_RELEASE_E2E_OPENROUTER_MODEL",
        "CTX_RELEASE_PREFLIGHT_OPENROUTER_MODEL",
    )
    if model:
        return model
    if os.environ.get("CTX_LIVE_PROVIDER_OPENROUTER_ALLOW_DEFAULT_FREE_MODEL") == "1":
        return os.environ.get(
            "CTX_LIVE_PROVIDER_OPENROUTER_DEFAULT_FREE_MODEL", DEFAULT_FREE_MODEL
        )
    raise SystemExit(
        "OpenRouter model env is required; set CTX_LIVE_PROVIDER_OPENROUTER_MODEL "
        "or CTX_E2E_OPENROUTER_MODEL_OVERRIDE"
    )


def openrouter_completion(provider: str, query: str, model: str) -> str:
    api_key = env_first("OPENROUTER_API_KEY", "CTX_OPENROUTER_API_KEY")
    if not api_key:
        raise SystemExit("OpenRouter credential env is required")
    base_url = env_first("OPENROUTER_BASE_URL", "CTX_OPENROUTER_BASE_URL")
    if not base_url:
        base_url = "https://openrouter.ai/api/v1"
    url = base_url.rstrip("/") + "/chat/completions"
    prompt = (
        "Write one short, non-sensitive assistant response for a ctx live E2E "
        f"synthetic {provider} history. Include this marker exactly once: {query}. "
        "Do not include credentials, personal data, URLs with tokens, or local paths."
    )
    body = {
        "model": model,
        "messages": [
            {
                "role": "system",
                "content": "You generate synthetic release smoke test text only.",
            },
            {"role": "user", "content": prompt},
        ],
        "temperature": 0.2,
        "max_tokens": 160,
    }
    request = urllib.request.Request(
        url,
        data=json.dumps(body).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
            "HTTP-Referer": "https://github.com/ctxrs/ctx",
            "X-Title": "search live e2e",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            payload = json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as err:
        raise SystemExit(f"OpenRouter request failed with HTTP {err.code}") from err
    except urllib.error.URLError as err:
        raise SystemExit(f"OpenRouter request failed: {err.reason}") from err

    choices = payload.get("choices") or []
    if not choices:
        raise SystemExit("OpenRouter response did not include choices")
    content = (
        choices[0]
        .get("message", {})
        .get("content", "")
        .replace("\r", " ")
        .replace("\n", " ")
        .strip()
    )
    if not content:
        raise SystemExit("OpenRouter response content was empty")
    return content[:1200]


def write_jsonl(path: Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        for row in rows:
            json.dump(row, handle, sort_keys=True, separators=(",", ":"))
            handle.write("\n")


def at(offset_seconds: int) -> str:
    base = dt.datetime(2026, 6, 24, 12, 0, 0, tzinfo=dt.timezone.utc)
    return (base + dt.timedelta(seconds=offset_seconds)).isoformat().replace("+00:00", "Z")


def normalized_rows(provider: str, query: str, completion: str) -> list[dict]:
    primary = f"{provider}-openrouter-primary"
    worker = f"{provider}-openrouter-worker"
    followup = f"{provider}-openrouter-followup"
    session_base = {
        "agent_type": "primary",
        "role_hint": "primary",
        "is_primary": True,
        "status": "imported",
        "cwd": "/workspace/openrouter-provider-e2e",
        "metadata": {
            "source": "openrouter-live-e2e",
            "synthetic": True,
            "raw_retention": "path_reference",
        },
    }
    return [
        {
            "provider": provider,
            "session": {
                **session_base,
                "provider_session_id": primary,
                "started_at": at(0),
            },
            "event": {
                "provider_event_index": 0,
                "cursor": f"{provider}-openrouter-primary-0",
                "event_type": "message",
                "role": "user",
                "occurred_at": at(1),
                "payload": {"text": f"{query} primary requests provider release smoke."},
                "metadata": {"source": "openrouter-live-e2e"},
            },
        },
        {
            "provider": provider,
            "session": {
                **session_base,
                "provider_session_id": worker,
                "parent_provider_session_id": primary,
                "root_provider_session_id": primary,
                "external_agent_id": f"{provider}-worker",
                "agent_type": "subagent",
                "role_hint": "worker",
                "is_primary": False,
                "started_at": at(2),
            },
            "event": {
                "provider_event_index": 0,
                "cursor": f"{provider}-openrouter-worker-0",
                "event_type": "summary",
                "role": "assistant",
                "occurred_at": at(3),
                "payload": {"text": f"{query} worker generated response: {completion}"},
                "metadata": {"source": "openrouter-live-e2e"},
            },
        },
        {
            "provider": provider,
            "session": {
                **session_base,
                "provider_session_id": followup,
                "started_at": at(60),
            },
            "event": {
                "provider_event_index": 0,
                "cursor": f"{provider}-openrouter-followup-0",
                "event_type": "message",
                "role": "assistant",
                "occurred_at": at(61),
                "payload": {"text": f"{query} followup validates release smoke context."},
                "metadata": {"source": "openrouter-live-e2e"},
            },
        },
    ]


def write_codex_history(output: Path, query: str, completion: str) -> tuple[Path, int, int]:
    root = output
    session_dir = root / "2026" / "06" / "24"
    primary = "codex-openrouter-primary"
    worker = "codex-openrouter-worker"
    followup = "codex-openrouter-followup"
    write_jsonl(
        session_dir / "primary.jsonl",
        [
            {
                "timestamp": at(0),
                "type": "session_meta",
                "payload": {
                    "id": primary,
                    "timestamp": at(0),
                    "cwd": "/workspace/openrouter-provider-e2e",
                    "originator": "codex-live-e2e",
                    "cli_version": "synthetic",
                    "source": "openrouter-live-e2e",
                    "model_provider": "openrouter",
                },
            },
            {
                "timestamp": at(1),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": f"{query} primary asks."}],
                },
            },
            {
                "timestamp": at(2),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": f"{query} generated response: {completion}",
                        }
                    ],
                },
            },
        ],
    )
    write_jsonl(
        session_dir / "worker.jsonl",
        [
            {
                "timestamp": at(3),
                "type": "session_meta",
                "payload": {
                    "id": worker,
                    "timestamp": at(3),
                    "cwd": "/workspace/openrouter-provider-e2e",
                    "originator": "codex-live-e2e",
                    "cli_version": "synthetic",
                    "source": {
                        "subagent": {
                            "thread_spawn": {
                                "parent_thread_id": primary,
                                "depth": 1,
                                "agent_nickname": "OpenRouterWorker",
                                "agent_role": "worker",
                            }
                        }
                    },
                    "agent_nickname": "OpenRouterWorker",
                    "agent_role": "worker",
                    "model_provider": "openrouter",
                },
            },
            {
                "timestamp": at(4),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": f"{query} worker confirms."}],
                },
            },
        ],
    )
    write_jsonl(
        session_dir / "followup.jsonl",
        [
            {
                "timestamp": at(60),
                "type": "session_meta",
                "payload": {
                    "id": followup,
                    "timestamp": at(60),
                    "cwd": "/workspace/openrouter-provider-e2e",
                    "originator": "codex-live-e2e",
                    "cli_version": "synthetic",
                    "source": "openrouter-live-e2e",
                    "model_provider": "openrouter",
                },
            },
            {
                "timestamp": at(61),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": f"{query} followup checks context."}],
                },
            },
        ],
    )
    return root, 3, 4


def write_pi_history(output: Path, query: str, completion: str) -> tuple[Path, int, int]:
    rows = [
        {
            "type": "session",
            "version": 3,
            "id": "pi-openrouter-primary",
            "timestamp": at(0),
            "cwd": "/workspace/openrouter-provider-e2e",
        },
        {
            "type": "message",
            "id": "pi-primary-user-0",
            "parentId": None,
            "timestamp": at(1),
            "message": {"role": "user", "content": f"{query} primary asks."},
        },
        {
            "type": "message",
            "id": "pi-primary-assistant-0",
            "parentId": "pi-primary-user-0",
            "timestamp": at(2),
            "message": {
                "role": "assistant",
                "content": f"{query} generated response: {completion}",
                "provider": "openrouter",
                "model": "openrouter-live-e2e",
            },
        },
        {
            "type": "session",
            "version": 3,
            "id": "pi-openrouter-worker",
            "timestamp": at(3),
            "cwd": "/workspace/openrouter-provider-e2e",
        },
        {
            "type": "message",
            "id": "pi-worker-assistant-0",
            "parentId": None,
            "timestamp": at(4),
            "message": {"role": "assistant", "content": f"{query} worker confirms."},
        },
        {
            "type": "session",
            "version": 3,
            "id": "pi-openrouter-followup",
            "timestamp": at(60),
            "cwd": "/workspace/openrouter-provider-e2e",
        },
        {
            "type": "message",
            "id": "pi-followup-user-0",
            "parentId": None,
            "timestamp": at(61),
            "message": {"role": "user", "content": f"{query} followup checks context."},
        },
    ]
    write_jsonl(output, rows)
    return output, 3, 4


def write_provider_history(provider: str, output: Path, query: str, completion: str) -> tuple[Path, int, int, str]:
    if provider == "codex":
        path, sessions, events = write_codex_history(output, query, completion)
        return path, sessions, events, "codex_session_jsonl_tree"
    if provider == "pi":
        path, sessions, events = write_pi_history(output, query, completion)
        return path, sessions, events, "pi_session_jsonl"
    rows = normalized_rows(provider, query, completion)
    write_jsonl(output, rows)
    return output, 3, 3, "normalized_provider_jsonl"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--provider", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--query", required=True)
    parser.add_argument("--model")
    args = parser.parse_args()

    provider = PROVIDER_ALIASES.get(args.provider, args.provider)
    if provider not in {"codex", "pi", "claude", "opencode", "antigravity", "gemini", "cursor"}:
        raise SystemExit(f"unsupported generated provider: {args.provider}")

    model = model_for(provider, args.model)
    completion = openrouter_completion(provider, args.query, model)
    source_path, sessions, events, source_format = write_provider_history(
        provider, Path(args.output), args.query, completion
    )
    json.dump(
        {
            "schema_version": 1,
            "provider": provider,
            "source_format": source_format,
            "sessions": sessions,
            "events": events,
            "model": model,
            "output_path": str(source_path),
        },
        sys.stdout,
        sort_keys=True,
    )
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
