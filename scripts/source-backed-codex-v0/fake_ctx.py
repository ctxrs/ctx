#!/usr/bin/env python3
"""Small fake ctx candidate used only by self_test.sh."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path


EVENT_ID = "11111111-1111-4111-8111-111111111111"
SESSION_ID = "22222222-2222-4222-8222-222222222222"


def emit_stderr(value: dict[str, object]) -> None:
    print(json.dumps(value, sort_keys=True), file=sys.stderr)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    root.add_argument("--data-root", required=True, type=Path)
    commands = root.add_subparsers(dest="command", required=True)

    search = commands.add_parser("search")
    search.add_argument("--provider", required=True)
    search.add_argument("--backend", required=True)
    search.add_argument("--refresh", required=True)
    search.add_argument("--format", required=True)
    search.add_argument("query")

    show = commands.add_parser("show")
    show_commands = show.add_subparsers(dest="show_target", required=True)
    event = show_commands.add_parser("event")
    event.add_argument("id")
    event.add_argument("--content", required=True)
    event.add_argument("--format", required=True)
    session = show_commands.add_parser("session")
    session.add_argument("id")
    session.add_argument("--content", required=True)
    session.add_argument("--format", required=True)
    return root


def validate_environment() -> Path:
    expected_home = os.environ.get("CTX_BENCH_INHERITED_HOME", "")
    if os.environ.get("HOME", "") != expected_home:
        raise RuntimeError("benchmark harness repurposed HOME")
    codex_home = Path(os.environ["CODEX_HOME"])
    if not codex_home.is_dir():
        raise RuntimeError("CODEX_HOME is not an existing directory")
    return codex_home


def run_search(args: argparse.Namespace, codex_home: Path) -> int:
    if (
        args.provider != "codex"
        or args.backend != "lexical"
        or args.refresh != "wait"
        or args.format != "json"
    ):
        raise RuntimeError("search did not use the required provider/backend/refresh/format")

    args.data_root.mkdir(parents=True, exist_ok=True)
    index_dir = args.data_root / "lexical-index"
    index_dir.mkdir(exist_ok=True)
    counter_path = args.data_root / "fake-search-count"
    count = int(counter_path.read_text(encoding="ascii")) + 1 if counter_path.exists() else 1
    counter_path.write_text(f"{count}\n", encoding="ascii")
    database = args.data_root / "work.sqlite"
    if not database.exists():
        database.write_bytes(b"SQLite format 3\0" + b"\0" * 4080)
        (index_dir / "segment-0001").write_bytes(b"fake lexical segment\n")
    cold = count == 1
    phase = "cold-ingest" if cold else "warm-refresh"

    emit_stderr(
        {
            "type": "ctx_progress",
            "operation": "search-refresh",
            "phase": "source-inventory",
            "message": "codex",
            "completed_bytes": 19,
            "total_bytes": 19,
            "percent": 100.0,
            "elapsed_seconds": 0.001,
            "eta_seconds": None,
            "completed_files": 1,
            "total_files": 1,
            "imported_events": 1,
            "done": True,
        }
    )
    emit_stderr(
        {
            "type": "ctx_phase_attribution",
            "operation": "search-refresh",
            "phase": phase,
            "wall_seconds": 0.002 if cold else 0.001,
        }
    )
    result = {
        "schema_version": 1,
        "payload_type": "search_results",
        "query": args.query,
        "freshness": {
            "mode": "wait",
            "status": "completed",
            "source_count": 1,
            "totals": {"source_bytes": 19},
        },
        "retrieval": {
            "requested_mode": "lexical",
            "effective_mode": "lexical",
            "phase_attribution": {
                "source_inventory_seconds": 0.001,
                "projection_seconds": 0.001 if cold else 0.0,
                "lexical_query_seconds": 0.001,
            },
        },
        "results": [
            {
                "ctx_event_id": EVENT_ID,
                "ctx_session_id": SESSION_ID,
                "event_id": EVENT_ID,
                "session_id": SESSION_ID,
                "provider": "codex",
                "source_path": str(
                    codex_home / "archived_sessions" / "fake-session.jsonl"
                ),
                "title": "fake result",
                "snippet": args.query,
                "rank": 1.0,
            }
        ],
    }
    print(json.dumps(result, sort_keys=True))
    return 0


def run_show(args: argparse.Namespace) -> int:
    if args.format != "json" or args.content not in {"complete", "indexed"}:
        raise RuntimeError("show did not use the expected format/content policy")
    expected = EVENT_ID if args.show_target == "event" else SESSION_ID
    if args.id != expected:
        raise RuntimeError(f"unexpected {args.show_target} id")
    emit_stderr(
        {
            "type": "ctx_phase_attribution",
            "operation": f"show-{args.show_target}",
            "phase": "source-resolve",
            "wall_seconds": 0.001,
        }
    )
    print(
        json.dumps(
            {
                "schema_version": 1,
                "payload_type": f"show_{args.show_target}",
                "ctx_event_id": EVENT_ID,
                "ctx_session_id": SESSION_ID,
                "content_policy": args.content,
                "events": [{"event_id": EVENT_ID, "payload": "fake complete content"}],
            },
            sort_keys=True,
        )
    )
    return 0


def main() -> int:
    args = parser().parse_args()
    codex_home = validate_environment()
    if args.command == "search":
        return run_search(args, codex_home)
    return run_show(args)


if __name__ == "__main__":
    raise SystemExit(main())
