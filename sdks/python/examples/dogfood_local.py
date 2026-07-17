"""Small offline dogfood example for the local ctx agent history client.

Run directly after installing the SDK editable, or set CTX_AGENT_HISTORY_CTX to point
at a real ctx binary. With no environment, the example uses a temporary fake ctx
script and never reads private history or makes network calls.
"""

from __future__ import annotations

import json
import os
import stat
import tempfile
import textwrap
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator, Optional, Union

from ctx_agent_history import (
    LocateEventResponse,
    LocateSessionResponse,
    AgentHistoryClient,
    ImportResponse,
    InitResponse,
    SearchResponse,
    ShowEventResponse,
    ShowSessionResponse,
    StatusResponse,
)

Pathish = Union[str, Path]
EVENT_ID = "11111111-1111-4111-8111-111111111111"
SESSION_ID = "22222222-2222-4222-8222-222222222222"


@dataclass(frozen=True)
class DogfoodSnapshot:
    status: StatusResponse
    init: InitResponse
    imported: ImportResponse
    synced: ImportResponse
    search: SearchResponse
    event: ShowEventResponse
    session: ShowSessionResponse
    event_location: LocateEventResponse
    session_location: LocateSessionResponse

    def as_dict(self) -> dict[str, object]:
        return {
            "status": self.status,
            "init": self.init,
            "imported": self.imported,
            "synced": self.synced,
            "search": self.search,
            "event": self.event,
            "session": self.session,
            "event_location": self.event_location,
            "session_location": self.session_location,
        }


def run_demo(client: AgentHistoryClient) -> DogfoodSnapshot:
    return DogfoodSnapshot(
        status=client.status(),
        init=client.init(catalog_only=True),
        imported=client.import_(provider="codex", resume=True),
        synced=client.sync(all=True),
        search=client.search(
            {"version": "ctx-search-v1", "any": [{"all": "local agent history"}]},
            provider="codex",
            limit=5,
            refresh="off",
        ),
        event=client.show_event(EVENT_ID, window=1),
        session=client.show_session(SESSION_ID, mode="lite"),
        event_location=client.locate_event(EVENT_ID),
        session_location=client.locate_session(SESSION_ID),
    )


@contextmanager
def local_client(
    *,
    ctx_binary: Optional[Pathish] = None,
    data_root: Optional[Pathish] = None,
) -> Iterator[AgentHistoryClient]:
    configured_ctx = str(ctx_binary or os.environ.get("CTX_AGENT_HISTORY_CTX") or "")
    configured_data_root = (
        data_root or os.environ.get("CTX_AGENT_HISTORY_DATA_ROOT") or "/tmp/ctx-agent-history-dogfood"
    )
    if configured_ctx:
        yield AgentHistoryClient.local(ctx_binary=configured_ctx, data_root=configured_data_root)
        return

    with _FakeCtx() as fake_ctx:
        yield AgentHistoryClient.local(ctx_binary=str(fake_ctx), data_root=configured_data_root)


def run(
    *,
    ctx_binary: Optional[Pathish] = None,
    data_root: Optional[Pathish] = None,
) -> DogfoodSnapshot:
    with local_client(ctx_binary=ctx_binary, data_root=data_root) as client:
        return run_demo(client)


def main() -> None:
    print(json.dumps(run().as_dict(), indent=2, sort_keys=True))


class _FakeCtx:
    def __init__(self) -> None:
        self._tmp: Optional[tempfile.TemporaryDirectory[str]] = None
        self.path: Optional[Path] = None

    def __enter__(self) -> Path:
        self._tmp = tempfile.TemporaryDirectory()
        self.path = Path(self._tmp.name) / "ctx"
        self.path.write_text(_fake_ctx_script(), encoding="utf-8")
        self.path.chmod(self.path.stat().st_mode | stat.S_IXUSR)
        return self.path

    def __exit__(self, exc_type, exc, tb) -> None:  # type: ignore[no-untyped-def]
        if self._tmp is not None:
            self._tmp.cleanup()


