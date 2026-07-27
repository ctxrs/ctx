#!/usr/bin/env bash

perf_smoke_emit_python_runner() {
  cat <<'PY'
def run_concurrent_query_profile(
    ctx_bin: Path,
    corpus_root: Path,
    search_args: list[str],
    query_samples: int,
    query_interval_ms: int,
    require_concurrency: bool,
    env: dict[str, str],
    data_root: Path,
    sampling_interval_ms: int,
) -> tuple[dict[str, object], dict[str, object]]:
    importer = start_ctx(
        ctx_bin,
        import_command(corpus_root, resume=True),
        env,
        data_root,
        sampling_interval_ms,
    )
    query_results: list[dict[str, object]] = []
    active_before_samples: list[bool] = []
    try:
        for _ in range(query_samples):
            active_before = process_is_active(importer.process.pid)
            if not active_before:
                break
            result = run_ctx(ctx_bin, search_args, env, data_root, sampling_interval_ms)
            packet = result["packet"]
            if not isinstance(packet.get("results"), list) or not packet["results"]:
                raise HarnessError(f"concurrent refresh-off search returned no results: {packet}")
            query_results.append(result)
            active_before_samples.append(active_before)
            if query_interval_ms:
                time.sleep(query_interval_ms / 1000.0)
    except BaseException:
        if process_is_active(importer.process.pid):
            importer.process.terminate()
        importer.finish()
        raise

    import_result = finish_ctx(importer)
    import_summary = profile_summary(import_result["packet"])
    expect_exact_import_delta(import_summary, "concurrent idempotent rescan", 0, 0)
    if require_concurrency and not query_results:
        raise HarnessError(
            "the rescan finished before a concurrent query sample started; increase "
            "CTX_PERF_SMOKE_SESSIONS or disable CTX_PERF_SMOKE_REQUIRE_CONCURRENCY"
        )
    if query_results:
        query_profile = command_profile(ctx_bin, search_args, query_results)
    else:
        query_profile = {
            "command": command_string(ctx_bin, search_args),
            "timings": None,
            "resources": None,
        }
    return {
        "requested_samples": query_samples,
        "collected_samples": len(query_results),
        "import_active_before_each_sample": active_before_samples,
        "query": query_profile,
        "rescan_import": {
            **command_profile(ctx_bin, import_command(corpus_root, resume=True), [import_result]),
            "totals": import_summary,
        },
    }, import_result["packet"]


def run_one(
    ctx_bin: Path,
    label: str,
    role: str,
    hash_binding: dict[str, object],
    comparison_order: str,
    execution_position: int,
    run_index: int,
    work_root: Path,
    sessions: int,
    large_session_events: int,
    initial_repeats: int,
    repeats: int,
    changed_files: int,
    concurrent_queries: int,
    concurrent_interval_ms: int,
    require_concurrency: bool,
    sampling_interval_ms: int,
    thresholds: dict[str, float],
) -> dict[str, object]:
    observed_sha256 = binary_sha256(ctx_bin)
    if observed_sha256 != hash_binding["observed_sha256"]:
        raise HarnessError(
            f"{role} binary bytes changed after hash preflight: "
            f"expected {hash_binding['observed_sha256']}, observed {observed_sha256}"
        )
    expected_digest = hash_binding["expected_sha256"]
    if expected_digest is not None and observed_sha256 != expected_digest:
        raise HarnessError(
            f"{role} binary no longer matches its expected SHA-256 before execution"
        )

    run_root = work_root / f"profile-{run_index:02d}"
    run_root.mkdir(parents=True, exist_ok=False)
    active_root = run_root / "initial-00"
    active_root.mkdir()
    home = active_root / "home"
    data_root = active_root / "data"
    temp_root = active_root / "tmp"
    corpus_root = active_root / "corpus" / "codex-sessions"
    home.mkdir(parents=True, exist_ok=True)
    data_root.mkdir(parents=True, exist_ok=True)
    temp_root.mkdir(parents=True, exist_ok=True)

    env = command_env(home, data_root, temp_root)

    generation_started = time.perf_counter()
    source_bytes, generated_events = generate_corpus(
        corpus_root,
        sessions,
        large_session_events,
    )
    generation_ms = (time.perf_counter() - generation_started) * 1000.0

    version = ctx_version(ctx_bin, env)
    resolved_role = effective_role(role, version)
    append_oracle = append_expectation(resolved_role, changed_files)

    initial_result = run_ctx(
        ctx_bin,
        import_command(corpus_root),
        env,
        data_root,
        sampling_interval_ms,
    )
    initial_import_packet = initial_result["packet"]
    initial_totals = profile_summary(initial_import_packet)
    expect_exact_import_delta(initial_totals, "initial import", sessions, generated_events)
    initial_results = [initial_result]
    initial_sample_paths = [str(corpus_root)]
    for sample in range(1, initial_repeats):
        sample_root = run_root / f"initial-{sample:02d}"
        sample_home = sample_root / "home"
        sample_data_root = sample_root / "data"
        sample_temp_root = sample_root / "tmp"
        sample_corpus_root = sample_root / "corpus" / "codex-sessions"
        sample_home.mkdir(parents=True)
        sample_data_root.mkdir(parents=True)
        sample_temp_root.mkdir(parents=True)
        sample_source_bytes, sample_generated_events = generate_corpus(
            sample_corpus_root,
            sessions,
            large_session_events,
        )
        if sample_source_bytes != source_bytes or sample_generated_events != generated_events:
            raise HarnessError("repeated initial corpus generation was not deterministic")
        sample_env = command_env(sample_home, sample_data_root, sample_temp_root)
        sample_result = run_ctx(
            ctx_bin,
            import_command(sample_corpus_root),
            sample_env,
            sample_data_root,
            sampling_interval_ms,
        )
        sample_totals = profile_summary(sample_result["packet"])
        expect_exact_import_delta(
            sample_totals,
            f"initial import repeat {sample}",
            sessions,
            generated_events,
        )
        initial_results.append(sample_result)
        initial_sample_paths.append(str(sample_corpus_root))
    storage_samples = [storage_sample("after_initial_import", data_root, generated_events)]

    status_profile, status_last, _ = measure(
        "status",
        ctx_bin,
        ["status", "--json"],
        repeats,
        env,
        data_root,
        sampling_interval_ms,
        lambda packet: (
            packet.get("initialized") is True
            or (_ for _ in ()).throw(HarnessError(f"status did not report initialized: {packet}"))
        ),
    )

    search_args = ["search", QUERY, "--refresh", "off", "--json", "--limit", "20"]
    search_profile, search_last, _ = measure(
        "search_refresh_off",
        ctx_bin,
        search_args,
        repeats,
        env,
        data_root,
        sampling_interval_ms,
        lambda packet: (
            isinstance(packet.get("results"), list)
            and len(packet["results"]) > 0
            or (_ for _ in ()).throw(HarnessError(f"search returned no results: {packet}"))
        ),
    )
    first_result = search_last["results"][0]
    session_id = first_result.get("ctx_session_id")
    if not isinstance(session_id, str) or not session_id:
        raise HarnessError(f"search result is missing ctx_session_id: {first_result}")

    filtered_search_args = [
        "search",
        QUERY,
        "--refresh",
        "off",
        "--json",
        "--limit",
        "10",
        "--provider",
        "codex",
        "--workspace",
        "/workspace/ctx",
        "--event-type",
        "message",
    ]
    filtered_search_profile, filtered_search_last, _ = measure(
        "filtered_search_refresh_off",
        ctx_bin,
        filtered_search_args,
        repeats,
        env,
        data_root,
        sampling_interval_ms,
        lambda packet: (
            isinstance(packet.get("results"), list)
            and len(packet["results"]) > 0
            or (_ for _ in ()).throw(HarnessError(f"filtered search returned no results: {packet}"))
        ),
    )

    noop_profile, noop_last, _ = measure(
        "noop_incremental_import",
        ctx_bin,
        import_command(corpus_root),
        repeats,
        env,
        data_root,
        sampling_interval_ms,
        lambda packet: (
            profile_summary(packet)["imported_sessions"] == 0
            and profile_summary(packet)["imported_events"] == 0
            and profile_summary(packet)["imported_edges"] == 0
            or (_ for _ in ()).throw(HarnessError(f"no-op import imported data: {packet}"))
        ),
    )
    storage_samples.append(storage_sample("after_noop_import", data_root, generated_events))

    append_results: list[dict[str, object]] = []
    append_summaries: list[dict[str, object]] = []
    for sample in range(repeats):
        append_changed_events(
            corpus_root,
            sessions,
            changed_files,
            sample,
            large_session_events,
        )
        result = run_ctx(
            ctx_bin,
            import_command(corpus_root),
            env,
            data_root,
            sampling_interval_ms,
        )
        packet = result["packet"]
        summary = profile_summary(packet)
        expect_exact_import_delta(
            summary,
            f"{resolved_role} append import sample {sample}",
            int(append_oracle["imported_sessions"]),
            int(append_oracle["imported_events"]),
            int(append_oracle["imported_edges"]),
        )
        append_results.append(result)
        append_summaries.append(summary)
    total_changed_events = changed_files * repeats
    storage_samples.append(
        storage_sample("after_append_imports", data_root, generated_events + total_changed_events)
    )
    append_profile = {
        **command_profile(ctx_bin, import_command(corpus_root), append_results),
        "changed_files_per_sample": changed_files,
        "sample_summaries": append_summaries,
    }

    replacement_results: list[dict[str, object]] = []
    replacement_summaries: list[dict[str, object]] = []
    total_replacement_events = 0
    for sample in range(repeats):
        sample_replacement_events = replace_changed_sessions(
            corpus_root,
            sessions,
            changed_files,
            sample,
            large_session_events,
        )
        total_replacement_events += sample_replacement_events
        result = run_ctx(
            ctx_bin,
            import_command(corpus_root),
            env,
            data_root,
            sampling_interval_ms,
        )
        packet = result["packet"]
        summary = profile_summary(packet)
        expect_exact_import_delta(
            summary,
            f"replacement import sample {sample}",
            changed_files,
            sample_replacement_events,
        )
        replacement_results.append(result)
        replacement_summaries.append(summary)
    storage_samples.append(
        storage_sample(
            "after_replacement_imports",
            data_root,
            generated_events + total_changed_events + total_replacement_events,
        )
    )
    replacement_profile = {
        **command_profile(ctx_bin, import_command(corpus_root), replacement_results),
        "changed_files_per_sample": changed_files,
        "sample_summaries": replacement_summaries,
    }

    concurrent_profile, _ = run_concurrent_query_profile(
        ctx_bin,
        corpus_root,
        search_args,
        concurrent_queries,
        concurrent_interval_ms,
        require_concurrency,
        env,
        data_root,
        sampling_interval_ms,
    )
    storage_samples.append(
        storage_sample(
            "after_concurrent_rescan",
            data_root,
            generated_events + total_changed_events + total_replacement_events,
        )
    )

    show_profile, show_last, _ = measure(
        "show_session_lite",
        ctx_bin,
        ["show", "session", session_id, "--mode", "lite", "--format", "json"],
        repeats,
        env,
        data_root,
        sampling_interval_ms,
        lambda packet: (
            isinstance(packet, dict)
            and (packet.get("id") == session_id or packet.get("ctx_session_id") == session_id)
            or (_ for _ in ()).throw(HarnessError(f"show session did not return {session_id}: {packet}"))
        ),
    )

    profiles: dict[str, object] = {
        "generation": {"duration_ms": round2(generation_ms)},
        "initial_import": {
            **command_profile(ctx_bin, import_command(corpus_root), initial_results),
            "totals": initial_totals,
            "sample_source_paths": initial_sample_paths,
        },
        "status": {
            **status_profile,
            "last": {
                "indexed_items": status_last.get("indexed_items"),
                "indexed_catalog_sessions": status_last.get("indexed_catalog_sessions"),
                "database_path": status_last.get("database_path"),
            },
        },
        "search_refresh_off": {
            **search_profile,
            "last": {
                "result_count": len(search_last.get("results", [])),
                "freshness": search_last.get("freshness"),
                "top_result": {
                    "ctx_session_id": first_result.get("ctx_session_id"),
                    "ctx_event_id": first_result.get("ctx_event_id"),
                    "result_scope": first_result.get("result_scope"),
                    "provider": first_result.get("provider"),
                    "source_exists": first_result.get("source_exists"),
                },
            },
        },
        "filtered_search_refresh_off": {
            **filtered_search_profile,
            "filters": {
                "provider": "codex",
                "workspace": "/workspace/ctx",
                "event_type": "message",
            },
            "last": {
                "result_count": len(filtered_search_last.get("results", [])),
                "freshness": filtered_search_last.get("freshness"),
            },
        },
        "noop_incremental_import": {
            **noop_profile,
            "last_totals": profile_summary(noop_last),
        },
        "append_incremental_import": append_profile,
        "replacement_import": replacement_profile,
        "concurrent_refresh_off_search": concurrent_profile,
        "show_session_lite": {
            **show_profile,
            "session_id": session_id,
            "event_count": len(show_last.get("events", [])) if isinstance(show_last.get("events"), list) else None,
        },
    }

    checks = [
        {
            "name": "status_p95_ms",
            "actual": profiles["status"]["timings"]["p95_ms"],
            "threshold": thresholds["status_p95_ms"],
        },
        {
            "name": "search_refresh_off_p95_ms",
            "actual": profiles["search_refresh_off"]["timings"]["p95_ms"],
            "threshold": thresholds["search_p95_ms"],
        },
        {
            "name": "noop_incremental_import_p95_ms",
            "actual": profiles["noop_incremental_import"]["timings"]["p95_ms"],
            "threshold": thresholds["import_noop_p95_ms"],
        },
        {
            "name": "changed_incremental_import_p95_ms",
            "actual": profiles["append_incremental_import"]["timings"]["p95_ms"],
            "threshold": thresholds["import_changed_p95_ms"],
        },
        {
            "name": "replacement_import_p95_ms",
            "actual": profiles["replacement_import"]["timings"]["p95_ms"],
            "threshold": thresholds["import_replacement_p95_ms"],
        },
        {
            "name": "show_session_lite_p95_ms",
            "actual": profiles["show_session_lite"]["timings"]["p95_ms"],
            "threshold": thresholds["show_session_p95_ms"],
        },
    ]
    concurrent_timings = profiles["concurrent_refresh_off_search"]["query"]["timings"]
    if concurrent_timings is not None:
        checks.append(
            {
                "name": "concurrent_refresh_off_search_p95_ms",
                "actual": concurrent_timings["p95_ms"],
                "threshold": thresholds["concurrent_search_p95_ms"],
            }
        )
    for check in checks:
        check["passed"] = float(check["actual"]) <= float(check["threshold"])

    passed = all(bool(check["passed"]) for check in checks)
    return {
        "run_id": f"{comparison_order}:{execution_position}:{resolved_role}",
        "label": label,
        "role": resolved_role,
        "comparison_order": comparison_order,
        "execution_position": execution_position,
        "status": "passed" if passed else "failed",
        "binary": {
            "path": str(ctx_bin),
            "version": version,
            "size_bytes": ctx_bin.stat().st_size,
            "expected_sha256_env": hash_binding["expected_sha256_env"],
            "expected_sha256": expected_digest,
            "sha256": observed_sha256,
            "sha256_matched": (
                observed_sha256 == expected_digest if expected_digest is not None else None
            ),
        },
        "work_root": str(run_root),
        "corpus": {
            "provider": "codex",
            "source_format": "codex_session_jsonl_tree",
            "sessions": sessions,
            "generated_events": generated_events,
            "large_session_events": large_session_events,
            "append_events": total_changed_events,
            "replacement_fixture_events": total_replacement_events,
            "source_files": sessions,
            "initial_source_bytes": source_bytes,
            "final_source_bytes": file_tree_bytes(corpus_root),
            "changed_files_per_sample": changed_files,
            "append_expectation": append_oracle,
            "query": QUERY,
            "source_path": str(corpus_root),
        },
        "profiles": profiles,
        "storage": {
            "data_root": str(data_root),
            "db_footprint_bytes": db_footprint_bytes(data_root),
            "files": sqlite_footprint(data_root),
            "samples": storage_samples,
        },
        "checks": checks,
    }


PY
}
