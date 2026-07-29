#!/usr/bin/env bash

perf_smoke_emit_python_report() {
  cat <<'PY'
def comparison_report(
    order: str,
    baseline: dict[str, object],
    candidate: dict[str, object],
    allowed_regression_pct: float,
    max_peak_rss_bytes: float,
) -> dict[str, object]:
    if baseline.get("role") != "baseline":
        raise HarnessError(f"{order} comparison is missing its baseline role")
    if candidate.get("role") != "candidate":
        raise HarnessError(f"{order} comparison is missing its candidate role")

    checks: list[dict[str, object]] = []
    for phase in CORE_PHASES:
        baseline_wall = lookup_metric(baseline, phase, "timings", "p95_ms")
        candidate_wall = lookup_metric(candidate, phase, "timings", "p95_ms")
        baseline_write = lookup_resource(baseline, phase, "device_write_bytes", "p95")
        candidate_write = lookup_resource(candidate, phase, "device_write_bytes", "p95")
        checks.append(
            regression_check(
                f"{phase}_wall_p95",
                baseline_wall,
                candidate_wall,
                allowed_regression_pct,
                "ms",
            )
        )
        checks.append(
            regression_check(
                f"{phase}_device_write_p95",
                baseline_write,
                candidate_write,
                allowed_regression_pct,
                "bytes",
            )
        )
        checks.append(
            absolute_limit_check(
                f"{phase}_peak_rss_max",
                lookup_resource(baseline, phase, "peak_rss_bytes", "max"),
                lookup_resource(candidate, phase, "peak_rss_bytes", "max"),
                max_peak_rss_bytes,
                "bytes",
            )
        )
        checks.extend(
            [
                ratio_check(
                    f"{phase}_wall_parity",
                    baseline_wall,
                    candidate_wall,
                    WALL_PARITY_RATIO,
                    "ms",
                    "candidate_wall_at_or_below_baseline",
                    inclusive=True,
                ),
                ratio_check(
                    f"{phase}_cpu_total_relative",
                    lookup_resource(baseline, phase, "cpu_total_ms", "p95"),
                    lookup_resource(candidate, phase, "cpu_total_ms", "p95"),
                    CPU_RSS_RELATIVE_RATIO,
                    "ms",
                    "candidate_cpu_relative_gate",
                    inclusive=True,
                ),
                ratio_check(
                    f"{phase}_peak_rss_relative",
                    lookup_resource(baseline, phase, "peak_rss_bytes", "max"),
                    lookup_resource(candidate, phase, "peak_rss_bytes", "max"),
                    CPU_RSS_RELATIVE_RATIO,
                    "bytes",
                    "candidate_rss_relative_gate",
                    inclusive=True,
                ),
                ratio_check(
                    f"{phase}_device_read_amplification",
                    lookup_resource(baseline, phase, "device_read_bytes", "p95"),
                    lookup_resource(candidate, phase, "device_read_bytes", "p95"),
                    IO_AMPLIFICATION_RATIO,
                    "bytes",
                    "device_read_below_1.73x_baseline",
                    inclusive=False,
                ),
                ratio_check(
                    f"{phase}_device_write_parity",
                    baseline_write,
                    candidate_write,
                    DEVICE_WRITE_PARITY_RATIO,
                    "bytes",
                    "candidate_device_write_at_or_below_baseline",
                    inclusive=True,
                ),
                ratio_check(
                    f"{phase}_device_write_amplification",
                    baseline_write,
                    candidate_write,
                    IO_AMPLIFICATION_RATIO,
                    "bytes",
                    "device_write_below_1.73x_baseline",
                    inclusive=False,
                ),
                ratio_check(
                    f"{phase}_device_total_io_amplification",
                    lookup_resource(baseline, phase, "device_total_io_bytes", "p95"),
                    lookup_resource(candidate, phase, "device_total_io_bytes", "p95"),
                    IO_AMPLIFICATION_RATIO,
                    "bytes",
                    "device_total_io_below_1.73x_baseline",
                    inclusive=False,
                ),
            ]
        )

    baseline_query_timings = baseline["profiles"]["concurrent_refresh_off_search"]["query"][
        "timings"
    ]
    candidate_query_timings = candidate["profiles"]["concurrent_refresh_off_search"]["query"][
        "timings"
    ]
    if baseline_query_timings is not None and candidate_query_timings is not None:
        baseline_query_wall = float(baseline_query_timings["p95_ms"])
        candidate_query_wall = float(candidate_query_timings["p95_ms"])
        checks.append(
            regression_check(
                "concurrent_refresh_off_search_wall_p95",
                baseline_query_wall,
                candidate_query_wall,
                allowed_regression_pct,
                "ms",
            )
        )
        checks.append(
            ratio_check(
                "concurrent_refresh_off_search_wall_parity",
                baseline_query_wall,
                candidate_query_wall,
                WALL_PARITY_RATIO,
                "ms",
                "candidate_wall_at_or_below_baseline",
                inclusive=True,
            )
        )
    else:
        for name, policy in [
            ("concurrent_refresh_off_search_wall_p95", "legacy_relative_regression"),
            (
                "concurrent_refresh_off_search_wall_parity",
                "candidate_wall_at_or_below_baseline",
            ),
        ]:
            checks.append({
                "name": name,
                "policy": policy,
                "unit": "ms",
                "baseline": None,
                "candidate": None,
                "head": None,
                "change_pct": None,
                "threshold": None,
                "allowed_regression_pct": allowed_regression_pct,
                "passed": False,
                "reason": "one or both runs collected no concurrent query sample",
            })

    informational: list[dict[str, object]] = []
    for phase in CORE_PHASES:
        for resource, statistic, unit in [
            ("cpu_total_ms", "p95", "ms"),
            ("peak_rss_bytes", "max", "bytes"),
            ("filesystem_read_chars", "p95", "characters"),
            ("filesystem_write_chars", "p95", "characters"),
            ("device_read_bytes", "p95", "bytes"),
            ("device_write_bytes", "p95", "bytes"),
            ("device_total_io_bytes", "p95", "bytes"),
            ("source_backed_storage_high_water_bytes", "max", "bytes"),
            ("source_backed_storage_growth_high_water_bytes", "p95", "bytes"),
        ]:
            baseline_value = lookup_resource(baseline, phase, resource, statistic)
            candidate_value = lookup_resource(candidate, phase, resource, statistic)
            informational.append(
                {
                    "name": f"{phase}_{resource}_{statistic}",
                    "unit": unit,
                    "baseline": round2(baseline_value),
                    "candidate": round2(candidate_value),
                    "head": round2(candidate_value),
                    "change_pct": percent_change(baseline_value, candidate_value),
                }
            )
    for check in checks:
        check["comparison_order"] = order
    for metric in informational:
        metric["comparison_order"] = order

    return {
        "comparison_order": order,
        "baseline_label": baseline["label"],
        "candidate_label": candidate["label"],
        "head_label": candidate["label"],
        "allowed_regression_pct": allowed_regression_pct,
        "cpu_rss_relative_ratio": CPU_RSS_RELATIVE_RATIO,
        "wall_parity_ratio": WALL_PARITY_RATIO,
        "device_write_parity_ratio": DEVICE_WRITE_PARITY_RATIO,
        "io_amplification_ratio_exclusive": IO_AMPLIFICATION_RATIO,
        "max_peak_rss_bytes": round2(max_peak_rss_bytes),
        "status": "passed" if all(bool(check["passed"]) for check in checks) else "failed",
        "checks": checks,
        "informational_metrics": informational,
    }


PY
}
