#!/usr/bin/env python3
"""Private local search evaluator for semantic/hybrid dogfooding.

The manifest and output intentionally avoid snippets, paths, citations, and raw
provider IDs. Expected IDs are salted hashes so local history can be evaluated
without producing shareable transcript material. The semantic and hybrid
defaults use `--refresh background` so they exercise the daemon-backed product
path instead of the read-only lexical baseline used under `--refresh off`.
"""

import argparse
import hashlib
import json
import os
import pathlib
import re
import shlex
import statistics
import subprocess
import sys
import time


DEFAULT_BACKENDS = {
    "lexical": "{ctx} {data_root_args} search {q} --backend lexical --refresh off --limit {limit} --format json {search_args}",
    "semantic": "{ctx} {data_root_args} search {q} --backend semantic --refresh background --limit {limit} --format json {search_args}",
    "hybrid": "{ctx} {data_root_args} search {q} --backend hybrid --refresh background --limit {limit} --format json {search_args}",
}

DEFAULT_PREFLIGHT_SEARCH_COMMAND = "{ctx} {data_root_args} daemon run --once --format json"
DEFAULT_PREFLIGHT_STATUS_COMMAND = "{ctx} {data_root_args} status --format json"

SAFE_DIAGNOSTIC_KEYS = {
    "query_embed_ms",
    "vector_scan_ms",
    "chunks_scanned",
    "vector_bytes_read",
    "events_scored",
    "hydration_ms",
    "stale_events_dropped",
    "semantic_candidates",
    "vector_backend",
}

UUID_RE = re.compile(
    r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b"
)
LOCAL_PATH_RE = re.compile(r"(/home/|/Users/|file://|[A-Za-z]:\\\\)")
HASH_RE = re.compile(r"^[0-9a-f]{20}$")


def stable_hash(salt, kind, value):
    if not value:
        return None
    return hashlib.sha256(f"{salt}:{kind}:{value}".encode()).hexdigest()[:20]


def shell_join(value):
    return shlex.join(shlex.split(value))


def optional_data_root_args(data_root):
    if not data_root:
        return ""
    return shlex.join(["--data-root", data_root])


def extra_search_args(values):
    if not values:
        return ""
    return shlex.join(values)


def format_command(template, args, query=None, limit=None):
    return " ".join(
        template.format(
            ctx=shell_join(args.ctx_command),
            data_root_args=optional_data_root_args(args.data_root),
            search_args=extra_search_args(args.search_arg),
            q=shlex.quote(query or ""),
            limit=limit if limit is not None else args.limit,
        ).split()
    )


def run_backend(template, args, query, limit):
    cmd = format_command(template, args, query=query, limit=limit)
    started = time.perf_counter()
    completed = subprocess.run(
        cmd,
        shell=True,
        check=True,
        text=True,
        capture_output=True,
        timeout=args.timeout_seconds,
    )
    latency_ms = (time.perf_counter() - started) * 1000
    return latency_ms, json.loads(completed.stdout)


def needs_semantic_preflight(backends):
    return any(
        "--backend semantic" in template or "--backend hybrid" in template
        for template in backends.values()
    )


def semantic_status_summary(data):
    semantic = data.get("semantic") if isinstance(data, dict) else None
    if not isinstance(semantic, dict):
        return None
    summary = {
        key: semantic.get(key)
        for key in (
            "status",
            "running",
            "started_at_ms",
            "heartbeat_at_ms",
            "finished_at_ms",
            "indexed_chunks",
        )
        if key in semantic
    }
    summary["last_error_present"] = bool(semantic.get("last_error"))
    coverage = semantic.get("coverage")
    if isinstance(coverage, dict):
        summary["coverage"] = coverage
    return summary


def semantic_embedded_chunks(summary):
    if not isinstance(summary, dict):
        return 0
    coverage = summary.get("coverage")
    if not isinstance(coverage, dict):
        return 0
    value = coverage.get("embedded_chunks", 0)
    return value if isinstance(value, int) and value > 0 else 0