def _fake_ctx_script() -> str:
    return textwrap.dedent(
        f"""\
        #!/usr/bin/env python3
        import json
        import sys

        EVENT_ID = {EVENT_ID!r}
        SESSION_ID = {SESSION_ID!r}
        args = sys.argv[1:]
        if args[:2] == ["--data-root", "/tmp/ctx-agent-history-dogfood"]:
            args = args[2:]

        payload = {{"schema_version": 1}}
        if args == ["status", "--json"]:
            payload.update({{"initialized": True, "data_root": "/tmp/ctx-agent-history-dogfood"}})
        elif args[:2] == ["setup", "--json"]:
            payload.update({{"mode": "catalog_only", "data_root": "/tmp/ctx-agent-history-dogfood", "indexed_items": 1}})
        elif args[:2] == ["import", "--json"]:
            payload.update(
                {{
                    "resume": True,
                    "totals": {{"imported_sources": 1, "imported_sessions": 1, "imported_events": 1}},
                    "sources": [
                        {{
                            "provider": "codex",
                            "path": "/tmp/ctx-agent-history-dogfood/session.jsonl",
                            "status": "imported",
                            "imported_sessions": 1,
                            "imported_events": 1,
                        }}
                    ],
                }}
            )
        elif args and args[0] == "search":
            query = json.loads(args[args.index("--query-json") + 1])
            payload.update(
                {{
                    "schema_version": 2,
                    "query": query,
                    "query_execution": {{
                        "query_version": "ctx-search-v1",
                        "candidate_strategy": "bounded_fts",
                        "resolved": {{
                            "query_bytes": 8192,
                            "clauses": 32,
                            "analyzed_tokens_per_clause": 32,
                            "candidates_per_positive_seed": 1024,
                            "candidate_rows": 16384,
                            "retained_candidate_ids": 8192,
                            "residual_rows": 8192,
                            "verification_bytes": 16777216,
                            "verification_lookup_bytes": 16384,
                            "hydrated_rows": 256,
                            "hydration_input_bytes": 8388608,
                            "hydration_input_bytes_per_event": 65536,
                            "snippet_input_bytes": 8388608,
                            "returned_text_bytes": 524288,
                            "serialized_response_bytes": 2097152,
                            "results": 5,
                            "elapsed_ms": 1000,
                        }},
                        "consumed": {{
                            "query_bytes": 19,
                            "clauses": 1,
                            "analyzed_tokens": 3,
                            "largest_analyzed_tokens_per_clause": 3,
                            "largest_positive_seed_candidates": 1,
                            "candidate_rows": 1,
                            "retained_candidate_ids": 1,
                            "residual_rows": 1,
                            "verification_bytes": 16,
                            "largest_verification_lookup_bytes": 16,
                            "hydrated_rows": 1,
                            "hydration_input_bytes": 32,
                            "largest_hydration_input_bytes": 32,
                            "snippet_input_bytes": 32,
                            "returned_results": 1,
                            "returned_text_bytes": 32,
                            "serialized_response_bytes": 1000,
                            "elapsed_ms": 2,
                        }},
                        "semantic": {{
                            "attempted": False,
                            "required": False,
                            "readiness": "unavailable",
                            "effective_backend": "lexical",
                            "requested_candidates": 0,
                            "eligible_candidates": 0,
                            "candidates_supplied": 0,
                            "candidates_consumed": 0,
                            "candidates_used": 0,
                            "coverage": {{}},
                            "completeness": "not_attempted",
                            "positive_text_rule_version": "ctx-search-positive-text-v1",
                        }},
                        "rrf_k": 60,
                        "per_branch_candidate_rows": 1024,
                        "requested_result_limit": 5,
                        "result_limit": 5,
                        "max_result_limit": 200,
                        "clauses_executed": 1,
                        "verification_dropped": 0,
                        "filter_verification_dropped": 0,
                        "candidate_budget_exhausted": False,
                        "timed_out": False,
                        "truncated": False,
                    }},
                    "filters": {{"provider": "codex"}},
                    "freshness": {{"mode": "off", "status": "skipped"}},
                    "results": [
                        {{
                            "ctx_event_id": EVENT_ID,
                            "ctx_session_id": SESSION_ID,
                            "provider_session_id": "codex-fixture-session",
                            "result_scope": "event",
                            "provider": "codex",
                            "snippet": "local agent history search result",
                        }}
                    ],
                }}
            )
        elif args[:2] == ["show", "event"]:
            payload.update(
                {{
                    "event": {{
                        "ctx_event_id": args[2],
                        "ctx_session_id": SESSION_ID,
                        "event_type": "message",
                        "role": "assistant",
                        "text": "local agent history search result",
                    }},
                    "events": [],
                    "source": {{"path": "/tmp/ctx-agent-history-dogfood/session.jsonl", "exists": True}},
                }}
            )
        elif args[:2] == ["show", "session"]:
            payload.update(
                {{
                    "ctx_session_id": args[2],
                    "provider_session_id": "codex-fixture-session",
                    "session": {{"provider": "codex", "title": "Fixture session"}},
                    "events": [],
                    "source": {{"path": "/tmp/ctx-agent-history-dogfood/session.jsonl", "exists": True}},
                    "mode": "lite",
                    "format": "json",
                }}
            )
        elif args[:2] == ["locate", "event"]:
            payload.update(
                {{
                    "ctx_session_id": SESSION_ID,
                    "ctx_event_id": args[2],
                    "provider": "codex",
                    "source": {{"path": "/tmp/ctx-agent-history-dogfood/session.jsonl", "exists": True}},
                }}
            )
        elif args[:2] == ["locate", "session"]:
            payload.update(
                {{
                    "ctx_session_id": args[2],
                    "provider": "codex",
                    "source": {{"path": "/tmp/ctx-agent-history-dogfood/session.jsonl", "exists": True}},
                }}
            )
        else:
            raise SystemExit(f"unexpected fake ctx args: {{args!r}}")

        print(json.dumps(payload))
        """
    )


if __name__ == "__main__":
    main()
