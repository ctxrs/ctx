#!/usr/bin/env python3
"""Aggregate local ctx search dogfood reports into a thresholded soak summary.

Inputs are private local reports. Output is designed to be private-safe: it
contains aggregate counts, timings, retrieval mode distributions, and gate
results, but not query text, snippets, paths, UUIDs, provider IDs, or ctx IDs.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import re
import statistics
import sys
from typing import Any


UUID_RE = re.compile(
    r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b"
)
PATH_RE = re.compile(r"(?<![A-Za-z0-9_.-])(?:/home/|/Users/|/[A-Za-z0-9_.-]+/)")


DEFAULT_THRESHOLDS: dict[str, Any] = {
    "reports": {
        "require_basics": True,
        "require_status": True,
        "require_private_eval": False,
    },
    "basics": {
        "require_ok": True,
        "require_refresh_background": True,
        "hybrid_p95_vs_lexical_max_ratio": 2.0,
        "max_hybrid_p95_ms": 5000,
        "require_hybrid_fallback_lexical": True,
        "require_hybrid_fallback_safe": True,
    },
    "private_eval": {
        "baseline": "lexical",
        "candidate": "hybrid",
        "min_hit5_delta": 0.0,
        "min_mrr_delta": 0.0,
        "max_p95_ratio": 2.0,
    },
    "status": {
        "require_semantic_dirty_zero": True,
        "min_semantic_coverage_ratio": 1.0,
        "require_semantic_model_cache": True,
        "require_history_refresh_not_failed": True,
    },
}


def utc_now() -> str:
    return dt.datetime.now(dt.UTC).isoformat().replace("+00:00", "Z")


def deep_merge(base: dict[str, Any], override: dict[str, Any]) -> dict[str, Any]:
    merged = dict(base)
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def load_json(path: str | None) -> Any:
    if not path:
        return None
    expanded = pathlib.Path(path).expanduser()
    try:
        text = expanded.read_text(encoding="utf-8")
    except OSError as error:
        raise SystemExit(f"failed to read {expanded}: {error}") from error
    if not text.strip():
        raise SystemExit(f"report is empty: {expanded}")
    try:
        return json.loads(text)
    except json.JSONDecodeError as error:
        raise SystemExit(f"failed to parse JSON from {expanded}: {error}") from error


def load_thresholds(path: str | None) -> dict[str, Any]:
    if not path:
        return DEFAULT_THRESHOLDS
    return deep_merge(DEFAULT_THRESHOLDS, load_json(path))


def as_number(value: Any) -> float | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return float(value)
    return None


def ratio(numerator: float | None, denominator: float | None) -> float | None:
    if numerator is None or denominator is None or denominator <= 0:
        return None
    return numerator / denominator


def percentile(values: list[float], pct: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, int((len(ordered) - 1) * pct / 100 + 0.999))
    return ordered[index]


def add_gate(gates: list[dict[str, Any]], name: str, passed: bool, detail: str) -> None:
    gates.append({"name": name, "passed": bool(passed), "detail": detail})


def basics_summary(report: dict[str, Any], thresholds: dict[str, Any]) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    gates: list[dict[str, Any]] = []
    summary = report.get("summary", {}) if isinstance(report, dict) else {}
    by_mode = summary.get("by_mode", {}) if isinstance(summary, dict) else {}
    lexical = by_mode.get("lexical", {}) if isinstance(by_mode, dict) else {}
    hybrid = by_mode.get("hybrid", {}) if isinstance(by_mode, dict) else {}
    lexical_p95 = as_number(lexical.get("p95_ms"))
    hybrid_p95 = as_number(hybrid.get("p95_ms"))
    p95_ratio = ratio(hybrid_p95, lexical_p95)

    require_ok = bool(thresholds["basics"].get("require_ok", True))
    if require_ok:
        add_gate(
            gates,
            "basics_ok",
            bool(summary.get("ok")),
            "default experience gate summary ok",
        )
    if thresholds["basics"].get("require_refresh_background"):
        add_gate(
            gates,
            "basics_refresh_background",
            report.get("refresh") == "background",
            f"basics report refresh {report.get('refresh')}",
        )

    max_ratio = thresholds["basics"].get("hybrid_p95_vs_lexical_max_ratio")
    if max_ratio is not None:
        add_gate(
            gates,
            "basics_hybrid_p95_ratio",
            p95_ratio is not None and p95_ratio <= float(max_ratio),
            f"hybrid/lexical p95 ratio {p95_ratio}",
        )

    max_hybrid_p95 = thresholds["basics"].get("max_hybrid_p95_ms")
    if max_hybrid_p95 is not None:
        add_gate(
            gates,
            "basics_hybrid_p95_ms",
            hybrid_p95 is not None and hybrid_p95 <= float(max_hybrid_p95),
            f"hybrid p95 {hybrid_p95} ms",
        )

    mode_counts: dict[str, dict[str, int]] = {}
    fallback_counts: dict[str, int] = {}
    hybrid_fallback_runs = []
    for query in report.get("queries", []) if isinstance(report, dict) else []:
        for run in query.get("runs", []) if isinstance(query, dict) else []:
            retrieval = run.get("retrieval", {}) if isinstance(run, dict) else {}
            if not isinstance(retrieval, dict):
                continue
            requested = str(retrieval.get("requested_mode", "unknown"))
            effective = str(retrieval.get("effective_mode", "unknown"))
            status = str(retrieval.get("semantic_status", "unknown"))
            mode_counts.setdefault(requested, {})
            mode_counts[requested][effective] = mode_counts[requested].get(effective, 0) + 1
            fallback = retrieval.get("semantic_fallback_code") or retrieval.get("semantic_fallback")
            if isinstance(fallback, str):
                fallback_counts[fallback] = fallback_counts.get(fallback, 0) + 1
            if requested == "hybrid" and status in {"partial", "unavailable", "skipped"}:
                coverage = retrieval.get("coverage")
                hybrid_fallback_runs.append(
                    {
                        "effective": effective,
                        "dirty_items": as_number(
                            coverage.get("dirty_items")
                            if isinstance(coverage, dict)
                            else None
                        ),
                    }
                )

    if thresholds["basics"].get("require_hybrid_fallback_lexical"):
        add_gate(
            gates,
            "hybrid_fallback_lexical",
            all(run["effective"] == "lexical" for run in hybrid_fallback_runs),
            "partial/unavailable hybrid runs must fall back to lexical",
        )
    if thresholds["basics"].get("require_hybrid_fallback_safe"):
        def hybrid_fallback_safe(run: dict[str, Any]) -> bool:
            if run["effective"] == "lexical":
                return True
            return run["effective"] == "hybrid" and run["dirty_items"] == 0

        add_gate(
            gates,
            "hybrid_fallback_safe",
            all(hybrid_fallback_safe(run) for run in hybrid_fallback_runs),
            "partial/unavailable hybrid must be lexical or hybrid with zero dirty items",
        )

    safe = {
        "ok": bool(summary.get("ok")),
        "total_runs": summary.get("total_runs", 0),
        "command_failure_count": len(summary.get("command_failures", [])),
        "expected_failure_count": len(summary.get("expected_failures", [])),
        "latency_by_mode": by_mode,
        "hybrid_p95_vs_lexical_p95": p95_ratio,
        "effective_mode_counts": mode_counts,
        "fallback_counts": fallback_counts,
    }
    return safe, gates


def private_eval_summary(report: dict[str, Any], thresholds: dict[str, Any]) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    gates: list[dict[str, Any]] = []
    summary = report.get("summary", {}) if isinstance(report, dict) else {}
    baseline_name = thresholds["private_eval"].get("baseline", "lexical")
    candidate_name = thresholds["private_eval"].get("candidate", "hybrid")
    baseline = summary.get(baseline_name, {}) if isinstance(summary, dict) else {}
    candidate = summary.get(candidate_name, {}) if isinstance(summary, dict) else {}

    hit5_delta = as_number(candidate.get("hit5"))
    base_hit5 = as_number(baseline.get("hit5"))
    if hit5_delta is not None and base_hit5 is not None:
        hit5_delta -= base_hit5
    mrr_delta = as_number(candidate.get("mrr"))
    base_mrr = as_number(baseline.get("mrr"))
    if mrr_delta is not None and base_mrr is not None:
        mrr_delta -= base_mrr
    p95_ratio = ratio(as_number(candidate.get("p95_ms")), as_number(baseline.get("p95_ms")))

    add_gate(
        gates,
        "private_eval_hit5_delta",
        hit5_delta is not None
        and hit5_delta >= float(thresholds["private_eval"].get("min_hit5_delta", 0.0)),
        f"{candidate_name} hit5 delta {hit5_delta}",
    )
    add_gate(
        gates,
        "private_eval_mrr_delta",
        mrr_delta is not None
        and mrr_delta >= float(thresholds["private_eval"].get("min_mrr_delta", 0.0)),
        f"{candidate_name} mrr delta {mrr_delta}",
    )
    add_gate(
        gates,
        "private_eval_p95_ratio",
        p95_ratio is not None
        and p95_ratio <= float(thresholds["private_eval"].get("max_p95_ratio", 2.0)),
        f"{candidate_name}/{baseline_name} p95 ratio {p95_ratio}",
    )

    return {
        "baseline": baseline_name,
        "candidate": candidate_name,
        "hit5_delta": hit5_delta,
        "mrr_delta": mrr_delta,
        "p95_ratio": p95_ratio,
        "summary_by_backend": summary,
    }, gates


def status_semantic(status: dict[str, Any]) -> dict[str, Any]:
    semantic = status.get("semantic") if isinstance(status, dict) else None
    if isinstance(semantic, dict):
        return semantic
    return {}


def status_summary(status: dict[str, Any], thresholds: dict[str, Any]) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    gates: list[dict[str, Any]] = []
    semantic = status_semantic(status)
    daemon = status.get("daemon") if isinstance(status, dict) else {}
    daemon_jobs = daemon.get("jobs") if isinstance(daemon, dict) else {}
    history_refresh = daemon_jobs.get("history_refresh") if isinstance(daemon_jobs, dict) else {}
    coverage = semantic.get("coverage") if isinstance(semantic.get("coverage"), dict) else {}
    searchable = as_number(coverage.get("searchable_items"))
    embedded = as_number(coverage.get("embedded_items"))
    dirty = as_number(coverage.get("dirty_items"))
    coverage_ratio = ratio(embedded, searchable)
    min_ratio = thresholds["status"].get("min_semantic_coverage_ratio")
    if min_ratio is not None:
        add_gate(
            gates,
            "status_min_semantic_coverage",
            coverage_ratio is not None and coverage_ratio >= float(min_ratio),
            f"semantic coverage ratio {coverage_ratio}",
        )
    if thresholds["status"].get("require_semantic_dirty_zero"):
        add_gate(
            gates,
            "status_semantic_dirty_zero",
            dirty == 0,
            f"semantic dirty items {dirty}",
        )
    if thresholds["status"].get("require_semantic_model_cache"):
        model_cache_available = semantic.get("model_cache_available")
        add_gate(
            gates,
            "status_semantic_model_cache",
            model_cache_available is True,
            f"semantic model cache available {model_cache_available}",
        )
    if thresholds["status"].get("require_history_refresh_not_failed"):
        history_status = history_refresh.get("status") if isinstance(history_refresh, dict) else None
        add_gate(
            gates,
            "status_history_refresh_not_failed",
            history_status not in {None, "failed"},
            f"history refresh status {history_status}",
        )
    return {
        "semantic_status": semantic.get("status"),
        "semantic_running": semantic.get("running"),
        "coverage_ratio": coverage_ratio,
        "searchable_items": searchable,
        "embedded_items": embedded,
        "dirty_items": dirty,
        "model_cache_available": semantic.get("model_cache_available"),
        "daemon_status": daemon.get("status") if isinstance(daemon, dict) else None,
        "history_refresh_status": history_refresh.get("status") if isinstance(history_refresh, dict) else None,
        "semantic_index_status": (
            daemon_jobs.get("semantic_index", {}).get("status")
            if isinstance(daemon_jobs, dict) and isinstance(daemon_jobs.get("semantic_index"), dict)
            else None
        ),
    }, gates


def validate_safe_output(payload: dict[str, Any]) -> list[str]:
    serialized = json.dumps(payload, sort_keys=True)
    issues = []
    if UUID_RE.search(serialized):
        issues.append("summary contains UUID-shaped text")
    if PATH_RE.search(serialized):
        issues.append("summary contains absolute-path-shaped text")
    return issues


def write_json(path: str, payload: dict[str, Any]) -> None:
    output = pathlib.Path(path).expanduser()
    output.parent.mkdir(parents=True, exist_ok=True)
    body = json.dumps(payload, indent=2, sort_keys=True) + "\n"
    fd = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(body)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--basics-report")
    parser.add_argument("--private-eval")
    parser.add_argument("--status-json")
    parser.add_argument("--thresholds")
    parser.add_argument("--output", required=True)
    parser.add_argument("--no-fail", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    thresholds = load_thresholds(args.thresholds)
    gates: list[dict[str, Any]] = []
    reports: dict[str, Any] = {}
    required = thresholds.get("reports", {})

    basics = load_json(args.basics_report)
    if basics is not None:
        reports["basics"], basics_gates = basics_summary(basics, thresholds)
        gates.extend(basics_gates)
    elif required.get("require_basics"):
        add_gate(gates, "basics_report_present", False, "missing --basics-report")

    private_eval = load_json(args.private_eval)
    if private_eval is not None:
        reports["private_eval"], eval_gates = private_eval_summary(private_eval, thresholds)
        gates.extend(eval_gates)
    elif required.get("require_private_eval"):
        add_gate(gates, "private_eval_present", False, "missing --private-eval")

    status = load_json(args.status_json)
    if status is not None:
        reports["status"], status_gates = status_summary(status, thresholds)
        gates.extend(status_gates)
    elif required.get("require_status"):
        add_gate(gates, "status_report_present", False, "missing --status-json")

    if not reports and not gates:
        raise SystemExit("provide at least one report: --basics-report, --private-eval, or --status-json")

    payload = {
        "schema_version": 1,
        "generated_at": utc_now(),
        "local_only": True,
        "private_safe_summary": True,
        "reports_present": sorted(reports),
        "reports": reports,
        "gates": gates,
        "ok": all(gate["passed"] for gate in gates),
    }
    privacy_issues = validate_safe_output(payload)
    if privacy_issues:
        payload["privacy_issues"] = privacy_issues
        payload["ok"] = False

    write_json(args.output, payload)
    print(
        f"wrote {pathlib.Path(args.output).expanduser()}: "
        f"{sum(1 for gate in gates if gate['passed'])}/{len(gates)} gates passed"
    )
    if payload["ok"] or args.no_fail:
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