def semantic_coverage_ratio(summary):
    if not isinstance(summary, dict):
        return 0.0
    coverage = summary.get("coverage")
    if not isinstance(coverage, dict):
        return 0.0
    value = coverage.get("coverage_ratio", 0.0)
    return value if isinstance(value, (int, float)) and value > 0 else 0.0


def run_status_command(command, timeout_seconds):
    completed = subprocess.run(
        command,
        shell=True,
        check=True,
        text=True,
        capture_output=True,
        timeout=timeout_seconds,
    )
    return json.loads(completed.stdout) if completed.stdout.strip() else None


def run_semantic_preflight(backends, rows, args):
    if os.environ.get("CTX_SEARCH_EVAL_PREFLIGHT_SEMANTIC", "1").lower() in {
        "0",
        "false",
        "off",
        "no",
    }:
        return None
    if not needs_semantic_preflight(backends):
        return None
    if not rows:
        return None
    query = os.environ.get("CTX_SEARCH_EVAL_PREFLIGHT_QUERY", rows[0]["query"])
    search_command = format_command(
        os.environ.get(
            "CTX_SEARCH_EVAL_PREFLIGHT_SEARCH_COMMAND",
            DEFAULT_PREFLIGHT_SEARCH_COMMAND,
        ),
        args,
        query=query,
        limit=1,
    )
    status_command = format_command(
        os.environ.get(
            "CTX_SEARCH_EVAL_PREFLIGHT_STATUS_COMMAND",
            DEFAULT_PREFLIGHT_STATUS_COMMAND,
        ),
        args,
    )
    poll_count = int(os.environ.get("CTX_SEARCH_EVAL_PREFLIGHT_POLLS", "30"))
    poll_interval = float(os.environ.get("CTX_SEARCH_EVAL_PREFLIGHT_POLL_INTERVAL", "2"))
    min_coverage_ratio = float(
        os.environ.get("CTX_SEARCH_EVAL_PREFLIGHT_MIN_COVERAGE_RATIO", "0")
    )
    initial_status = semantic_status_summary(
        run_status_command(status_command, args.timeout_seconds)
    )
    initial_chunks = semantic_embedded_chunks(initial_status)
    started = time.perf_counter()
    search_completed = subprocess.run(
        search_command,
        shell=True,
        check=True,
        text=True,
        capture_output=True,
        timeout=args.timeout_seconds,
    )
    search_latency_ms = (time.perf_counter() - started) * 1000
    final_status = None
    polls = []
    saw_worker_running = False
    for index in range(max(poll_count, 0)):
        if index and poll_interval > 0:
            time.sleep(poll_interval)
        status_doc = run_status_command(status_command, args.timeout_seconds)
        final_status = semantic_status_summary(status_doc)
        polls.append(final_status)
        running = bool(isinstance(final_status, dict) and final_status.get("running"))
        saw_worker_running = saw_worker_running or running
        if (
            min_coverage_ratio > 0
            and semantic_coverage_ratio(final_status) >= min_coverage_ratio
        ):
            break
        if min_coverage_ratio <= 0 and semantic_embedded_chunks(final_status) > initial_chunks:
            break
        if saw_worker_running and not running:
            break
        if index > 0 and not saw_worker_running and not running:
            break
    return {
        "command": "<semantic product preflight>",
        "latency_ms": (time.perf_counter() - started) * 1000,
        "search_latency_ms": search_latency_ms,
        "search_returncode": search_completed.returncode,
        "initial_status": initial_status,
        "status": final_status,
        "polls": len(polls),
    }


def score_results(salt, results, expected, excluded_session_hashes):
    accepted_events = set(expected.get("event_hashes", []))
    accepted_sessions = set(expected.get("session_hashes", []))
    excluded_sessions = set(excluded_session_hashes)
    clean = []
    first_rank = None

    for result in results:
        event_hash = stable_hash(salt, "event", result.get("ctx_event_id"))
        session_hash = stable_hash(salt, "session", result.get("ctx_session_id"))
        if session_hash in excluded_sessions:
            continue
        clean.append(
            {
                "event": event_hash,
                "session": session_hash,
                "rank": result.get("rank"),
                "why_matched": result.get("why_matched"),
            }
        )
        if first_rank is None and (
            event_hash in accepted_events or session_hash in accepted_sessions
        ):
            first_rank = len(clean)

    return {
        "hit1": first_rank == 1,
        "hit5": bool(first_rank and first_rank <= 5),
        "mrr": 0 if not first_rank else 1 / first_rank,
        "top": clean[:5],
    }


