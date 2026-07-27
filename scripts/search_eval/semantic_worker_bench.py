#!/usr/bin/env python3
"""Benchmark ctx search plus default semantic catch-up.

Measures first-search wall time, worker progress over time, sidecar size, RSS
when a worker PID is visible through status, and an optional incremental search
+ catch-up cycle. Output omits search result rows and raw queries.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shlex
import subprocess
import sys
import time
from typing import Any


GLOBAL_SENSITIVE_VALUE_FLAGS = {
    "--data-root",
}

SENSITIVE_SEARCH_VALUE_FLAGS = {
    "--file",
    "--history-source",
    "--provider-key",
    "--provider-session",
    "--session",
    "--since",
    "--source-format",
    "--source-id",
    "--term",
    "--workspace",
}

PATH_VALUE_FLAGS = {
    "--data-root",
    "--file",
    "--workspace",
}

SAFE_COVERAGE_KEYS = {
    "searchable_items",
    "embedded_items",
    "embedded_chunks",
    "queued_items_estimate",
    "coverage_ratio",
    "indexed_now",
}

SAFE_DIAGNOSTIC_KEYS = {
    "query_embed_ms",
    "vector_scan_ms",
    "chunks_scanned",
    "vector_bytes_read",
    "events_scored",
    "hydration_ms",
    "stale_events_dropped",
    "semantic_candidates",
}

UUID_RE = re.compile(
    r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b"
)
LOCAL_PATH_RE = re.compile(r"(/home/|/Users/|file://|[A-Za-z]:\\\\)")
WINDOWS_PATH_RE = re.compile(r"^[A-Za-z]:[\\/]")

FORBIDDEN_OUTPUT_KEYS = {
    "citations",
    "ctx_event_id",
    "ctx_session_id",
    "cursor",
    "last_error",
    "lock_path",
    "path",
    "provider_session_id",
    "snippet",
    "source_path",
    "stderr",
    "vector_path",
}


def stable_hash(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()[:20]


def sidecar_file_bytes(path: str | None) -> int:
    if not path:
        return 0
    total = 0
    for suffix in ("", "-wal", "-shm"):
        try:
            total += pathlib.Path(path + suffix).stat().st_size
        except FileNotFoundError:
            pass
    return total


def read_linux_rss_kib(pid: int | str | None) -> int | None:
    try:
        pid_int = int(pid) if pid not in (None, "") else None
    except (TypeError, ValueError):
        return None
    if pid_int is None:
        return None
    try:
        with open(f"/proc/{pid_int}/status", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("VmRSS:"):
                    fields = line.split()
                    return int(fields[1]) if len(fields) >= 2 else None
    except OSError:
        return None
    return None


def nested_string(data: Any, path: tuple[str, ...]) -> str | None:
    current = data
    for key in path:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    return current if isinstance(current, str) and current else None


def extract_sidecar_path(
    *documents: Any,
    explicit_sidecar: str | None = None,
    data_root: str | None = None,
) -> str | None:
    if explicit_sidecar:
        return str(pathlib.Path(explicit_sidecar).expanduser())
    for document in documents:
        for path in (
            ("retrieval", "vector_path"),
            ("vector_path",),
            ("semantic", "vector_path"),
            ("sidecar", "path"),
            ("sidecar_path",),
        ):
            value = nested_string(document, path)
            if value:
                return value
    if data_root:
        return str(pathlib.Path(data_root).expanduser() / "vectors.sqlite")
    return None


def private_text_summary(value: str | None) -> dict[str, Any] | None:
    if not isinstance(value, str):
        return None
    value = value.strip()
    if not value:
        return None
    return {
        "present": True,
        "bytes": len(value.encode("utf-8")),
        "sha256": stable_hash(value),
    }


def redacted_scalar(value: str, label: str) -> str:
    return f"<{label}:sha256:{stable_hash(value)}:chars:{len(value)}>"


def is_path_like(value: str) -> bool:
    return (
        value.startswith(("/", "./", "../", "~/", "file://"))
        or "/" in value
        or "\\" in value
        or bool(WINDOWS_PATH_RE.match(value))
    )


def split_inline_flag(token: str) -> tuple[str, str] | None:
    if not token.startswith("--") or "=" not in token:
        return None
    flag, value = token.split("=", 1)
    return flag, value


def is_sensitive_value_flag(flag: str, in_search: bool) -> bool:
    return flag in GLOBAL_SENSITIVE_VALUE_FLAGS or (
        in_search and flag in SENSITIVE_SEARCH_VALUE_FLAGS
    )


def redacted_flag_value(flag: str, value: str) -> str:
    label = "path" if flag in PATH_VALUE_FLAGS or is_path_like(value) else "value"
    return redacted_scalar(value, label)


def redact_argv(argv: list[str]) -> list[str]:
    redacted = []
    in_search = False
    query_redacted = False
    redact_next: str | None = None
    for token in argv:
        if redact_next is not None:
            redacted.append(redacted_flag_value(redact_next, token))
            redact_next = None
            continue
        inline = split_inline_flag(token)
        if inline and is_sensitive_value_flag(inline[0], in_search):
            flag, value = inline
            redacted.append(f"{flag}={redacted_flag_value(flag, value)}")
            continue
        if in_search and not query_redacted and not token.startswith("-"):
            redacted.append(redacted_scalar(token, "query"))
            query_redacted = True
            continue
        if is_sensitive_value_flag(token, in_search):
            redacted.append(token)
            redact_next = token
            continue
        redacted.append(redacted_scalar(token, "path") if is_path_like(token) else token)
        if token == "search":
            in_search = True
    return redacted


def parse_json_or_none(text: str) -> tuple[Any, str | None]:
    text = text.strip()
    if not text:
        return None, None
    try:
        return json.loads(text), None
    except json.JSONDecodeError as error:
        return None, str(error)


def ctx_base_argv(ctx_command: str, data_root: str | None) -> list[str]:
    argv = shlex.split(ctx_command)
    if data_root:
        argv.extend(["--data-root", data_root])
    return argv


def run_json(argv: list[str], cwd: str | None, timeout_seconds: float) -> dict[str, Any]:
    started = time.perf_counter()
    try:
        completed = subprocess.run(
            argv,
            cwd=cwd,
            text=True,
            capture_output=True,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as error:
        stdout = error.stdout or ""
        stderr = error.stderr or ""
        if isinstance(stdout, bytes):
            stdout = stdout.decode(errors="replace")
        if isinstance(stderr, bytes):
            stderr = stderr.decode(errors="replace")
        parsed, parse_error = parse_json_or_none(stdout)
        return {
            "argv": redact_argv(argv),
            "returncode": 124,
            "timed_out": True,
            "wall_ms": (time.perf_counter() - started) * 1000,
            "json": parsed,
            "stderr_summary": private_text_summary(stderr),
            "stdout_parse_error": parse_error,
        }
    parsed, parse_error = parse_json_or_none(completed.stdout)
    return {
        "argv": redact_argv(argv),
        "returncode": completed.returncode,
        "timed_out": False,
        "wall_ms": (time.perf_counter() - started) * 1000,
        "json": parsed,
        "stderr_summary": private_text_summary(completed.stderr),
        "stdout_parse_error": parse_error,
    }


def command_summary(run: dict[str, Any]) -> dict[str, Any]:
    summary = {
        "argv": run["argv"],
        "returncode": run["returncode"],
        "timed_out": bool(run.get("timed_out")),
        "wall_ms": run["wall_ms"],
    }
    stderr_summary = run.get("stderr_summary")
    if stderr_summary:
        summary["stderr_present"] = stderr_summary["present"]
        summary["stderr_bytes"] = stderr_summary["bytes"]
        summary["stderr_sha256"] = stderr_summary["sha256"]
    if run.get("stdout_parse_error"):
        summary["stdout_parse_error"] = run["stdout_parse_error"]
    return summary


def sanitize_coverage(data: Any) -> dict[str, Any] | None:
    if not isinstance(data, dict):
        return None
    summary = {key: data.get(key) for key in SAFE_COVERAGE_KEYS if key in data}
    return summary or None


def sanitize_diagnostics(data: Any) -> dict[str, Any] | None:
    if not isinstance(data, dict):
        return None
    summary = {
        key: data.get(key)
        for key in SAFE_DIAGNOSTIC_KEYS
        if isinstance(data.get(key), (int, float))
    }
    return summary or None


def sanitize_worker_status(data: Any) -> dict[str, Any] | None:
    if not isinstance(data, dict):
        return None
    summary = {
        key: data.get(key)
        for key in (
            "status",
            "running",
            "pid",
            "started_at_ms",
            "heartbeat_at_ms",
            "finished_at_ms",
            "indexed_chunks",
        )
        if key in data
    }
    last_error = private_text_summary(data.get("last_error"))
    if last_error:
        summary["last_error_present"] = True
        summary["last_error_bytes"] = last_error["bytes"]
        summary["last_error_sha256"] = last_error["sha256"]
    coverage = sanitize_coverage(data.get("coverage"))
    if coverage:
        summary["coverage"] = coverage
    return summary or None


def sanitize_retrieval(data: Any) -> dict[str, Any] | None:
    retrieval = data.get("retrieval") if isinstance(data, dict) else None
    if not isinstance(retrieval, dict):
        return None
    summary = {
        key: retrieval.get(key)
        for key in (
            "requested_mode",
            "effective_mode",
            "semantic_weight",
            "semantic_status",
            "semantic_fallback_code",
            "semantic_fallback",
            "embedding_model",
        )
        if key in retrieval
    }
    coverage = sanitize_coverage(retrieval.get("coverage"))
    if coverage:
        summary["coverage"] = coverage
    worker = sanitize_worker_status(retrieval.get("worker"))
    if worker:
        summary["worker"] = worker
    diagnostics = sanitize_diagnostics(retrieval.get("diagnostics"))
    if diagnostics:
        summary["diagnostics"] = diagnostics
    summary["has_vector_path"] = bool(retrieval.get("vector_path"))
    return summary


def sanitize_freshness(data: Any) -> dict[str, Any] | None:
    if not isinstance(data, dict):
        return None
    summary = {
        key: data.get(key)
        for key in (
            "mode",
            "status",
            "source_count",
        )
        if key in data
    }
    totals = data.get("totals")
    if isinstance(totals, dict):
        summary["totals"] = {
            key: value
            for key, value in totals.items()
            if isinstance(value, (int, float, bool)) or value is None
        }
    error = private_text_summary(data.get("error"))
    if error:
        summary["error_present"] = True
        summary["error_bytes"] = error["bytes"]
        summary["error_sha256"] = error["sha256"]
    return summary or None


def sanitize_search_json(data: Any) -> dict[str, Any] | None:
    if not isinstance(data, dict):
        return None
    results = data.get("results")
    summary = {
        "schema_version": data.get("schema_version"),
        "payload_type": data.get("payload_type"),
        "result_count": len(results) if isinstance(results, list) else None,
        "freshness": sanitize_freshness(data.get("freshness")),
        "retrieval": sanitize_retrieval(data),
    }
    if isinstance(data.get("truncated"), dict):
        summary["truncated"] = data["truncated"]
    return summary


def run_search(args, query: str, sidecar_path: str | None) -> tuple[dict[str, Any], str | None]:
    argv = ctx_base_argv(args.ctx_command, args.data_root)
    argv.extend(
        [
            "search",
            query,
            "--backend",
            args.backend,
            "--refresh",
            args.refresh,
            "--limit",
            str(args.limit),
            "--format=json",
        ]
    )
    argv.extend(args.search_arg)
    run = run_json(argv, args.cwd, args.timeout_seconds)
    sidecar_path = extract_sidecar_path(
        run["json"],
        explicit_sidecar=args.sidecar or sidecar_path,
        data_root=args.data_root,
    )
    return (
        {
            "command": command_summary(run),
            "search": sanitize_search_json(run["json"]),
            "sidecar_bytes": sidecar_file_bytes(sidecar_path),
        },
        sidecar_path,
    )


def capture_status(args, sidecar_path: str | None) -> tuple[dict[str, Any], str | None]:
    argv = ctx_base_argv(args.ctx_command, args.data_root)
    argv.extend(["status", "--format=json"])
    run = run_json(argv, args.cwd, args.timeout_seconds)
    semantic_doc = run["json"].get("semantic") if isinstance(run["json"], dict) else None
    sidecar_path = extract_sidecar_path(
        run["json"],
        semantic_doc,
        explicit_sidecar=args.sidecar or sidecar_path,
        data_root=args.data_root,
    )
    status = sanitize_worker_status(semantic_doc)
    if status is not None:
        status["sidecar_bytes"] = sidecar_file_bytes(sidecar_path)
        status["worker_rss_kib"] = read_linux_rss_kib(status.get("pid"))
    return {"command": command_summary(run), "status": status}, sidecar_path


def poll_status(args, label: str, sidecar_path: str | None):
    samples = []
    started = time.perf_counter()
    for index in range(args.polls):
        if index and args.poll_interval > 0:
            time.sleep(args.poll_interval)
        sample, sidecar_path = capture_status(args, sidecar_path)
        sample["label"] = label
        sample["sample_index"] = index
        sample["elapsed_ms"] = (time.perf_counter() - started) * 1000
        samples.append(sample)
    return samples, sidecar_path


def ctx_command_summary(ctx_command: str) -> dict[str, Any]:
    argv = shlex.split(ctx_command)
    return {
        "argv": redact_argv(argv),
        "argc": len(argv),
        "sha256": stable_hash(ctx_command),
        "uses_cargo": bool(argv and argv[0] == "cargo"),
    }


def private_values_from_args(args) -> dict[str, list[str]]:
    path_values = [value for value in (args.data_root, args.cwd, args.sidecar) if value]
    for token in shlex.split(args.ctx_command):
        if is_path_like(token):
            path_values.append(token)
    for token in args.search_arg:
        if is_path_like(token):
            path_values.append(token)
    return {
        "queries": [
            value
            for value in (args.query, args.incremental_query)
            if isinstance(value, str) and value
        ],
        "paths": path_values,
    }


def validate_private_output(
    payload: Any,
    *,
    raw_queries: list[str] | tuple[str, ...] = (),
    raw_paths: list[str] | tuple[str, ...] = (),
) -> None:
    if os.environ.get("CTX_SEARCH_EVAL_VALIDATE_PRIVACY", "1").lower() in {
        "0",
        "false",
        "off",
        "no",
    }:
        return
    serialized = json.dumps(payload, sort_keys=True)
    if UUID_RE.search(serialized):
        raise SystemExit("refusing to write benchmark output containing a raw UUID")
    if LOCAL_PATH_RE.search(serialized):
        raise SystemExit("refusing to write benchmark output containing a local path")
    for query in raw_queries:
        if query and query in serialized:
            raise SystemExit("refusing to write benchmark output containing raw query text")
    for path in raw_paths:
        if path and path in serialized:
            raise SystemExit("refusing to write benchmark output containing a raw local path")

    seen_forbidden = []

    def walk(value):
        if isinstance(value, dict):
            for key, child in value.items():
                if key in FORBIDDEN_OUTPUT_KEYS:
                    seen_forbidden.append(key)
                walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)

    walk(payload)
    if seen_forbidden:
        keys = ", ".join(sorted(set(seen_forbidden)))
        raise SystemExit(f"refusing to write benchmark output containing raw result keys: {keys}")


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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--query", required=True)
    parser.add_argument("--incremental-query")
    parser.add_argument(
        "--ctx-command",
        default=os.environ.get("CTX_BENCH_CTX", "ctx"),
        help="ctx command prefix, e.g. 'ctx' or 'cargo run -q -p ctx --'",
    )
    parser.add_argument("--data-root")
    parser.add_argument("--cwd")
    parser.add_argument("--sidecar", help="explicit vectors.sqlite path")
    parser.add_argument("--backend", choices=["hybrid", "lexical", "semantic"], default="hybrid")
    parser.add_argument("--refresh", choices=["background", "off", "wait"], default="background")
    parser.add_argument("--limit", type=positive_int, default=20)
    parser.add_argument("--polls", type=non_negative_int, default=6)
    parser.add_argument("--poll-interval", type=float, default=2.0)
    parser.add_argument(
        "--timeout-seconds",
        type=float,
        default=120.0,
        help="per ctx subprocess timeout (default: 120)",
    )
    parser.add_argument(
        "--search-arg",
        action="append",
        default=[],
        help="extra ctx search arg; repeat for flags and values",
    )
    parser.add_argument("--output")
    args = parser.parse_args()

    sidecar_path = extract_sidecar_path(explicit_sidecar=args.sidecar, data_root=args.data_root)
    status_before, sidecar_path = capture_status(args, sidecar_path)
    first_search, sidecar_path = run_search(args, args.query, sidecar_path)
    status_samples, sidecar_path = poll_status(args, "after_first_search", sidecar_path)

    incremental = None
    if args.incremental_query:
        incremental_search, sidecar_path = run_search(args, args.incremental_query, sidecar_path)
        incremental_samples, sidecar_path = poll_status(
            args,
            "after_incremental_search",
            sidecar_path,
        )
        incremental = {
            "query_hash": stable_hash(args.incremental_query),
            "query_chars": len(args.incremental_query),
            "search": incremental_search,
            "status_samples": incremental_samples,
        }

    metrics = {
        "config": {
            "ctx_command_summary": ctx_command_summary(args.ctx_command),
            "has_data_root": bool(args.data_root),
            "has_cwd": bool(args.cwd),
            "has_sidecar": bool(args.sidecar),
            "backend": args.backend,
            "refresh": args.refresh,
            "limit": args.limit,
            "polls": args.polls,
            "poll_interval": args.poll_interval,
        },
        "query_hash": stable_hash(args.query),
        "query_chars": len(args.query),
        "status_before": status_before,
        "first_search": first_search,
        "first_search_wall_ms": first_search["command"]["wall_ms"],
        "status_samples": status_samples,
        "incremental": incremental,
        "sidecar_bytes_final": sidecar_file_bytes(sidecar_path),
    }
    private_values = private_values_from_args(args)
    validate_private_output(
        metrics,
        raw_queries=private_values["queries"],
        raw_paths=private_values["paths"],
    )
    output = json.dumps(metrics, indent=2)
    if args.output:
        pathlib.Path(args.output).write_text(output + "\n", encoding="utf-8")
    else:
        print(output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
