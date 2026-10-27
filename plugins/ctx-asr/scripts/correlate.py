#!/usr/bin/env python3
"""Offline UUID associations, never a live hook and never a content merger.

Pass explicit native transcript files after the session exits. Output is an
ordinary JSONL link map, not another ASR stream or a change to the observer log.
"""

import argparse
import json
import os
from pathlib import Path

from projection import META, identifier
from storage import decode, encode

MAX_LINE = 16 * 1024 * 1024
MAX_KEYS = 100000


def records(path):
    with open(path, "rb") as source:
        while True:
            line = source.readline(MAX_LINE + 1)
            if not line:
                break
            if len(line) > MAX_LINE:
                raise ValueError("record limit exceeded")
            if not line.endswith(b"\n"):
                break
            try:
                row = decode(line)
            except (ValueError, UnicodeError, RecursionError):
                continue
            if isinstance(row, dict):
                yield row


def native_index(paths):
    index = {}
    count = 0
    for path in paths:
        for row in records(path):
            session, agent, uuid = (identifier(row.get(k)) for k in ("sessionId", "agentId", "uuid"))
            if not session or not uuid:
                continue
            message = row.get("message")
            if not isinstance(message, dict):
                continue
            content = message.get("content")
            parts = content if isinstance(content, list) else []
            keys = []
            for part in parts:
                if not isinstance(part, dict):
                    continue
                kind = part.get("type")
                if row.get("type") == "assistant" and kind == "tool_use":
                    keys.append(("call", identifier(part.get("id"))))
                if row.get("type") == "user" and kind == "tool_result":
                    keys.append(("result", identifier(part.get("tool_use_id"))))
            if (row.get("type") == "user" and not keys and not row.get("isMeta")
                    and not row.get("isCompactSummary")):
                keys.append(("prompt", identifier(row.get("promptId"))))
            for kind, key in keys:
                if key:
                    bucket = index.setdefault((session, agent, kind, key), set())
                    if uuid not in bucket:
                        count += 1
                        if count > MAX_KEYS:
                            raise ValueError("index limit exceeded")
                        bucket.add(uuid)
    return index


def correlate(stream, paths):
    index = native_index(paths)
    for row in records(stream):
        source = row.get("_meta", {}).get(META)
        if not isinstance(source, dict):
            continue
        event = source.get("event")
        kind = {"PreToolUse": "call", "PostToolUse": "result",
                "PostToolUseFailure": "result", "PermissionDenied": "result",
                "UserPromptSubmit": "prompt"}.get(event)
        key = source.get("promptId" if kind == "prompt" else "toolUseId")
        session = identifier(source.get("sessionId"))
        agent = identifier(source.get("agentId"))
        matches = sorted(index.get((session, agent, kind, key), [])) if kind and key else []
        yield {"asrRecordId": row["id"], "sessionId": session,
               "relation": "event_association", "basis": kind or "session",
               "status": "unique" if len(matches) == 1 else "ambiguous" if matches else "unresolved",
               "nativeRecordUuids": matches}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stream", required=True, type=Path)
    parser.add_argument("--transcript", required=True, type=Path, action="append")
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    counts = {"unique": 0, "ambiguous": 0, "unresolved": 0}
    try:
        fd = os.open(args.out, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as dest:
            for row in correlate(args.stream, args.transcript):
                dest.write(encode(row))
                counts[row["status"]] += 1
    except (OSError, ValueError, TypeError, RecursionError):
        # Do not print native input or a path that could contain a secret.
        parser.exit(1, "Correlation failed; check input format and output availability.\n")
    print(json.dumps(counts))


if __name__ == "__main__":
    main()