def append_unique(items, value):
    if value and value not in items:
        items.append(value)


def search_json_hashes(salt, data, limit=None):
    if not isinstance(data, dict):
        raise SystemExit("search JSON must be an object")
    results = data.get("results")
    if not isinstance(results, list):
        raise SystemExit("search JSON must contain a results array")

    event_hashes = []
    session_hashes = []
    selected = results if limit is None else results[:limit]
    for result in selected:
        if not isinstance(result, dict):
            continue
        append_unique(
            event_hashes,
            stable_hash(salt, "event", result.get("ctx_event_id")),
        )
        append_unique(
            session_hashes,
            stable_hash(salt, "session", result.get("ctx_session_id")),
        )

    if not event_hashes and not session_hashes:
        raise SystemExit(
            "search JSON did not contain ctx_event_id or ctx_session_id values"
        )

    return {
        "expected": {
            "event_hashes": event_hashes,
            "session_hashes": session_hashes,
        }
    }


def read_search_json(path):
    if path == "-":
        content = sys.stdin.read()
    else:
        content = pathlib.Path(path).read_text(encoding="utf-8")
    try:
        return json.loads(content)
    except json.JSONDecodeError as error:
        raise SystemExit(f"failed to parse search JSON: {error}") from error


def retrieval_fallback(retrieval):
    if not isinstance(retrieval, dict):
        return False
    if retrieval.get("semantic_fallback"):
        return True
    requested = retrieval.get("requested_mode")
    effective = retrieval.get("effective_mode")
    if requested in {"semantic", "hybrid"} and effective and effective != requested:
        return True
    return False


def retrieval_mode_summary(retrievals):
    samples = [item for item in retrievals if isinstance(item, dict)]
    if not samples:
        return {
            "retrieval_samples": 0,
            "semantic_fallback_count": 0,
            "semantic_fallback_rate": 0,
            "effective_mode_counts": {},
            "effective_mode_rates": {},
        }

    effective_mode_counts = {}
    for retrieval in samples:
        effective_mode = retrieval.get("effective_mode") or "unknown"
        effective_mode_counts[effective_mode] = (
            effective_mode_counts.get(effective_mode, 0) + 1
        )
    fallback_count = sum(1 for retrieval in samples if retrieval_fallback(retrieval))
    diagnostics = [
        retrieval.get("diagnostics")
        for retrieval in samples
        if isinstance(retrieval.get("diagnostics"), dict)
    ]
    diagnostic_summary = {"samples": len(diagnostics)}
    for key in sorted(SAFE_DIAGNOSTIC_KEYS):
        values = [
            item.get(key)
            for item in diagnostics
            if isinstance(item.get(key), (int, float))
        ]
        if values:
            diagnostic_summary[f"{key}_p95"] = pct(values, 95)
            diagnostic_summary[f"{key}_max"] = max(values)
    return {
        "retrieval_samples": len(samples),
        "semantic_fallback_count": fallback_count,
        "semantic_fallback_rate": fallback_count / len(samples),
        "effective_mode_counts": effective_mode_counts,
        "effective_mode_rates": {
            mode: count / len(samples)
            for mode, count in sorted(effective_mode_counts.items())
        },
        "diagnostics": diagnostic_summary,
    }


def safe_retrieval_diagnostics(data):
    if not isinstance(data, dict):
        return None
    summary = {
        key: data.get(key)
        for key in SAFE_DIAGNOSTIC_KEYS
        if isinstance(data.get(key), (int, float))
    }
    return summary or None


