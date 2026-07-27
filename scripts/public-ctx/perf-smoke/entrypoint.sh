#!/usr/bin/env bash

perf_smoke_emit_python_entrypoint() {
  cat <<'PY'
def selected_comparison_orders(selector: str, enforce: bool) -> list[str]:
    normalized = "candidate-first" if selector == "head-first" else selector
    if normalized == "both":
        return ["baseline-first", "candidate-first"]
    if normalized not in {"baseline-first", "candidate-first"}:
        raise HarnessError(
            "CTX_PERF_SMOKE_COMPARISON_ORDER must be both, baseline-first, "
            "candidate-first, or the head-first alias"
        )
    if enforce:
        raise HarnessError(
            "enforced baseline/candidate comparisons require both execution orders; "
            "set CTX_PERF_SMOKE_COMPARISON_ORDER=both"
        )
    return [normalized]


def execution_specs_for_order(order: str) -> list[tuple[Path, str, str]]:
    if order == "baseline-first":
        return list(RUN_SPECS)
    if order == "candidate-first":
        return list(reversed(RUN_SPECS))
    raise HarnessError(f"unknown comparison order: {order}")


def main() -> int:
    if RUN_MODE not in {"single", "comparison"}:
        raise HarnessError(f"unknown run mode: {RUN_MODE}")
    if not Path("/proc/self/io").is_file():
        raise HarnessError("this resource profile requires Linux /proc process I/O accounting")
    labels = [label for _, label, _ in RUN_SPECS]
    if len(labels) != len(set(labels)):
        raise HarnessError("performance run labels must be distinct")
    enforce = env_flag("CTX_PERF_SMOKE_ENFORCE", True)
    binary_hash_binding = preflight_binary_hashes(RUN_MODE, enforce, RUN_SPECS)
    hash_binding_by_role = {
        str(binding["role"]): binding for binding in binary_hash_binding["bindings"]
    }

    sessions = env_int("CTX_PERF_SMOKE_SESSIONS", 2000, 1, 10_000)
    large_session_events = env_int(
        "CTX_PERF_SMOKE_LARGE_SESSION_EVENTS", 4096, 65, 65_536
    )
    initial_repeats = env_int("CTX_PERF_SMOKE_INITIAL_REPEATS", 3, 1, 5)
    repeats = env_int("CTX_PERF_SMOKE_REPEATS", 5, 1, 10)
    changed_files = min(env_int("CTX_PERF_SMOKE_CHANGED_FILES", 5), sessions)
    concurrent_queries = env_int("CTX_PERF_SMOKE_CONCURRENT_QUERIES", 5, 1, 20)
    concurrent_interval_ms = env_int(
        "CTX_PERF_SMOKE_CONCURRENT_QUERY_INTERVAL_MS", 10, 0, 1_000
    )
    sampling_interval_ms = env_int("CTX_PERF_SMOKE_SAMPLING_INTERVAL_MS", 5, 1, 1_000)
    command_timeout_seconds = env_float(
        "CTX_PERF_SMOKE_COMMAND_TIMEOUT_SECONDS", 300.0, 1.0, 900.0
    )
    total_timeout_seconds = env_float(
        "CTX_PERF_SMOKE_TOTAL_TIMEOUT_SECONDS", 1800.0, 1.0, 7_200.0
    )
    require_concurrency = env_flag("CTX_PERF_SMOKE_REQUIRE_CONCURRENCY", True)
    allowed_regression_pct = env_float("CTX_PERF_SMOKE_REGRESSION_PCT", 10.0)
    max_peak_rss_bytes = (
        env_float("CTX_PERF_SMOKE_MAX_PEAK_RSS_MIB", 1024.0, 1.0, 1024.0)
        * 1024.0
        * 1024.0
    )
    max_wal_bytes = (
        env_float("CTX_PERF_SMOKE_MAX_WAL_MIB", 64.0, 1.0, 64.0)
        * 1024.0
        * 1024.0
    )

    comparison_selector = os.environ.get(
        "CTX_PERF_SMOKE_COMPARISON_ORDER", "both"
    ).strip()
    comparison_orders = (
        selected_comparison_orders(comparison_selector, enforce)
        if RUN_MODE == "comparison"
        else ["single"]
    )

    work_base = Path(
        os.environ.get("CTX_PERF_SMOKE_WORK_DIR", SIDECAR_ROOT / "target" / "ctx-perf-smoke")
    )
    work_root = prepare_work_root(work_base)
    default_artifact_dir = Path(
        os.environ.get(
            "TEST_UNDECLARED_OUTPUTS_DIR",
            SIDECAR_ROOT / "target" / "ctx-artifacts" / "perf-smoke",
        )
    )
    artifact_path = Path(
        os.environ.get(
            "CTX_PERF_SMOKE_ARTIFACT",
            default_artifact_dir / "ctx-cli-perf-smoke.json",
        )
    )
    thresholds = {
        "status_p95_ms": env_float("CTX_PERF_SMOKE_STATUS_P95_MS", 750.0),
        "search_p95_ms": env_float("CTX_PERF_SMOKE_SEARCH_P95_MS", 2500.0),
        "import_noop_p95_ms": env_float("CTX_PERF_SMOKE_IMPORT_NOOP_P95_MS", 2500.0),
        "import_changed_p95_ms": env_float("CTX_PERF_SMOKE_IMPORT_CHANGED_P95_MS", 3000.0),
        "import_replacement_p95_ms": env_float(
            "CTX_PERF_SMOKE_IMPORT_REPLACEMENT_P95_MS", 3500.0
        ),
        "concurrent_search_p95_ms": env_float(
            "CTX_PERF_SMOKE_CONCURRENT_SEARCH_P95_MS", 2500.0
        ),
        "show_session_p95_ms": env_float("CTX_PERF_SMOKE_SHOW_SESSION_P95_MS", 1500.0),
    }

    execution_orders: list[dict[str, object]] = []
    runs: list[dict[str, object]] = []
    order_reports: list[dict[str, object]] = []
    run_index = 0
    if RUN_MODE == "single":
        ctx_bin, label, role = RUN_SPECS[0]
        run = run_one(
            ctx_bin,
            label,
            role,
            hash_binding_by_role[role],
            "single",
            0,
            run_index,
            work_root,
            sessions,
            large_session_events,
            initial_repeats,
            repeats,
            changed_files,
            concurrent_queries,
            concurrent_interval_ms,
            require_concurrency,
            sampling_interval_ms,
            thresholds,
        )
        runs.append(run)
        execution_orders.append(
            {
                "comparison_order": "single",
                "execution_sequence": [run["role"]],
                "run_ids": [run["run_id"]],
                "status": run["status"],
                "comparison": None,
            }
        )
    else:
        for order in comparison_orders:
            order_runs: list[dict[str, object]] = []
            execution_specs = execution_specs_for_order(order)
            for position, (ctx_bin, label, role) in enumerate(execution_specs):
                run = run_one(
                    ctx_bin,
                    label,
                    role,
                    hash_binding_by_role[role],
                    order,
                    position,
                    run_index,
                    work_root,
                    sessions,
                    large_session_events,
                    initial_repeats,
                    repeats,
                    changed_files,
                    concurrent_queries,
                    concurrent_interval_ms,
                    require_concurrency,
                    sampling_interval_ms,
                    thresholds,
                )
                run_index += 1
                order_runs.append(run)
                runs.append(run)
            runs_by_role = {str(run["role"]): run for run in order_runs}
            report = comparison_report(
                order,
                runs_by_role["baseline-v0.25"],
                runs_by_role["candidate"],
                allowed_regression_pct,
                max_peak_rss_bytes,
                max_wal_bytes,
            )
            order_passed = (
                all(run["status"] == "passed" for run in order_runs)
                and report["status"] == "passed"
            )
            order_reports.append(report)
            execution_orders.append(
                {
                    "comparison_order": order,
                    "execution_sequence": [role for _, _, role in execution_specs],
                    "run_ids": [run["run_id"] for run in order_runs],
                    "status": "passed" if order_passed else "failed",
                    "comparison": report,
                }
            )

    comparison = None
    if RUN_MODE == "comparison":
        comparison_checks = [
            check for report in order_reports for check in report["checks"]
        ]
        comparison = {
            "status": (
                "passed"
                if all(report["status"] == "passed" for report in order_reports)
                else "failed"
            ),
            "required_orders": ["baseline-first", "candidate-first"],
            "observed_orders": comparison_orders,
            "baseline_label": RUN_SPECS[0][1],
            "candidate_label": RUN_SPECS[1][1],
            "hard_policy": {
                "wall_ratio_max_inclusive": WALL_PARITY_RATIO,
                "device_write_ratio_max_inclusive": DEVICE_WRITE_PARITY_RATIO,
                "max_peak_rss_bytes": round2(max_peak_rss_bytes),
                "max_wal_bytes": round2(max_wal_bytes),
                "io_amplification_ratio_max_exclusive": IO_AMPLIFICATION_RATIO,
                "cpu_rss_ratio_max_inclusive": CPU_RSS_RELATIVE_RATIO,
            },
            "checks": comparison_checks,
            "orders": order_reports,
        }

    run_checks_passed = all(run["status"] == "passed" for run in runs)
    comparison_passed = comparison is None or comparison["status"] == "passed"
    passed = run_checks_passed and comparison_passed
    artifact = {
        "schema_version": 4,
        "profile": "ctx-cli-perf-smoke",
        "status": "passed" if passed else "failed",
        "enforced": enforce,
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z"),
        "mode": RUN_MODE,
        "binary_hash_binding": binary_hash_binding,
        "work_root": str(work_root),
        "configuration": {
            "sessions": sessions,
            "large_session_events": large_session_events,
            "initial_repeats": initial_repeats,
            "repeats": repeats,
            "changed_files_per_sample": changed_files,
            "concurrent_queries": concurrent_queries,
            "concurrent_query_interval_ms": concurrent_interval_ms,
            "require_concurrency": require_concurrency,
            "sampling_interval_ms": sampling_interval_ms,
            "command_timeout_seconds": command_timeout_seconds,
            "total_timeout_seconds": total_timeout_seconds,
            "comparison_order": comparison_selector,
            "comparison_orders": comparison_orders,
        },
        "thresholds": {
            **thresholds,
            "allowed_regression_pct": allowed_regression_pct,
            "wall_parity_ratio": WALL_PARITY_RATIO,
            "device_write_parity_ratio": DEVICE_WRITE_PARITY_RATIO,
            "cpu_rss_relative_ratio": CPU_RSS_RELATIVE_RATIO,
            "io_amplification_ratio_exclusive": IO_AMPLIFICATION_RATIO,
            "max_peak_rss_bytes": round2(max_peak_rss_bytes),
            "max_wal_bytes": round2(max_wal_bytes),
            "env_overrides": [
                "CTX_PERF_SMOKE_STATUS_P95_MS",
                "CTX_PERF_SMOKE_SEARCH_P95_MS",
                "CTX_PERF_SMOKE_IMPORT_NOOP_P95_MS",
                "CTX_PERF_SMOKE_IMPORT_CHANGED_P95_MS",
                "CTX_PERF_SMOKE_IMPORT_REPLACEMENT_P95_MS",
                "CTX_PERF_SMOKE_CONCURRENT_SEARCH_P95_MS",
                "CTX_PERF_SMOKE_SHOW_SESSION_P95_MS",
                "CTX_PERF_SMOKE_REGRESSION_PCT",
                "CTX_PERF_SMOKE_MAX_PEAK_RSS_MIB",
                "CTX_PERF_SMOKE_MAX_WAL_MIB",
            ],
        },
        "measurement_contract": {
            "baseline_identity": (
                f"comparison baseline --version must equal {EXACT_BASELINE_VERSION!r}; "
                "enforced comparison also binds baseline and candidate paths to explicit "
                "expected SHA-256 values before any binary or corpus execution"
            ),
            "append_oracles": (
                "v0.25 reports one imported session per appended source; the candidate "
                "reports appended events without re-importing sessions"
            ),
            "wall_clock": "Python time.perf_counter around one ctx process",
            "cpu_and_rss": "wait4 child rusage; Linux ru_maxrss converted from KiB to bytes",
            "filesystem_io_proxy": "Linux /proc/<pid>/io rchar and wchar for the ctx process",
            "device_io_proxy": (
                "Linux /proc/<pid>/io read_bytes and write_bytes for the ctx process; "
                "total I/O is their per-process sum before percentile aggregation"
            ),
            "block_io_fallback": "wait4 ru_inblock/ru_oublock multiplied by 512 bytes",
            "wal_high_water": (
                "maximum observed work.sqlite-wal size at the configured polling interval; "
                "a shorter transient can be missed"
            ),
            "scope_limits": (
                "process counters include binary, SQLite, and source activity and do not isolate "
                "provider-source reads; /proc counters exclude work performed by separately spawned "
                "descendants"
            ),
            "comparison_order_note": (
                "enforced comparisons execute baseline-first and candidate-first sequentially "
                "with fresh HOME, CTX_DATA_ROOT, temp, and corpus roots for every run"
            ),
        },
        "runs": runs,
        "execution_orders": execution_orders,
        "comparison": comparison,
    }

    artifact_path.parent.mkdir(parents=True, exist_ok=True)
    artifact_path.write_text(json.dumps(artifact, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    print(f"ctx perf smoke artifact: {artifact_path}")
    print(f"ctx perf smoke status: {'passed' if passed else 'failed'}")
    runs_by_id = {run["run_id"]: run for run in runs}
    for order_result in execution_orders:
        print(
            "ctx perf smoke order: "
            f"{order_result['comparison_order']} status={order_result['status']} "
            f"sequence={','.join(order_result['execution_sequence'])}"
        )
        for run_id in order_result["run_ids"]:
            run = runs_by_id[run_id]
            print(
                f"ctx perf smoke run: {run['label']} role={run['role']} "
                f"status={run['status']}"
            )
            for check in run["checks"]:
                mark = "ok" if check["passed"] else "fail"
                print(
                    f"{mark}: {run['label']} {check['name']} "
                    f"actual={check['actual']}ms threshold={check['threshold']}ms"
                )
    if comparison is not None:
        for check in comparison["checks"]:
            mark = "ok" if check["passed"] else "fail"
            print(
                f"{mark}: comparison order={check['comparison_order']} {check['name']} "
                f"baseline={check['baseline']} candidate={check['candidate']} {check['unit']}"
            )

    if enforce and not passed:
        return 1
    return 0


try:
    raise SystemExit(main())
except HarnessError as exc:
    print(f"perf smoke failed: {exc}", file=sys.stderr)
    raise SystemExit(1)
PY
}
