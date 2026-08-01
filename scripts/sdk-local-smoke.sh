#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

cargo build -q -p ctx

tmp_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-sdk-smoke.XXXXXX")"
trap 'rm -rf "$tmp_root"' EXIT

export CTX_BIN="$repo_root/target/debug/ctx"
export CTX_DATA_ROOT="$tmp_root/data"
export PYTHONPATH="$repo_root/sdks/python/src${PYTHONPATH:+:$PYTHONPATH}"
export REPO_ROOT="$repo_root"

python3 - <<'PY'
import os
from pathlib import Path

from ctx_agent_history import AgentHistoryClient

repo_root = Path(os.environ["REPO_ROOT"])
fixture = repo_root / "tests" / "fixtures" / "provider-history" / "codex-sessions"

client = AgentHistoryClient.local(
    ctx_binary=os.environ["CTX_BIN"],
    data_root=os.environ["CTX_DATA_ROOT"],
    env={"CTX_ANALYTICS_ENABLED": "false"},
    timeout=30,
)

assert client.init(catalog_only=True, progress="none")["operation"] == "init"
assert client.sources()["operation"] == "sources"
assert client.import_(provider="codex", path=str(fixture), progress="none")["operation"] == "import"

search = client.search("onboarding", limit=5, refresh="off")
hits = search["search"]["results"]
assert hits, "expected at least one search result from sanitized Codex fixture"

first = hits[0]
event_id = first.get("ctxEventId")
session_id = first.get("ctxSessionId")
assert event_id, "search result must include ctxEventId"
assert session_id, "search result must include ctxSessionId"

assert client.show_event(event_id, window=1)["operation"] == "showEvent"
assert client.show_session(session_id, mode="lite")["operation"] == "showSession"

print("sdk local smoke passed")
PY