def retrieval_summary(data):
    retrieval = data.get("retrieval") if isinstance(data, dict) else None
    if not isinstance(retrieval, dict):
        return None
    worker = retrieval.get("worker")
    coverage = retrieval.get("coverage")
    summary = {
        key: retrieval.get(key)
        for key in (
            "requested_mode",
            "effective_mode",
            "semantic_status",
            "semantic_fallback_code",
            "semantic_fallback",
            "semantic_weight",
            "embedding_model",
        )
        if key in retrieval
    }
    if isinstance(coverage, dict):
        summary["coverage"] = coverage
    if isinstance(worker, dict):
        worker_coverage = worker.get("coverage")
        summary["worker"] = {
            "status": worker.get("status"),
            "running": worker.get("running"),
            "coverage": worker_coverage if isinstance(worker_coverage, dict) else None,
        }
    diagnostics = safe_retrieval_diagnostics(retrieval.get("diagnostics"))
    if diagnostics:
        summary["diagnostics"] = diagnostics
    return summary


def backend_comparison(summary, baseline_backend):
    baseline = summary.get(baseline_backend)
    if not isinstance(baseline, dict):
        return {}
    compared = {}
    baseline_p95 = baseline.get("p95_ms") or 0
    for backend, metrics in sorted(summary.items()):
        if backend == baseline_backend or not isinstance(metrics, dict):
            continue
        p95 = metrics.get("p95_ms") or 0
        compared[backend] = {
            "baseline": baseline_backend,
            "hit1_delta": metrics.get("hit1", 0) - baseline.get("hit1", 0),
            "hit5_delta": metrics.get("hit5", 0) - baseline.get("hit5", 0),
            "mrr_delta": metrics.get("mrr", 0) - baseline.get("mrr", 0),
            "p95_ms_delta": p95 - baseline_p95,
            "p95_ratio": 0 if baseline_p95 <= 0 else p95 / baseline_p95,
            "semantic_fallback_rate_delta": metrics.get(
                "semantic_fallback_rate", 0
            )
            - baseline.get("semantic_fallback_rate", 0),
        }
    return compared


def validate_private_output(payload):
    if os.environ.get("CTX_SEARCH_EVAL_VALIDATE_PRIVACY", "1").lower() in {
        "0",
        "false",
        "off",
        "no",
    }:
        return
    serialized = json.dumps(payload, sort_keys=True)
    if UUID_RE.search(serialized):
        raise SystemExit("refusing to write eval output containing a raw UUID")
    if LOCAL_PATH_RE.search(serialized):
        raise SystemExit("refusing to write eval output containing a local path")
    forbidden_keys = {
        "snippet",
        "citations",
        "source_path",
        "cursor",
        "provider_session_id",
        "ctx_event_id",
        "ctx_session_id",
    }
    seen_forbidden = []

    def walk(value):
        if isinstance(value, dict):
            for key, child in value.items():
                if key in forbidden_keys:
                    seen_forbidden.append(key)
                walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)

    walk(payload)
    if seen_forbidden:
        keys = ", ".join(sorted(set(seen_forbidden)))
        raise SystemExit(f"refusing to write eval output containing raw result keys: {keys}")


def validate_manifest_scaffold(payload):
    validate_private_output(payload)
    if set(payload) != {"expected"}:
        raise SystemExit("manifest scaffold output may only contain expected hashes")
    expected = payload.get("expected")
    if not isinstance(expected, dict):
        raise SystemExit("manifest scaffold expected field must be an object")
    if set(expected) != {"event_hashes", "session_hashes"}:
        raise SystemExit(
            "manifest scaffold expected field may only contain event_hashes and session_hashes"
        )
    for key in ("event_hashes", "session_hashes"):
        values = expected.get(key)
        if not isinstance(values, list):
            raise SystemExit(f"manifest scaffold {key} must be a list")
        for value in values:
            if not isinstance(value, str) or not HASH_RE.match(value):
                raise SystemExit(f"manifest scaffold {key} contains a non-hash value")


