#!/usr/bin/env bash

perf_smoke_emit_python_runner() {
  cat <<'PY'
def run_status_snapshot(
    ctx_bin: Path,
    env: dict[str, str],
    data_root: Path,
    sampling_interval_ms: int,
    expected_sessions: int,
    expected_events: int,
) -> tuple[dict[str, object], dict[str, object]]:
    result = run_ctx(
        ctx_bin,
        ["status", "--format=json"],
        env,
        data_root,
        sampling_interval_ms,
    )
    packet = result["packet"]
    expect_source_backed_status(
        packet,
        data_root,
        expected_sessions,
        expected_events,
    )
    return packet, result


def expect_generation_transition(
    label: str,
    refresh: dict[str, object],
    status: dict[str, object],
    previous_generation: str | None,
    changed: bool,
) -> str:
    generation = status["lexical"]["generation_id"]
    if not isinstance(generation, str) or not generation:
        raise HarnessError(f"{label} status has no lexical generation: {status}")
    if refresh["published_generation"] != generation:
        raise HarnessError(
            f"{label} refresh/status generation mismatch: "
            f"{refresh['published_generation']} != {generation}"
        )
    if previous_generation is not None and (generation != previous_generation) is not changed:
        raise HarnessError(
            f"{label} generation transition did not match changed={changed}: "
            f"{previous_generation} -> {generation}"
        )
    return generation


def start_resolver_daemon(
    ctx_bin: Path,
    env: dict[str, str],
    data_root: Path,
) -> tuple[subprocess.Popen[bytes], object, object]:
    process_temp_root = data_root.parent / "tmp"
    stdout_file = tempfile.TemporaryFile(mode="w+b", dir=process_temp_root)
    stderr_file = tempfile.TemporaryFile(mode="w+b", dir=process_temp_root)
    process = subprocess.Popen(
        [
            str(ctx_bin),
            "daemon",
            "run",
            "--force",
            "--idle-exit-seconds",
            "300",
            "--loop-interval-seconds",
            "300",
            "--format=json",
        ],
        cwd=REPO_ROOT,
        env=env,
        stdout=stdout_file,
        stderr=stderr_file,
    )
    deadline = time.perf_counter() + env_float(
        "CTX_PERF_SMOKE_COMMAND_TIMEOUT_SECONDS",
        300.0,
        1.0,
    )
    while time.perf_counter() < deadline:
        return_code = process.poll()
        if return_code is not None:
            stderr_file.seek(0)
            stderr = stderr_file.read().decode("utf-8", errors="replace")
            raise HarnessError(
                f"resolver daemon exited before becoming ready ({return_code}): {stderr}"
            )
        completed = subprocess.run(
            [str(ctx_bin), "daemon", "status", "--format=json"],
            cwd=REPO_ROOT,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if completed.returncode == 0:
            try:
                packet = json.loads(completed.stdout)
            except json.JSONDecodeError:
                packet = None
            daemon = packet.get("daemon") if isinstance(packet, dict) else None
            endpoint = (
                daemon.get("source_refresh_endpoint")
                if isinstance(daemon, dict)
                else None
            )
            jobs = daemon.get("jobs") if isinstance(daemon, dict) else None
            refresh = (
                jobs.get("source_backed_refresh") if isinstance(jobs, dict) else None
            )
            if (
                isinstance(daemon, dict)
                and daemon.get("running") is True
                and isinstance(endpoint, dict)
                and endpoint.get("available") is True
                and isinstance(refresh, dict)
                and refresh.get("status") == "completed"
                and refresh.get("request_state") == "published"
            ):
                return process, stdout_file, stderr_file
        time.sleep(0.02)
    process.terminate()
    process.wait(timeout=5)
    raise HarnessError("resolver daemon did not become ready before the command timeout")


def stop_resolver_daemon(
    process: subprocess.Popen[bytes],
    stdout_file: object,
    stderr_file: object,
) -> None:
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    stdout_file.close()
    stderr_file.close()


def measure_hydrated_queries(
    ctx_bin: Path,
    env: dict[str, str],
    data_root: Path,
    sampling_interval_ms: int,
    repeats: int,
) -> tuple[
    list[str],
    dict[str, object],
    dict[str, object],
    dict[str, object],
    dict[str, object],
    dict[str, object],
    dict[str, object],
    str,
]:
    search_args = ["search", QUERY, "--refresh", "off", "--format=json", "--limit", "20"]
    filtered_search_args = [
        "search",
        QUERY,
        "--refresh",
        "off",
        "--format=json",
        "--limit",
        "10",
        "--provider",
        "codex",
        "--workspace",
        "/workspace/ctx",
        "--event-type",
        "message",
    ]
    process, stdout_file, stderr_file = start_resolver_daemon(ctx_bin, env, data_root)
    try:
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
                or (_ for _ in ()).throw(
                    HarnessError(f"search returned no results: {packet}")
                )
            ),
        )
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
                or (_ for _ in ()).throw(
                    HarnessError(f"filtered search returned no results: {packet}")
                )
            ),
        )
        first_result = search_last["results"][0]
        session_id = first_result.get("ctx_session_id")
        if not isinstance(session_id, str) or not session_id:
            raise HarnessError(f"search result is missing ctx_session_id: {first_result}")
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
                and (
                    packet.get("id") == session_id
                    or packet.get("ctx_session_id") == session_id
                )
                or (_ for _ in ()).throw(
                    HarnessError(f"show session did not return {session_id}: {packet}")
                )
            ),
        )
    finally:
        stop_resolver_daemon(process, stdout_file, stderr_file)
    return (
        search_args,
        search_profile,
        search_last,
        filtered_search_profile,
        filtered_search_last,
        show_profile,
        show_last,
        session_id,
    )


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
    corpus_root = home / ".codex" / "sessions"
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
    expected_sessions, expected_events = corpus_counts(corpus_root)
    if expected_sessions != sessions or expected_events != generated_events:
        raise HarnessError("generated source corpus counts are inconsistent")

    version = ctx_version(ctx_bin, env)
    resolved_role = effective_role(role, version)

    initial_result = run_ctx(
        ctx_bin,
        source_refresh_command(),
        env,
        data_root,
        sampling_interval_ms,
    )
    initial_refresh = expect_source_refresh(initial_result["packet"], changed=True)
    initial_status, _ = run_status_snapshot(
        ctx_bin,
        env,
        data_root,
        sampling_interval_ms,
        expected_sessions,
        expected_events,
    )
    current_generation = expect_generation_transition(
        "initial source refresh",
        initial_refresh,
        initial_status,
        None,
        True,
    )
    initial_results = [initial_result]
    initial_receipts = [initial_refresh]
    initial_sample_paths = [str(corpus_root)]

    for sample in range(1, initial_repeats):
        sample_root = run_root / f"initial-{sample:02d}"
        sample_home = sample_root / "home"
        sample_data_root = sample_root / "data"
        sample_temp_root = sample_root / "tmp"
        sample_corpus_root = sample_home / ".codex" / "sessions"
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
            source_refresh_command(),
            sample_env,
            sample_data_root,
            sampling_interval_ms,
        )
        sample_refresh = expect_source_refresh(sample_result["packet"], changed=True)
        sample_status, _ = run_status_snapshot(
            ctx_bin,
            sample_env,
            sample_data_root,
            sampling_interval_ms,
            sessions,
            generated_events,
        )
        expect_generation_transition(
            f"initial source refresh repeat {sample}",
            sample_refresh,
            sample_status,
            None,
            True,
        )
        initial_results.append(sample_result)
        initial_receipts.append(sample_refresh)
        initial_sample_paths.append(str(sample_corpus_root))

    storage_samples = [
        storage_sample("after_initial_source_refresh", data_root, expected_events)
    ]

    status_profile, status_last, _ = measure(
        "status",
        ctx_bin,
        ["status", "--format=json"],
        repeats,
        env,
        data_root,
        sampling_interval_ms,
        lambda packet: expect_source_backed_status(
            packet,
            data_root,
            expected_sessions,
            expected_events,
        ),
    )

    noop_profile, noop_last, _ = measure(
        "noop_source_refresh",
        ctx_bin,
        source_refresh_command(),
        repeats,
        env,
        data_root,
        sampling_interval_ms,
        lambda packet: expect_source_refresh(packet, changed=False),
    )
    noop_status, _ = run_status_snapshot(
        ctx_bin,
        env,
        data_root,
        sampling_interval_ms,
        expected_sessions,
        expected_events,
    )
    expect_generation_transition(
        "no-op source refresh",
        expect_source_refresh(noop_last, changed=False),
        noop_status,
        current_generation,
        False,
    )
    storage_samples.append(
        storage_sample("after_noop_source_refresh", data_root, expected_events)
    )

    append_results: list[dict[str, object]] = []
    append_receipts: list[dict[str, object]] = []
    for sample in range(repeats):
        append_changed_events(
            corpus_root,
            sessions,
            changed_files,
            sample,
            large_session_events,
        )
        expected_sessions, expected_events = corpus_counts(corpus_root)
        result = run_ctx(
            ctx_bin,
            source_refresh_command(),
            env,
            data_root,
            sampling_interval_ms,
        )
        refresh = expect_source_refresh(result["packet"], changed=True)
        status, _ = run_status_snapshot(
            ctx_bin,
            env,
            data_root,
            sampling_interval_ms,
            expected_sessions,
            expected_events,
        )
        current_generation = expect_generation_transition(
            f"append source refresh sample {sample}",
            refresh,
            status,
            current_generation,
            True,
        )
        append_results.append(result)
        append_receipts.append(
            {
                **refresh,
                "expected_sessions": expected_sessions,
                "expected_events": expected_events,
            }
        )
    total_appended_events = changed_files * repeats
    storage_samples.append(
        storage_sample("after_append_source_refreshes", data_root, expected_events)
    )
    append_profile = {
        **command_profile(ctx_bin, source_refresh_command(), append_results),
        "changed_files_per_sample": changed_files,
        "receipts": append_receipts,
    }

    replacement_results: list[dict[str, object]] = []
    replacement_receipts: list[dict[str, object]] = []
    total_replacement_fixture_events = 0
    for sample in range(repeats):
        sample_replacement_events = replace_changed_sessions(
            corpus_root,
            sessions,
            changed_files,
            sample,
            large_session_events,
        )
        total_replacement_fixture_events += sample_replacement_events
        expected_sessions, expected_events = corpus_counts(corpus_root)
        result = run_ctx(
            ctx_bin,
            source_refresh_command(),
            env,
            data_root,
            sampling_interval_ms,
        )
        refresh = expect_source_refresh(result["packet"], changed=True)
        status, _ = run_status_snapshot(
            ctx_bin,
            env,
            data_root,
            sampling_interval_ms,
            expected_sessions,
            expected_events,
        )
        current_generation = expect_generation_transition(
            f"replacement source refresh sample {sample}",
            refresh,
            status,
            current_generation,
            True,
        )
        replacement_results.append(result)
        replacement_receipts.append(
            {
                **refresh,
                "expected_sessions": expected_sessions,
                "expected_events": expected_events,
            }
        )
    storage_samples.append(
        storage_sample("after_replacement_source_refreshes", data_root, expected_events)
    )
    replacement_profile = {
        **command_profile(ctx_bin, source_refresh_command(), replacement_results),
        "changed_files_per_sample": changed_files,
        "receipts": replacement_receipts,
    }

    delete_results: list[dict[str, object]] = []
    delete_receipts: list[dict[str, object]] = []
    total_deleted_sessions = 0
    total_deleted_events = 0
    for sample in range(repeats):
        remaining_samples = repeats - sample
        delete_batch = min(
            changed_files,
            max(1, (expected_sessions - 1) // remaining_samples),
        )
        deleted_sessions, deleted_events = delete_sessions(corpus_root, delete_batch)
        total_deleted_sessions += deleted_sessions
        total_deleted_events += deleted_events
        expected_sessions, expected_events = corpus_counts(corpus_root)
        result = run_ctx(
            ctx_bin,
            source_refresh_command(),
            env,
            data_root,
            sampling_interval_ms,
        )
        refresh = expect_source_refresh(result["packet"], changed=True)
        status, _ = run_status_snapshot(
            ctx_bin,
            env,
            data_root,
            sampling_interval_ms,
            expected_sessions,
            expected_events,
        )
        current_generation = expect_generation_transition(
            f"delete source refresh sample {sample}",
            refresh,
            status,
            current_generation,
            True,
        )
        delete_results.append(result)
        delete_receipts.append(
            {
                **refresh,
                "deleted_sessions": deleted_sessions,
                "deleted_events": deleted_events,
                "expected_sessions": expected_sessions,
                "expected_events": expected_events,
            }
        )
    storage_samples.append(
        storage_sample("after_delete_source_refreshes", data_root, expected_events)
    )
    delete_profile = {
        **command_profile(ctx_bin, source_refresh_command(), delete_results),
        "maximum_deleted_files_per_sample": changed_files,
        "receipts": delete_receipts,
    }

    final_status, _ = run_status_snapshot(
        ctx_bin,
        env,
        data_root,
        sampling_interval_ms,
        expected_sessions,
        expected_events,
    )
    if final_status["lexical"]["generation_id"] != current_generation:
        raise HarnessError("final status changed generation without a source mutation")

    (
        search_args,
        search_profile,
        search_last,
        filtered_search_profile,
        filtered_search_last,
        show_profile,
        show_last,
        session_id,
    ) = measure_hydrated_queries(
        ctx_bin,
        env,
        data_root,
        sampling_interval_ms,
        repeats,
    )

    profiles: dict[str, object] = {
        "generation": {"duration_ms": round2(generation_ms)},
        "initial_source_refresh": {
            **command_profile(ctx_bin, source_refresh_command(), initial_results),
            "receipts": initial_receipts,
            "sample_source_paths": initial_sample_paths,
        },
        "status": {
            **status_profile,
            "initial": {
                "indexed_items": status_last.get("indexed_items"),
                "indexed_sessions": status_last.get("indexed_sessions"),
                "indexed_sources": status_last.get("indexed_sources"),
                "lexical": status_last.get("lexical"),
                "catalog": status_last.get("catalog"),
                "semantic": status_last.get("semantic"),
                "relational": status_last.get("relational"),
            },
            "final": {
                "indexed_items": final_status.get("indexed_items"),
                "indexed_sessions": final_status.get("indexed_sessions"),
                "indexed_sources": final_status.get("indexed_sources"),
                "lexical": final_status.get("lexical"),
                "catalog": final_status.get("catalog"),
                "semantic": final_status.get("semantic"),
                "relational": final_status.get("relational"),
            },
        },
        "search_refresh_off": {
            **search_profile,
            "last": {
                "result_count": len(search_last.get("results", [])),
                "freshness": search_last.get("freshness"),
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
        "noop_source_refresh": {
            **noop_profile,
            "last_receipt": expect_source_refresh(noop_last, changed=False),
        },
        "append_source_refresh": append_profile,
        "replacement_source_refresh": replacement_profile,
        "delete_source_refresh": delete_profile,
        "show_session_lite": {
            **show_profile,
            "session_id": session_id,
            "event_count": len(show_last.get("events", []))
            if isinstance(show_last.get("events"), list)
            else None,
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
            "name": "noop_source_refresh_p95_ms",
            "actual": profiles["noop_source_refresh"]["timings"]["p95_ms"],
            "threshold": thresholds["refresh_noop_p95_ms"],
        },
        {
            "name": "append_source_refresh_p95_ms",
            "actual": profiles["append_source_refresh"]["timings"]["p95_ms"],
            "threshold": thresholds["refresh_append_p95_ms"],
        },
        {
            "name": "replacement_source_refresh_p95_ms",
            "actual": profiles["replacement_source_refresh"]["timings"]["p95_ms"],
            "threshold": thresholds["refresh_replacement_p95_ms"],
        },
        {
            "name": "delete_source_refresh_p95_ms",
            "actual": profiles["delete_source_refresh"]["timings"]["p95_ms"],
            "threshold": thresholds["refresh_delete_p95_ms"],
        },
        {
            "name": "show_session_lite_p95_ms",
            "actual": profiles["show_session_lite"]["timings"]["p95_ms"],
            "threshold": thresholds["show_session_p95_ms"],
        },
    ]
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
            "initial_sessions": sessions,
            "initial_events": generated_events,
            "final_sessions": expected_sessions,
            "final_events": expected_events,
            "large_session_events": large_session_events,
            "appended_events": total_appended_events,
            "replacement_fixture_events": total_replacement_fixture_events,
            "deleted_sessions": total_deleted_sessions,
            "deleted_events": total_deleted_events,
            "initial_source_files": sessions,
            "initial_source_bytes": source_bytes,
            "final_source_bytes": file_tree_bytes(corpus_root),
            "changed_files_per_sample": changed_files,
            "query": QUERY,
            "source_path": str(corpus_root),
        },
        "profiles": profiles,
        "storage": {
            "data_root": str(data_root),
            "source_backed_footprint_bytes": source_backed_footprint_bytes(data_root),
            "files": source_backed_storage_footprint(data_root),
            "samples": storage_samples,
        },
        "checks": checks,
    }


PY
}
