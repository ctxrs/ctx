"""Authored semantic and native-format fixtures; no copied provider history."""

import json


def fixture(path, source, texts, *, session="same-native-session", cwd=None, append=False):
    """Authored JSONL-v2 semantics, not a native provider capture."""
    records = [
        {"record_type": "manifest", "schema_version": "ctx-history-jsonl-v2"},
        {"record_type": "source", "source_id": source,
         "provider_key": "acceptance-agent", "source_format": "authored-jsonl"},
        {"record_type": "session", "source_id": source,
         "provider_session_id": session, "started_at": "2026-01-01T00:00:00Z"},
    ]
    if cwd is not None:
        records[2]["cwd"] = cwd
    for index, text in enumerate(texts):
        records.append({
            "record_type": "event", "source_id": source,
            "provider_session_id": session, "event_id": f"event-{index}",
            "event_index": index, "occurred_at": "2026-01-01T00:00:01Z",
            "event_type": "message", "role": "user", "payload": {"text": text},
        })
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a" if append else "w", encoding="utf-8") as stream:
        for row in records[2:] if append else records:
            stream.write(json.dumps(row, ensure_ascii=False) + "\n")


def codex_fixture(home, number, text, *, rootless=False):
    session = f"019faaaa-0000-7000-8000-{number:012d}"
    metadata = {"id": session, "timestamp": "2026-01-01T00:00:00Z",
                "originator": "codex_cli_rs", "cli_version": "0.1.0",
                "source": "cli", "model_provider": "openai"}
    if not rootless:
        metadata["cwd"] = "/synthetic/acceptance-work"
    rows = [
        {"timestamp": "2026-01-01T00:00:00Z", "type": "session_meta", "payload": metadata},
        {"timestamp": "2026-01-01T00:00:01Z", "type": "response_item", "payload": {
            "type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}},
    ]
    sessions = home / "sessions"
    sessions.mkdir(parents=True, exist_ok=True)
    path = sessions / f"rollout-2026-01-01T00-00-00-{session}.jsonl"
    path.write_text("".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows), encoding="utf-8")


def prepare_ongoing(h, sources):
    profiles = [sources / "codex-allowed", sources / "codex-excluded"]
    cases = [("sparrowseed", "sparrowseed initial allowed history."),
             ("otterseed", "otterseed previously authorized history.")]
    for number, (home, (_, body)) in enumerate(zip(profiles, cases), 1):
        codex_fixture(home, number, body)
        h.cli("alice", "sources", "add", home.name, "--provider", "codex",
              "--root", home, "--format=json")
    # A full refresh commits both new profile definitions on an existing index.
    h.import_file("alice", all_sources=True)
    identities = []
    for marker, body in cases:
        h.local_event("alice", marker, body)
        identities.append(h.source_id("alice", marker))
    if len(set(identities)) != 2:
        raise AssertionError("registered Codex fixtures must retain two distinct Core sources")
    return profiles