def pct(values, percentile):
    ordered = sorted(values)
    if not ordered:
        return 0
    index = min(
        len(ordered) - 1,
        int((len(ordered) - 1) * percentile / 100 + 0.999),
    )
    return ordered[index]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", nargs="?")
    parser.add_argument(
        "--scaffold-from-search-json",
        metavar="PATH",
        help="read private ctx search --format json output from PATH or '-' and emit only salted manifest hashes",
    )
    parser.add_argument(
        "--scaffold-limit",
        type=int,
        help="only hash the first N search results when scaffolding",
    )
    parser.add_argument(
        "--ctx-command",
        default=os.environ.get("CTX_SEARCH_EVAL_CTX", "cargo run -q -p ctx --"),
        help="ctx command prefix, e.g. 'ctx', 'target/release/ctx', or 'cargo run -q -p ctx --'",
    )
    parser.add_argument("--data-root")
    parser.add_argument(
        "--search-arg",
        action="append",
        default=[],
        help="extra ctx search arg; repeat for flags and values",
    )
    parser.add_argument("--limit", type=int, default=10)
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument(
        "--timeout-seconds",
        type=float,
        default=120.0,
        help="per ctx subprocess timeout (default: 120)",
    )
    parser.add_argument(
        "--baseline-backend",
        default="lexical",
        help="backend name to compare against in the private-safe comparison summary",
    )
    parser.add_argument("--output")
    args = parser.parse_args()

    salt = os.environ["CTX_EVAL_SALT"]
    if args.scaffold_limit is not None and args.scaffold_limit <= 0:
        parser.error("--scaffold-limit must be greater than 0")
    if args.scaffold_from_search_json:
        scaffold = search_json_hashes(
            salt,
            read_search_json(args.scaffold_from_search_json),
            limit=args.scaffold_limit,
        )
        validate_manifest_scaffold(scaffold)
        serialized = json.dumps(scaffold, sort_keys=True)
        if args.output:
            pathlib.Path(args.output).write_text(serialized + "\n", encoding="utf-8")
        else:
            print(serialized)
        return
    if not args.manifest:
        parser.error("manifest is required unless --scaffold-from-search-json is used")

    backends = json.loads(
        os.environ.get("CTX_SEARCH_EVAL_BACKENDS", json.dumps(DEFAULT_BACKENDS))
    )
    with open(args.manifest, encoding="utf-8") as manifest:
        rows = [
            json.loads(line)
            for line in manifest
            if line.strip() and not line.startswith("#")
        ]
    preflight = run_semantic_preflight(backends, rows, args)

    per_query = []
    summary = {}
    for backend_name, template in backends.items():
        latencies = []
        hit1 = []
        hit5 = []
        mrr = []
        retrievals = []
        for row in rows:
            samples = []
            data = None
            for _ in range(args.repeats):
                latency_ms, data = run_backend(template, args, row["query"], args.limit)
                samples.append(latency_ms)
            scores = score_results(
                salt,
                data.get("results", []),
                row.get("expected", {}),
                row.get("exclude_session_hashes", []),
            )
            latencies.extend(samples)
            hit1.append(scores["hit1"])
            hit5.append(scores["hit5"])
            mrr.append(scores["mrr"])
            retrieval = retrieval_summary(data)
            retrievals.append(retrieval)
            per_query.append(
                {
                    "id": row["id"],
                    "split": row.get("split"),
                    "backend": backend_name,
                    "latency_ms": samples,
                    "retrieval": retrieval,
                    **scores,
                }
            )
        summary[backend_name] = {
            "p50_ms": statistics.median(latencies) if latencies else 0,
            "p95_ms": pct(latencies, 95),
            "hit1": sum(hit1) / len(hit1) if hit1 else 0,
            "hit5": sum(hit5) / len(hit5) if hit5 else 0,
            "mrr": sum(mrr) / len(mrr) if mrr else 0,
            **retrieval_mode_summary(retrievals),
        }

    output = {
        "summary": summary,
        "comparison": backend_comparison(summary, args.baseline_backend),
        "preflight": preflight,
        "queries": per_query,
    }
    validate_private_output(output)
    serialized = json.dumps(output, indent=2)
    if args.output:
        pathlib.Path(args.output).write_text(serialized + "\n", encoding="utf-8")
    else:
        print(serialized)


if __name__ == "__main__":
    main()
