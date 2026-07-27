#!/usr/bin/env python3
"""Local basics gate for comparing ctx search retrieval modes.

This harness is intentionally local-only: it invokes `ctx search`, sets
opt-out/offline environment variables for the child process, and writes the
snippet-bearing report to a private local JSON file. Treat the output as private
history data. Use `--refresh off` for strictly read-only checks and
`--refresh background` for the default daemon-backed product path.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import datetime as dt
import json
import os
import pathlib
import statistics
import subprocess
import sys
import time
from typing import Any


DEFAULT_MODES = ("hybrid", "lexical", "semantic")
OFFLINE_ENV = {
    "CTX_ANALYTICS_ENABLED": "false",
    "CTX_UPGRADE_AUTO": "off",
    "CTX_SEMANTIC_WORKER_OFF": "1",
    "FASTEMBED_OFFLINE": "1",
    "HF_HUB_OFFLINE": "1",
    "NO_COLOR": "1",
    "TOKENIZERS_PARALLELISM": "false",
    "TRANSFORMERS_OFFLINE": "1",
}
RETRIEVAL_KEYS = (
    "requested_mode",
    "effective_mode",
    "semantic_status",
    "semantic_fallback_code",
    "semantic_fallback",
    "semantic_weight",
    "embedding_model",
)
SAFE_COVERAGE_KEYS = (
    "embedded_items",
    "embedded_chunks",
    "searchable_items",
    "indexed_now",
    "dirty_items",
    "coverage_ratio",
)
SAFE_DIAGNOSTIC_KEYS = (
    "query_embed_ms",
    "vector_scan_ms",
    "chunks_scanned",
    "vector_bytes_read",
    "events_scored",
    "hydration_ms",
    "stale_events_dropped",
    "semantic_candidates",
)
SAFE_DIAGNOSTIC_STRING_KEYS = ()


@dataclass(frozen=True)
class QueryCase:
    id: str
    query: str
    expected_substrings: list[str]
    search_args: list[str]


def positive_int(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be greater than 0")
    return parsed


def non_negative_int(value: str) -> int:
    parsed = int(value)
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be greater than or equal to 0")
    return parsed


def utc_now() -> str:
    return dt.datetime.now(dt.UTC).isoformat().replace("+00:00", "Z")


def expand_path(value: str) -> str:
    return str(pathlib.Path(value).expanduser())


def read_json_source(source: str) -> Any:
    if source == "-":
        raw = sys.stdin.read()
    elif source.lstrip().startswith(("{", "[")):
        raw = source
    else:
        path = pathlib.Path(source).expanduser()
        if not path.exists():
            raw = source
        else:
            raw = path.read_text(encoding="utf-8")
    try:
        return json.loads(raw)
    except json.JSONDecodeError as error:
        raise SystemExit(f"failed to parse query JSON: {error}") from error


def query_items_from_json(data: Any) -> list[Any]:
    if isinstance(data, list):
        return data
    if isinstance(data, dict):
        for key in ("queries", "cases", "items"):
            value = data.get(key)
            if isinstance(value, list):
                return value
        if "query" in data or "q" in data:
            return [data]
    raise SystemExit("query JSON must be an array, an object with queries, or one query object")


def string_list(value: Any, field: str) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        return [value]
    if isinstance(value, list) and all(isinstance(item, str) for item in value):
        return value
    raise SystemExit(f"{field} must be a string or a list of strings")


def expected_substrings(item: dict[str, Any]) -> list[str]:
    for key in ("expected_substrings", "expected", "expects"):
        if key in item:
            return string_list(item.get(key), key)
    return []


def normalize_query_item(item: Any, index: int) -> QueryCase:
    if isinstance(item, str):
        query = item.strip()
        if not query:
            raise SystemExit(f"query {index + 1} is empty")
        return QueryCase(
            id=f"q{index + 1}",
            query=query,
            expected_substrings=[],
            search_args=[],
        )
    if not isinstance(item, dict):
        raise SystemExit(f"query {index + 1} must be a string or object")

    raw_query = item.get("query", item.get("q"))
    if not isinstance(raw_query, str) or not raw_query.strip():
        raise SystemExit(f"query {index + 1} must contain a non-empty query string")
    raw_id = item.get("id", f"q{index + 1}")
    raw_search_args = item.get("search_args", [])
    return QueryCase(
        id=str(raw_id),
        query=raw_query,
        expected_substrings=expected_substrings(item),
        search_args=string_list(raw_search_args, "search_args"),
    )


def load_query_cases(args: argparse.Namespace) -> list[QueryCase]:
    items: list[Any] = []
    if args.query_set:
        items.extend(query_items_from_json(read_json_source(args.query_set)))
    for inline in args.query:
        inline = inline.strip()
        if inline.startswith(("{", "[")):
            items.extend(query_items_from_json(read_json_source(inline)))
        elif inline:
            items.append(inline)
    if not items:
        raise SystemExit("provide --query-set or at least one --query")
    return [normalize_query_item(item, index) for index, item in enumerate(items)]


def child_env() -> dict[str, str]:
    env = os.environ.copy()
    env.update(OFFLINE_ENV)
    return env


def truncate_text(value: Any, max_chars: int) -> str:
    if not isinstance(value, str):
        return ""
    value = " ".join(value.split())
    if max_chars <= 0 or len(value) <= max_chars:
        return value
    return value[: max_chars - 1].rstrip() + "..."


def safe_number_map(data: Any, keys: tuple[str, ...]) -> dict[str, int | float]:
    if not isinstance(data, dict):
        return {}
    return {
        key: data[key]
        for key in keys
        if isinstance(data.get(key), (int, float)) and not isinstance(data.get(key), bool)
    }


def safe_string_map(data: Any, keys: tuple[str, ...]) -> dict[str, str]:
    if not isinstance(data, dict):
        return {}
    return {key: data[key] for key in keys if isinstance(data.get(key), str)}


def compact_retrieval(data: Any, requested_mode: str) -> dict[str, Any]:
    retrieval = data.get("retrieval") if isinstance(data, dict) else None
    if not isinstance(retrieval, dict):
        return {"requested_mode": requested_mode, "effective_mode": "unknown"}

    summary: dict[str, Any] = {
        key: retrieval.get(key)
        for key in RETRIEVAL_KEYS
        if key in retrieval and retrieval.get(key) is not None
    }
    summary.setdefault("requested_mode", requested_mode)
    summary.setdefault("effective_mode", summary.get("requested_mode", "unknown"))
    coverage = safe_number_map(retrieval.get("coverage"), SAFE_COVERAGE_KEYS)
    diagnostics = {
        **safe_number_map(retrieval.get("diagnostics"), SAFE_DIAGNOSTIC_KEYS),
        **safe_string_map(retrieval.get("diagnostics"), SAFE_DIAGNOSTIC_STRING_KEYS),
    }
    if coverage:
        summary["coverage"] = coverage
    if diagnostics:
        summary["diagnostics"] = diagnostics
    return summary


def compact_freshness(data: Any) -> dict[str, Any]:
    freshness = data.get("freshness") if isinstance(data, dict) else None
    if not isinstance(freshness, dict):
        return {}
    summary = {
        key: freshness[key]
        for key in (
            "mode",
            "status",
            "reason",
            "source_count",
            "daemon_last_run_at_ms",
        )
        if key in freshness and freshness[key] is not None
    }
    totals = safe_number_map(
        freshness.get("totals"),
        (
            "imported_sources",
            "failed_sources",
            "imported_sessions",
            "imported_events",
            "imported_edges",
            "skipped",
            "failed",
        ),
    )
    if totals:
        summary["totals"] = totals
    if isinstance(freshness.get("error"), str):
        summary["error"] = truncate_text(freshness["error"], 200)
    return summary


def result_text(result: dict[str, Any]) -> str:
    return "\n".join(
        value
        for value in (
            result.get("title") if isinstance(result.get("title"), str) else "",
            result.get("snippet") if isinstance(result.get("snippet"), str) else "",
        )
        if value
    )


def top_snippets(results: list[Any], top: int, snippet_chars: int) -> list[dict[str, Any]]:
    snippets = []
    for index, result in enumerate(results[:top]):
        if not isinstance(result, dict):
            continue
        snippets.append(
            {
                "rank": index + 1,
                "score": result.get("rank"),
                "title": truncate_text(result.get("title"), snippet_chars),
                "snippet": truncate_text(result.get("snippet"), snippet_chars),
            }
        )
    return snippets


def substring_checks(
    expected: list[str],
    results: list[Any],
    *,
    case_sensitive: bool,
) -> list[dict[str, Any]]:
    checks = []
    haystacks = [
        result_text(result)
        for result in results
        if isinstance(result, dict) and result_text(result)
    ]
    folded_haystacks = haystacks if case_sensitive else [value.lower() for value in haystacks]
    for needle in expected:
        folded_needle = needle if case_sensitive else needle.lower()
        first_rank = None
        for index, haystack in enumerate(folded_haystacks):
            if folded_needle in haystack:
                first_rank = index + 1
                break
        checks.append(
            {
                "substring": needle,
                "matched": first_rank is not None,
                "first_rank": first_rank,
            }
        )
    return checks


def run_search(
    args: argparse.Namespace,
    case: QueryCase,
    mode: str,
) -> dict[str, Any]:
    ctx_bin = (
        expand_path(args.ctx_bin)
        if "/" in args.ctx_bin or "~" in args.ctx_bin
        else args.ctx_bin
    )
    argv = [
        ctx_bin,
        "--data-root",
        expand_path(args.data_root),
        "search",
        case.query,
        "--backend",
        mode,
        "--refresh",
        args.refresh,
        "--limit",
        str(args.limit),
        "--format=json",
        *args.search_arg,
        *case.search_args,
    ]
    started = time.perf_counter()
    try:
        completed = subprocess.run(
            argv,
            check=False,
            capture_output=True,
            env=child_env(),
            text=True,
            timeout=args.timeout_seconds,
        )
        elapsed_ms = (time.perf_counter() - started) * 1000
    except subprocess.TimeoutExpired as error:
        return {
            "mode": mode,
            "elapsed_ms": round((time.perf_counter() - started) * 1000, 3),
            "returncode": None,
            "timed_out": True,
            "stderr_tail": truncate_text(error.stderr, args.stderr_chars),
        }

    run: dict[str, Any] = {
        "mode": mode,
        "elapsed_ms": round(elapsed_ms, 3),
        "returncode": completed.returncode,
    }
    if completed.returncode != 0:
        run["stderr_tail"] = truncate_text(completed.stderr, args.stderr_chars)
        run["stdout_tail"] = truncate_text(completed.stdout, args.stderr_chars)
        return run

    try:
        data = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        run["json_error"] = str(error)
        run["stdout_tail"] = truncate_text(completed.stdout, args.stderr_chars)
        return run

    results = data.get("results") if isinstance(data, dict) else None
    result_list = results if isinstance(results, list) else []
    checks = substring_checks(
        case.expected_substrings,
        result_list,
        case_sensitive=args.case_sensitive,
    )
    run.update(
        {
            "retrieval": compact_retrieval(data, mode),
            "freshness": compact_freshness(data),
            "result_count": len(result_list),
            "top_snippets": top_snippets(result_list, args.top, args.snippet_chars),
            "expected_checks": checks,
            "expected_ok": all(check["matched"] for check in checks),
        }
    )
    return run


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, int((len(ordered) - 1) * pct / 100 + 0.999))
    return ordered[index]


def summarize(query_reports: list[dict[str, Any]], modes: list[str]) -> dict[str, Any]:
    runs = [
        run
        for query_report in query_reports
        for run in query_report.get("runs", [])
        if isinstance(run, dict)
    ]
    command_failures = [
        {"query_id": query_report["id"], "mode": run.get("mode")}
        for query_report in query_reports
        for run in query_report.get("runs", [])
        if isinstance(run, dict)
        and (
            run.get("returncode") not in (0, None)
            or run.get("timed_out")
            or run.get("json_error")
        )
    ]
    expected_failures = []
    for query_report in query_reports:
        for run in query_report.get("runs", []):
            for check in run.get("expected_checks", []):
                if not check.get("matched"):
                    expected_failures.append(
                        {
                            "query_id": query_report["id"],
                            "mode": run.get("mode"),
                            "substring": check.get("substring"),
                        }
                    )
    by_mode = {}
    for mode in modes:
        latencies = [
            float(run["elapsed_ms"])
            for run in runs
            if run.get("mode") == mode and isinstance(run.get("elapsed_ms"), (int, float))
        ]
        by_mode[mode] = {
            "runs": len(latencies),
            "p50_ms": round(statistics.median(latencies), 3) if latencies else 0,
            "p95_ms": round(percentile(latencies, 95), 3),
            "max_ms": round(max(latencies), 3) if latencies else 0,
        }
    ok = not command_failures and not expected_failures
    return {
        "ok": ok,
        "total_runs": len(runs),
        "command_failures": command_failures,
        "expected_failures": expected_failures,
        "by_mode": by_mode,
    }


def prioritized_modes(modes: list[str]) -> list[str]:
    priority = {"hybrid": 0, "lexical": 1, "semantic": 2}
    return [
        mode
        for _, mode in sorted(
            enumerate(modes), key=lambda item: (priority.get(item[1], 100), item[0])
        )
    ]


def write_private_json(path: str, payload: dict[str, Any]) -> None:
    output_path = pathlib.Path(path).expanduser()
    output_path.parent.mkdir(parents=True, exist_ok=True)
    body = json.dumps(payload, indent=2, sort_keys=True) + "\n"
    fd = os.open(output_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(body)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Run a local-only ctx search basics gate across hybrid/lexical/"
            "semantic and write a private JSON report."
        )
    )
    parser.add_argument(
        "--ctx-bin",
        default=os.environ.get("CTX_SEARCH_EVAL_CTX_BIN", "ctx"),
        help="ctx binary to execute (default: ctx or CTX_SEARCH_EVAL_CTX_BIN)",
    )
    parser.add_argument(
        "--data-root",
        required=True,
        help="ctx data root to search; required to avoid accidental default data use",
    )
    parser.add_argument(
        "--output",
        required=True,
        help="private local JSON report path; snippets are written here, not stdout",
    )
    parser.add_argument(
        "--limit",
        type=positive_int,
        default=10,
        help="ctx search limit per query/mode (default: 10)",
    )
    parser.add_argument(
        "--refresh",
        choices=("background", "off", "wait"),
        default="background",
        help="ctx search refresh mode per query/mode (default: background)",
    )
    parser.add_argument(
        "--query-set",
        help=(
            "query set JSON path, '-' for stdin, or inline JSON. Accepts an array, "
            "{'queries': [...]}, or one object with query/q and expected_substrings."
        ),
    )
    parser.add_argument(
        "--query",
        action="append",
        default=[],
        help="inline query text or inline JSON query object; repeatable",
    )
    parser.add_argument(
        "--mode",
        action="append",
        choices=DEFAULT_MODES,
        help="retrieval mode to run; repeatable (default: all four modes)",
    )
    parser.add_argument(
        "--search-arg",
        action="append",
        default=[],
        metavar="ARG",
        help="extra ctx search arg token; repeat for flags/values, e.g. --search-arg=--events",
    )
    parser.add_argument(
        "--top",
        type=non_negative_int,
        default=3,
        help="number of top snippets to record per run (default: 3)",
    )
    parser.add_argument(
        "--snippet-chars",
        type=non_negative_int,
        default=600,
        help="max characters to keep for each title/snippet field (default: 600)",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=positive_int,
        default=120,
        help="per-search subprocess timeout (default: 120)",
    )
    parser.add_argument(
        "--stderr-chars",
        type=non_negative_int,
        default=2000,
        help="max captured stdout/stderr chars on command errors (default: 2000)",
    )
    parser.add_argument(
        "--case-sensitive",
        action="store_true",
        help="make expected substring checks case-sensitive",
    )
    parser.add_argument(
        "--no-fail",
        action="store_true",
        help="write the report and exit 0 even when commands or expected checks fail",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    modes = prioritized_modes(list(args.mode or DEFAULT_MODES))
    cases = load_query_cases(args)

    started = time.perf_counter()
    query_reports = []
    for case in cases:
        runs = [run_search(args, case, mode) for mode in modes]
        query_reports.append(
            {
                "id": case.id,
                "query": case.query,
                "expected_substrings": case.expected_substrings,
                "search_args": case.search_args,
                "runs": runs,
            }
        )

    summary = summarize(query_reports, modes)
    payload = {
        "schema_version": 1,
        "generated_at": utc_now(),
        "local_only": True,
        "privacy": "contains local query text and top snippets; do not publish without review",
        "ctx_bin": args.ctx_bin,
        "data_root": expand_path(args.data_root),
        "refresh": args.refresh,
        "limit": args.limit,
        "modes": modes,
        "search_args": args.search_arg,
        "elapsed_ms": round((time.perf_counter() - started) * 1000, 3),
        "summary": summary,
        "queries": query_reports,
    }
    write_private_json(args.output, payload)
    print(
        f"wrote {expand_path(args.output)}: "
        f"{summary['total_runs']} runs, "
        f"{len(summary['command_failures'])} command failures, "
        f"{len(summary['expected_failures'])} expected misses"
    )
    if summary["ok"] or args.no_fail:
        return 0
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
