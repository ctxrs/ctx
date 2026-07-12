use super::support::*;

#[test]

fn codex_session_tree_imports_messages_and_subagent_edges() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codex_session_tree(
        &fixture,
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 7);
    assert_eq!(first.imported_edges, 1);

    let second = import_codex_session_tree(
        &fixture,
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_events, 8);
    assert_eq!(second.skipped_edges, 1);

    let parent_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-session-root");
    let child_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-session-child");
    let parent = store.get_session(parent_id).unwrap();
    let child = store.get_session(child_id).unwrap();
    assert_eq!(parent.sync.fidelity, Fidelity::Imported);
    assert_eq!(
        parent.sync.metadata["source_format"].as_str(),
        Some("codex_session_jsonl")
    );
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.root_session_id, Some(parent_id));
    assert_eq!(child.agent_type, AgentType::Subagent);
    assert_eq!(child.role_hint.as_deref(), Some("worker"));

    let parent_events = store.events_for_session(parent_id).unwrap();
    assert_eq!(parent_events.len(), 5);
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Message
            && event.role == Some(EventRole::System)
            && event
                .payload
                .to_string()
                .contains("Follow repo instructions")));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Message
            && event.payload.to_string().contains("Fix the onboarding bug")));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Message
            && event
                .payload
                .to_string()
                .contains("checking the setup flow")));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall
            && event.payload.to_string().contains("exec_command")));
    assert!(parent_events
        .iter()
        .any(|event| event.event_type == EventType::Summary
            && event
                .payload
                .to_string()
                .contains("provider history discovery")));
    let child_events = store.events_for_session(child_id).unwrap();
    assert_eq!(child_events.len(), 2);
    assert!(child_events
        .iter()
        .any(|event| event.payload.to_string().contains("local history search")));
}

#[test]
fn codex_session_catalog_large_noop_uses_metadata_cache() {
    let temp = tempdir();
    let root = temp.path().join("sessions");
    let session_count = 1_024;
    synthetic_codex_session_tree(&root, session_count);
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-06-26T12:00:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.source_files, session_count);
    assert_eq!(first.cataloged_sessions, session_count);
    assert_eq!(first.cached_sessions, 0);
    assert_eq!(first.parsed_sessions, session_count);
    assert_eq!(first.failed_sessions, 0);

    let second = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-06-26T12:01:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.source_files, session_count);
    assert_eq!(second.cataloged_sessions, session_count);
    assert_eq!(second.cached_sessions, session_count);
    assert_eq!(second.parsed_sessions, 0);
    assert_eq!(second.failed_sessions, 0);

    write_synthetic_codex_session(&root, 17, "changed-size-for-incremental-refresh");
    let third = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-06-26T12:02:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(third.source_files, session_count);
    assert_eq!(third.cataloged_sessions, session_count);
    assert_eq!(third.cached_sessions, session_count - 1);
    assert_eq!(third.parsed_sessions, 1);
    assert_eq!(third.failed_sessions, 0);
}

#[test]
fn codex_session_catalog_skips_oversized_metadata_probe_line() {
    let temp = tempdir();
    let root = temp.path().join("sessions/2026/07/03");
    fs::create_dir_all(&root).unwrap();
    write_oversized_jsonl_line(&root.join("oversized.jsonl"));
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = catalog_codex_session_tree(
        temp.path().join("sessions"),
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(temp.path().join("sessions")),
            cataloged_at: "2026-07-03T12:00:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.source_files, 1);
    assert_eq!(summary.cataloged_sessions, 1);
    assert_eq!(summary.parsed_sessions, 1);
    assert_eq!(summary.failed_sessions, 0);
}

#[test]
fn codex_session_catalog_marks_deleted_paths_stale_when_additions_outnumber_deletions() {
    let temp = tempdir();
    let root = temp.path().join("sessions");
    synthetic_codex_session_tree(&root, 2);
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = root.display().to_string();

    let first = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-06-26T12:00:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.cataloged_sessions, 2);

    fs::remove_file(
        root.join("2026/06/26/00")
            .join("synthetic-session-000000.jsonl"),
    )
    .unwrap();
    write_synthetic_codex_session(&root, 2, "addition-one");
    write_synthetic_codex_session(&root, 3, "addition-two");

    let second = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-06-26T12:01:00Z".parse().unwrap(),
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.source_files, 3);
    assert_eq!(second.cataloged_sessions, 3);
    assert_eq!(
        store
            .catalog_source_stale_session_count(CaptureProvider::Codex, &source_root)
            .unwrap(),
        1
    );
}

#[test]
#[ignore = "manual perf benchmark; release gates run scripts/public-ctx/perf-smoke.sh from the validation workspace"]
fn synthetic_codex_incremental_import_perf_records_thresholded_evidence() {
    let out_dir = std::env::var_os("CTX_ARTIFACT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            capture_repo_root().join("target/ctx-artifacts/synthetic_codex_incremental_import_perf")
        });
    fs::create_dir_all(&out_dir).unwrap();
    let artifact_path = out_dir.join("synthetic-codex-incremental-import-perf.json");

    let temp = tempdir();
    let root = temp.path().join("sessions");
    let file_count = incremental_perf_file_count();
    let repeats = incremental_perf_repeats();
    let generation_started = std::time::Instant::now();
    let source_bytes = synthetic_codex_session_tree(&root, file_count);
    let generation_ms = elapsed_ms(generation_started.elapsed());

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first_started = std::time::Instant::now();
    let first =
        incremental_codex_catch_up(&root, &mut store, "2026-06-26T13:00:00Z".parse().unwrap());
    let first_ms = elapsed_ms(first_started.elapsed());
    assert_eq!(first.catalog.parsed_sessions, file_count);
    assert_eq!(first.catalog.cached_sessions, 0);
    assert_eq!(first.pending_sessions, file_count);
    assert_eq!(first.import.imported_sessions, file_count);

    let warmup =
        incremental_codex_catch_up(&root, &mut store, "2026-06-26T13:01:00Z".parse().unwrap());
    assert_eq!(warmup.catalog.cached_sessions, file_count);
    assert_eq!(warmup.catalog.parsed_sessions, 0);
    assert_eq!(warmup.pending_sessions, 0);
    assert_eq!(warmup.import.imported_sessions, 0);
    assert_eq!(warmup.import.imported_events, 0);

    let mut noop_samples = Vec::with_capacity(repeats);
    let noop_base_time: DateTime<Utc> = "2026-06-26T13:02:00Z".parse().unwrap();
    for index in 0..repeats {
        let observed_at = noop_base_time + chrono::Duration::minutes(index as i64);
        let started = std::time::Instant::now();
        let noop = incremental_codex_catch_up(&root, &mut store, observed_at);
        let elapsed = elapsed_ms(started.elapsed());
        assert_eq!(noop.catalog.cached_sessions, file_count);
        assert_eq!(noop.catalog.parsed_sessions, 0);
        assert_eq!(noop.pending_sessions, 0);
        assert_eq!(noop.import.imported_sessions, 0);
        assert_eq!(noop.import.imported_events, 0);
        noop_samples.push(elapsed);
    }

    let noop_stats = timing_stats(&noop_samples);
    let noop_us_per_file = (noop_stats.p95_ms * 1000.0) / file_count as f64;
    let noop_p95_threshold_ms = incremental_perf_noop_p95_threshold_ms(file_count);
    let noop_us_per_file_threshold = incremental_perf_noop_us_per_file_threshold();
    let checks = vec![
        json!({
            "name": "no_op_catalog_parses_zero_sessions",
            "passed": warmup.catalog.parsed_sessions == 0,
            "actual": warmup.catalog.parsed_sessions,
            "threshold": 0
        }),
        json!({
            "name": "no_op_pending_sessions_zero",
            "passed": warmup.pending_sessions == 0,
            "actual": warmup.pending_sessions,
            "threshold": 0
        }),
        json!({
            "name": "no_op_p95_ms",
            "passed": noop_stats.p95_ms <= noop_p95_threshold_ms,
            "actual": rounded(noop_stats.p95_ms),
            "threshold": noop_p95_threshold_ms
        }),
        json!({
            "name": "no_op_us_per_file",
            "passed": noop_us_per_file <= noop_us_per_file_threshold,
            "actual": rounded(noop_us_per_file),
            "threshold": noop_us_per_file_threshold
        }),
    ];
    let passed = checks
        .iter()
        .all(|check| check["passed"].as_bool().unwrap_or(false));

    let artifact = json!({
        "schema_version": 1,
        "profile": "synthetic-codex-incremental-import-perf",
        "mode": if file_count >= 30_000 { "slow" } else { "standard" },
        "status": if passed { "passed" } else { "failed" },
        "corpus": {
            "source_files": file_count,
            "source_bytes": source_bytes,
            "events_per_session": 1
        },
        "thresholds": {
            "noop_p95_ms": noop_p95_threshold_ms,
            "noop_us_per_file": noop_us_per_file_threshold,
            "env_overrides": [
                "CTX_CODEX_INCREMENTAL_PERF_FILES",
                "CTX_CODEX_INCREMENTAL_PERF_REPEATS",
                "CTX_CODEX_INCREMENTAL_PERF_SLOW",
                "CTX_CODEX_INCREMENTAL_PERF_NOOP_P95_MS",
                "CTX_CODEX_INCREMENTAL_PERF_NOOP_US_PER_FILE"
            ]
        },
        "profiles": {
            "generation": {
                "duration_ms": rounded(generation_ms)
            },
            "first_incremental_catch_up": {
                "duration_ms": rounded(first_ms),
                "catalog": {
                    "source_files": first.catalog.source_files,
                    "source_bytes": first.catalog.source_bytes,
                    "cached_sessions": first.catalog.cached_sessions,
                    "parsed_sessions": first.catalog.parsed_sessions,
                    "failed_sessions": first.catalog.failed_sessions
                },
                "pending_sessions": first.pending_sessions,
                "imported_sessions": first.import.imported_sessions,
                "imported_events": first.import.imported_events
            },
            "noop_incremental_catch_up": {
                "timings": noop_stats.to_json(),
                "repeats": repeats,
                "cached_sessions": warmup.catalog.cached_sessions,
                "parsed_sessions": warmup.catalog.parsed_sessions,
                "pending_sessions": warmup.pending_sessions,
                "p95_us_per_file": rounded(noop_us_per_file)
            }
        },
        "checks": checks
    });
    fs::write(
        &artifact_path,
        serde_json::to_vec_pretty(&artifact).unwrap(),
    )
    .unwrap();
    println!(
        "synthetic Codex incremental import perf artifact: {}",
        artifact_path.display()
    );

    assert!(
        passed,
        "synthetic Codex incremental import perf thresholds failed; see {}",
        artifact_path.display()
    );
}

#[test]
fn codex_session_tree_defers_cross_file_child_edges_until_parent_is_known() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-out-of-order-sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_tree(
        &fixture,
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T02:15:00Z".parse().unwrap(),
            max_session_files: Some(usize::MAX),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(summary.imported_edges, 1);

    let parent_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-out-of-order-root");
    let child_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-out-of-order-child");
    let child = store.get_session(child_id).unwrap();
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.root_session_id, Some(parent_id));
}

#[test]
fn codex_session_paths_imports_only_explicit_subset() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
    let total_bytes = fs::metadata(&fixture).unwrap().len();
    let progress = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&progress);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_paths(
        vec![fixture.clone()],
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T02:30:00Z".parse().unwrap(),
            progress: Some(Arc::new(move |progress| {
                observed.lock().unwrap().push((
                    progress.total_files,
                    progress.total_bytes,
                    progress.done,
                ));
            })),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 5);
    assert_eq!(summary.imported_edges, 0);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    let root_id = stored_provider_session_id(&store, CaptureProvider::Codex, "codex-session-root");
    let child_id = provider_import_session_id_for_path(
        CaptureProvider::Codex,
        "codex_session_jsonl",
        &fixture,
        "codex-session-child",
    );
    assert_eq!(store.events_for_session(root_id).unwrap().len(), 5);
    assert!(store.events_for_session(child_id).unwrap().is_empty());

    let progress = progress.lock().unwrap();
    assert!(progress
        .iter()
        .all(|(files, bytes, _)| { *files == 1 && *bytes == total_bytes }));
    assert_eq!(progress.last().map(|(_, _, done)| *done), Some(true));
}

#[test]
fn codex_session_paths_reimport_skips_existing_events() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23");
    let paths = vec![fixture.join("root.jsonl"), fixture.join("subagent.jsonl")];
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codex_session_paths(
        paths.clone(),
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-24T02:45:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 7);
    assert_eq!(first.imported_edges, 1);

    let second = import_codex_session_paths(
        paths,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-24T02:45:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 8);
    assert_eq!(second.skipped_edges, 1);
}

#[cfg(unix)]
#[test]
fn codex_session_paths_rejects_symlinked_jsonl_files() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
    let link = temp.path().join("linked-root.jsonl");
    symlink(&fixture, &link).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let err = import_codex_session_paths(
        vec![link],
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-24T03:00:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("linked-root.jsonl")
                && reason == "symlinked provider transcript files are rejected"
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn codex_session_file_rejects_symlinked_jsonl_files() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
    let link = temp.path().join("linked-root.jsonl");
    symlink(&fixture, &link).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let err = import_codex_session_jsonl(
        &link,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("linked-root.jsonl")
                && reason == "symlinked provider transcript files are rejected"
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn codex_session_file_rejects_symlinked_parent_components() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let real_dir = temp.path().join("real-parent");
    fs::create_dir_all(&real_dir).unwrap();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
    fs::copy(&fixture, real_dir.join("root.jsonl")).unwrap();
    let link_dir = temp.path().join("linked-parent");
    symlink(&real_dir, &link_dir).unwrap();
    let linked_file = link_dir.join("root.jsonl");

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let err = import_codex_session_jsonl(
        &linked_file,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("linked-parent/root.jsonl")
                && reason == "symlinked provider transcript path components are rejected"
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn codex_session_tree_rejects_symlinked_jsonl_files() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23");
    let sessions = temp.path().join("sessions/2026/06/23");
    fs::create_dir_all(&sessions).unwrap();
    symlink(fixture.join("root.jsonl"), sessions.join("root.jsonl")).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let err = import_codex_session_tree(
        temp.path().join("sessions"),
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("root.jsonl")
                && reason == "symlinked provider transcript files are rejected"
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}

fn write_codex_session_with_oversized_event(path: &Path) {
    fs::write(
        path,
        [
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "codex-oversized-skip",
                    "timestamp": "2026-07-03T12:00:00Z",
                    "cwd": "/workspace",
                    "originator": "codex-cli"
                }
            })),
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "before oversized event"}]
                }
            })),
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:02Z",
                "type": "event_msg",
                "payload": {
                    "type": "patch_apply_end",
                    "stdout": "x".repeat(MAX_PROVIDER_JSONL_LINE_BYTES + 1),
                    "stderr": "",
                    "success": true
                }
            })),
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:03Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "after oversized event"}]
                }
            })),
        ]
        .concat(),
    )
    .unwrap();
}

#[test]
fn codex_session_jsonl_skips_oversized_line_and_normalizes_remaining_events() {
    let temp = tempdir();
    let path = temp.path().join("oversized-codex.jsonl");
    write_codex_session_with_oversized_event(&path);

    let normalized = CodexSessionJsonlAdapter
        .normalize_path(&path, &ProviderAdapterContext::default())
        .unwrap();

    assert_eq!(
        normalized.summary.failed, 0,
        "{:?}",
        normalized.summary.failures
    );
    assert_eq!(normalized.summary.skipped, 1);
    assert_eq!(normalized.summary.skipped_events, 1);
    assert_eq!(normalized.captures.len(), 3);
}

#[test]
fn codex_session_jsonl_fast_import_skips_one_oversized_line() {
    let temp = tempdir();
    let path = temp.path().join("oversized-codex.jsonl");
    write_codex_session_with_oversized_event(&path);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 2);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-oversized-skip");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    let payloads = events
        .iter()
        .map(|event| event.payload.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(payloads.contains("before oversized event"), "{payloads}");
    assert!(payloads.contains("after oversized event"), "{payloads}");
    assert!(!payloads.contains("patch_apply_end"), "{payloads}");
}

#[test]
fn codex_session_jsonl_skips_oversized_required_header_without_failure() {
    let temp = tempdir();
    let path = temp.path().join("oversized-header-codex.jsonl");
    let mut bytes = oversized_jsonl_line();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "should not import without header"}]
            }
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();

    let normalized = CodexSessionJsonlAdapter
        .normalize_path(&path, &ProviderAdapterContext::default())
        .unwrap();
    assert_eq!(
        normalized.summary.failed, 0,
        "{:?}",
        normalized.summary.failures
    );
    assert_eq!(normalized.summary.skipped, 1);
    assert_eq!(normalized.summary.skipped_sessions, 1);
    assert!(normalized.captures.is_empty());

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_sessions, 1);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_session_jsonl_skips_only_oversized_events_without_fake_session() {
    let temp = tempdir();
    let path = temp.path().join("only-oversized-event-codex.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-only-oversized-event",
                "timestamp": "2026-07-03T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli"
            }
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(&oversized_jsonl_line());
    fs::write(&path, bytes).unwrap();

    let normalized = CodexSessionJsonlAdapter
        .normalize_path(&path, &ProviderAdapterContext::default())
        .unwrap();
    assert_eq!(
        normalized.summary.failed, 0,
        "{:?}",
        normalized.summary.failures
    );
    assert_eq!(normalized.summary.skipped, 1);
    assert_eq!(normalized.summary.skipped_events, 1);
    assert!(normalized.captures.is_empty());

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());

    let mut slow_store = Store::open(temp.path().join("slow-work.sqlite")).unwrap();
    let slow_summary = import_codex_session_jsonl(
        &path,
        &mut slow_store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:31:00Z".parse().unwrap(),
            fast_event_inserts: false,
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(slow_summary.failed, 0, "{:?}", slow_summary.failures);
    assert_eq!(slow_summary.skipped, 1);
    assert_eq!(slow_summary.skipped_events, 1);
    assert_eq!(slow_summary.imported_sessions, 0);
    assert_eq!(slow_summary.imported_events, 0);
    assert!(slow_store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_session_jsonl_malformed_header_is_not_hidden_by_oversized_line() {
    let temp = tempdir();
    let path = temp
        .path()
        .join("malformed-header-before-oversized-codex.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "timestamp": "2026-07-03T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli"
            }
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(&oversized_jsonl_line());
    fs::write(&path, bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("codex session_meta missing id"));
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_session_jsonl_malformed_relevant_line_is_not_hidden_by_oversized_line() {
    let temp = tempdir();
    let path = temp
        .path()
        .join("malformed-relevant-before-oversized-codex.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-malformed-before-oversized",
                "timestamp": "2026-07-03T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli"
            }
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(
        br#"{"timestamp":"2026-07-03T12:00:01Z","type":"response_item","payload":{"type":"message","role":"assistant","content":["unterminated"]"#,
    );
    bytes.push(b'\n');
    bytes.extend_from_slice(&oversized_jsonl_line());
    fs::write(&path, bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(
        summary.failures[0].error.contains("EOF while parsing")
            || summary.failures[0].error.contains("expected")
    );
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_session_probe_skips_oversized_line_before_first_real_message() {
    let temp = tempdir();
    let path = temp.path().join("oversized-before-message-codex.jsonl");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-oversized-before-message",
                "timestamp": "2026-07-03T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli"
            }
        }))
        .as_bytes(),
    );
    bytes.extend_from_slice(&oversized_jsonl_line());
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "message after oversized probe line"}]
            }
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();

    assert!(
        codex_session_file_conversation_scan(&path)
            .unwrap()
            .has_real_conversation
    );

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(
        store
            .search_event_hits("message after oversized probe line", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn codex_session_jsonl_rejects_malformed_event_timestamp() {
    let temp = tempdir();
    let path = temp.path().join("bad-timestamp-codex.jsonl");
    fs::write(
        &path,
        [
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "codex-bad-timestamp",
                    "timestamp": "2026-07-03T12:00:00Z",
                    "cwd": "/workspace",
                    "originator": "codex-cli"
                }
            })),
            jsonl_line(json!({
                "timestamp": "not-rfc3339",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "bad timestamp should not import"}
                    ]
                }
            })),
        ]
        .concat(),
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            fast_event_inserts: false,
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("timestamp is not a valid RFC3339 timestamp"));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_session_jsonl_rejects_metadata_only_without_real_messages() {
    let temp = tempdir();
    let path = temp.path().join("metadata-only-codex.jsonl");
    fs::write(
        &path,
        [
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "codex-metadata-only",
                    "timestamp": "2026-07-03T12:00:00Z",
                    "cwd": "/workspace",
                    "originator": "codex-cli"
                }
            })),
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "shell",
                    "call_id": "call-tool-only",
                    "arguments": "{\"cmd\":\"echo tool-only\"}"
                }
            })),
        ]
        .concat(),
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            fast_event_inserts: false,
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("no real message content"));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn codex_session_jsonl_fast_rejects_metadata_only_without_real_messages() {
    let temp = tempdir();
    let path = temp.path().join("metadata-only-codex-fast.jsonl");
    fs::write(
        &path,
        [
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "codex-fast-metadata-only",
                    "timestamp": "2026-07-03T12:00:00Z",
                    "cwd": "/workspace",
                    "originator": "codex-cli"
                }
            })),
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "shell",
                    "call_id": "call-tool-only",
                    "arguments": "{\"cmd\":\"echo tool-only\"}"
                }
            })),
        ]
        .concat(),
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("no real message content"));
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store
        .search_event_hits("echo tool-only", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn codex_session_jsonl_fast_rejects_malformed_event_timestamp_atomically() {
    let temp = tempdir();
    let path = temp.path().join("bad-timestamp-codex-fast.jsonl");
    fs::write(
        &path,
        [
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": "codex-fast-bad-timestamp",
                    "timestamp": "2026-07-03T12:00:00Z",
                    "cwd": "/workspace",
                    "originator": "codex-cli"
                }
            })),
            jsonl_line(json!({
                "timestamp": "not-rfc3339",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "fast bad timestamp should not import"}
                    ]
                }
            })),
        ]
        .concat(),
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("timestamp is not a valid RFC3339 timestamp"));
    assert!(store.list_sessions().unwrap().is_empty());
    assert!(store
        .search_event_hits("fast bad timestamp should not import", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn codex_session_tail_rejects_malformed_append_atomically() {
    let temp = tempdir();
    let path = temp.path().join("tail-bad-timestamp-codex.jsonl");
    let initial = [
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-tail-bad-timestamp",
                "timestamp": "2026-07-03T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli"
            }
        })),
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "initial tail event"}]
            }
        })),
    ]
    .concat();
    fs::write(&path, &initial).unwrap();
    let tail_start = initial.len() as u64;

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first = import_codex_session_jsonl(
        &path,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);

    fs::write(
        &path,
        [
            initial,
            jsonl_line(json!({
                "timestamp": "2026-07-03T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "codex-tail-rollback-sentinel"}]
                }
            })),
            jsonl_line(json!({
                "timestamp": "not-rfc3339",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "tail bad timestamp"}]
                }
            })),
        ]
        .concat(),
    )
    .unwrap();

    let summary = import_codex_session_jsonl_tail(
        &path,
        tail_start,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:31:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    let session_id = provider_import_session_id_for_path(
        CaptureProvider::Codex,
        "codex_session_jsonl",
        &path,
        "codex-tail-bad-timestamp",
    );
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 1);
    assert_eq!(
        store
            .search_event_hits("initial tail event", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_event_hits("codex-tail-rollback-sentinel", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn codex_session_tail_skips_oversized_required_header_without_failure() {
    let temp = tempdir();
    let path = temp.path().join("tail-oversized-header-codex.jsonl");
    let mut bytes = oversized_jsonl_line();
    let tail_start = bytes.len() as u64;
    bytes.extend_from_slice(
        jsonl_line(json!({
            "timestamp": "2026-07-03T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "tail should not invent header"}]
            }
        }))
        .as_bytes(),
    );
    fs::write(&path, bytes).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl_tail(
        &path,
        tail_start,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-07-03T12:31:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.skipped_sessions, 1);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn provider_command_run_rejects_negative_duration() {
    let event = test_provider_event(EventType::CommandOutput);
    let err = provider_command_run_from_event(ProviderCommandRunInput {
        provider: CaptureProvider::Codex,
        provider_session_id: "duration-session",
        session_id: new_id(),
        source_id: new_id(),
        run_source_id: None,
        history_record_id: None,
        event: &event,
        payload: &json!({
            "command": "cargo test",
            "duration_ms": -1
        }),
        event_hash: "event-hash",
    })
    .unwrap_err();

    assert!(err.to_string().contains("duration_ms must be nonnegative"));
}

#[test]
fn codex_session_tree_applies_default_import_policy_to_tool_outputs() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-rich-sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_tree(
        &fixture,
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T01:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 5);
    assert_eq!(summary.skipped_events, 3);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-rich-session");
    let events = store.events_for_session(session_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall
            && event.payload.to_string().contains("apply_patch")));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::Summary
            && event
                .payload
                .to_string()
                .contains("sample command completed")));

    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("cargo test -p sample -- --token fixture-secret-token"));
    assert!(!rendered.contains("unit tests passed in /workspace/ctx-rich-fixture"));
    assert!(!rendered.contains("*** Begin Patch"));
    assert!(!rendered.contains("old_fixture"));
    assert!(!rendered.contains("new_fixture"));
    assert!(!rendered.contains("patch_apply_end"));
    assert!(!rendered.contains("opaque-private-reasoning-payload"));
}

#[test]
fn codex_default_output_policy_skips_success_and_keeps_failures() {
    let success = br#"{"timestamp":"2026-06-24T01:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-success","output":"Chunk ID: ok\nProcess exited with code 0\nOutput:\nunit tests passed\n"}}"#;
    let failure = br#"{"timestamp":"2026-06-24T01:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-failure","output":"Chunk ID: fail\nProcess exited with code 101\nOutput:\ntest failed\n"}}"#;
    let timeout = br#"{"timestamp":"2026-06-24T01:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-timeout","timed_out":true,"output":"timed out"}}"#;

    assert!(should_skip_codex_tool_output_line(success));
    assert!(!should_skip_codex_tool_output_line(failure));
    assert!(!should_skip_codex_tool_output_line(timeout));
}

#[test]
fn codex_default_line_filter_parses_policy_relevant_lines() {
    let session_meta =
        br#"{"timestamp":"2026-06-24T01:00:00.000Z","type":"session_meta","payload":{"id":"s"}}"#;
    let user_message = br#"{"timestamp":"2026-06-24T01:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"question"}]}}"#;
    let assistant_message = br#"{"timestamp":"2026-06-24T01:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}"#;
    let developer_message = br#"{"timestamp":"2026-06-24T01:00:02.500Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"instruction"}]}}"#;
    let tool_call = br#"{"timestamp":"2026-06-24T01:00:03.000Z","type":"response_item","payload":{"type":"function_call","call_id":"call-1","name":"shell","arguments":"cargo test"}}"#;
    let tool_output = br#"{"timestamp":"2026-06-24T01:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"passed"}}"#;
    let structured_failed_tool_output = br#"{"timestamp":"2026-06-24T01:00:04.500Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-2","output":{"message":{"exitCode":1,"output":"failed"}}}}"#;
    let reasoning = br#"{"timestamp":"2026-06-24T01:00:05.000Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"thinking"}]}}"#;
    let notice = br#"{"timestamp":"2026-06-24T01:00:06.000Z","type":"event_msg","payload":{"type":"task_complete"}}"#;
    let apply_patch = br#"{"timestamp":"2026-06-24T01:00:07.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Update File: crates/ctx-cli/src/main.rs\n@@\n-old\n+new\n*** End Patch","call_id":"call-patch","status":"completed"}}"#;

    for line in [
        session_meta.as_slice(),
        user_message.as_slice(),
        assistant_message.as_slice(),
        developer_message.as_slice(),
        tool_call.as_slice(),
        apply_patch.as_slice(),
        reasoning.as_slice(),
    ] {
        assert!(should_parse_codex_session_line(line));
    }
    assert!(should_parse_codex_session_line(tool_output));
    assert!(should_skip_codex_tool_output_line(tool_output));
    assert!(should_parse_codex_session_line(
        structured_failed_tool_output
    ));
    assert!(!should_skip_codex_tool_output_line(
        structured_failed_tool_output
    ));
    assert!(!should_parse_codex_session_line(notice));
}

#[test]
fn codex_structured_failed_tool_output_creates_failed_diagnostic_event() {
    let payload = json!({
        "type": "function_call_output",
        "call_id": "call-structured-failure",
        "output": {
            "message": {
                "exitCode": 1,
                "output": "structured failed output oracle"
            }
        }
    });
    let event = codex_tool_output_event(
        &payload,
        12,
        DateTime::parse_from_rfc3339("2026-06-24T01:00:04.500Z")
            .unwrap()
            .with_timezone(&Utc),
        &std::collections::BTreeMap::new(),
    )
    .expect("structured failed output should be retained");

    assert_eq!(event.event_type, EventType::ToolOutput);
    let rendered = event.payload.to_string();
    assert!(rendered.contains("structured failed output oracle"));
    assert!(rendered.contains("failed_preview"));
}

#[test]
fn codex_failed_diff_output_omits_raw_diff_preview() {
    let payload = json!({
        "type": "function_call_output",
        "call_id": "call-failed-diff",
        "output": "Process exited with code 1\nOutput:\ndiff --git a/src/lib.rs b/src/lib.rs\n@@\n-old raw diff\n+new raw diff\n"
    });
    let event = codex_tool_output_event(
        &payload,
        13,
        DateTime::parse_from_rfc3339("2026-06-24T01:00:05.000Z")
            .unwrap()
            .with_timezone(&Utc),
        &std::collections::BTreeMap::new(),
    )
    .expect("failed diff output should keep a diagnostic event");

    let rendered = event.payload.to_string();
    assert!(rendered.contains("output omitted"));
    assert!(!rendered.contains("diff --git"));
    assert!(!rendered.contains("old raw diff"));
    assert!(!rendered.contains("new raw diff"));
}

#[test]
fn codex_nested_failed_diff_output_omits_raw_diff_preview() {
    let payload = json!({
        "type": "function_call_output",
        "call_id": "call-nested-failed-diff",
        "output": {
            "message": {
                "exitCode": 1,
                "output": "@@ -1 +1\n-old nested diff\n+new nested diff\n"
            }
        }
    });
    let event = codex_tool_output_event(
        &payload,
        14,
        DateTime::parse_from_rfc3339("2026-06-24T01:00:05.500Z")
            .unwrap()
            .with_timezone(&Utc),
        &std::collections::BTreeMap::new(),
    )
    .expect("nested failed diff output should keep a diagnostic event");

    let rendered = event.payload.to_string();
    assert!(rendered.contains("output omitted"));
    assert!(!rendered.contains("old nested diff"));
    assert!(!rendered.contains("new nested diff"));
}

#[test]
fn codex_default_policy_persists_file_touches_without_raw_patch_text() {
    let temp = tempdir();
    let root = temp.path().join("codex-sessions/2026/06/24");
    fs::create_dir_all(&root).unwrap();
    let fixture = root.join("search-file-touch.jsonl");
    fs::write(
            &fixture,
            concat!(
                "{\"timestamp\":\"2026-06-24T01:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-search-file-touch\",\"cwd\":\"/workspace/ctx\"}}\n",
                "{\"timestamp\":\"2026-06-24T01:00:01.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Please update the CLI.\"}]}}\n",
                "{\"timestamp\":\"2026-06-24T01:00:02.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"apply_patch\",\"input\":\"*** Begin Patch\\n*** Update File: crates/ctx-cli/src/main.rs\\n@@\\n-old\\n+new\\n*** End Patch\",\"call_id\":\"call-patch\",\"status\":\"completed\"}}\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_tree(
        temp.path().join("codex-sessions"),
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(temp.path().join("codex-sessions")),
            imported_at: "2026-06-24T02:00:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 2);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-search-file-touch");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, EventType::Message);
    assert_eq!(events[1].event_type, EventType::ToolCall);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("file touches: modified:crates/ctx-cli/src/main.rs"));
    assert!(!rendered.contains("*** Begin Patch"));
    assert!(!rendered.contains("-old"));
    assert!(!rendered.contains("+new"));

    let archive = store.export_archive().unwrap();
    let touched = archive
        .files_touched
        .iter()
        .find(|file| file.path == "crates/ctx-cli/src/main.rs")
        .expect("apply_patch should create file touch metadata");
    assert_eq!(touched.change_kind, Some(FileChangeKind::Modified));
    assert!(touched.event_id.is_some());
    assert_eq!(touched.history_record_id, None);
}

#[test]
fn codex_default_policy_redacts_non_patch_edit_tool_arguments() {
    let temp = tempdir();
    let root = temp.path().join("codex-sessions/2026/06/24");
    fs::create_dir_all(&root).unwrap();
    let fixture = root.join("edit-tool.jsonl");
    fs::write(
        &fixture,
        concat!(
            "{\"timestamp\":\"2026-06-24T01:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-edit-tool\",\"cwd\":\"/workspace/ctx\"}}\n",
            "{\"timestamp\":\"2026-06-24T01:00:01.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Please edit the file.\"}]}}\n",
            "{\"timestamp\":\"2026-06-24T01:00:02.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"edit_file\",\"arguments\":{\"path\":\"src/edit_tool.rs\",\"old_string\":\"old-edit-tool-secret\",\"new_string\":\"new-edit-tool-secret\"},\"call_id\":\"call-edit\"}}\n",
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_tree(
        temp.path().join("codex-sessions"),
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(temp.path().join("codex-sessions")),
            imported_at: "2026-06-24T02:00:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 2);

    let session_id = stored_provider_session_id(&store, CaptureProvider::Codex, "codex-edit-tool");
    let events = store.events_for_session(session_id).unwrap();
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("file touches:"));
    assert!(rendered.contains("src/edit_tool.rs"));
    assert!(!rendered.contains("old-edit-tool-secret"));
    assert!(!rendered.contains("new-edit-tool-secret"));
    assert!(store
        .search_event_hits("old-edit-tool-secret", 10)
        .unwrap()
        .is_empty());

    let archive = store.export_archive().unwrap();
    assert!(archive
        .files_touched
        .iter()
        .any(|file| file.path == "src/edit_tool.rs"));
}

#[test]
fn structured_file_touch_extractor_reads_nested_provider_paths() {
    let event = ProviderEventEnvelope {
        provider_event_index: 7,
        provider_event_hash: None,
        cursor: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-06-24T01:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: serde_json::json!({}),
        metadata: serde_json::json!({}),
    };
    let antigravity = serde_json::json!({
        "type": "CODE_ACTION",
        "tool_calls": [{
            "name": "write_to_file",
            "args": {
                "TargetFile": "/workspace/demo/README.md",
                "CodeContent": "# Demo\n"
            }
        }]
    });
    let cursor = serde_json::json!({
        "role": "assistant",
        "message": {
            "content": [{
                "type": "tool_use",
                "name": "write_file",
                "input": {
                    "path": "cursor-native-cli-oracle.txt",
                    "content": "proof"
                }
            }]
        }
    });

    let antigravity_touches = provider_file_touches_from_raw_value(
        CaptureProvider::Antigravity,
        "agy-session",
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
        None,
        &antigravity,
        &event,
        1,
    );
    let cursor_touches = provider_file_touches_from_raw_value(
        CaptureProvider::Cursor,
        "cursor-session",
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        None,
        &cursor,
        &event,
        1,
    );

    assert_eq!(antigravity_touches[0].1.path, "/workspace/demo/README.md");
    assert_eq!(
        antigravity_touches[0].1.change_kind,
        Some(FileChangeKind::Created)
    );
    assert_eq!(cursor_touches[0].1.path, "cursor-native-cli-oracle.txt");
    assert_eq!(
        cursor_touches[0].1.change_kind,
        Some(FileChangeKind::Created)
    );
}

#[test]
fn structured_file_touch_extractor_covers_provider_tool_shapes() {
    let event = ProviderEventEnvelope {
        provider_event_index: 11,
        provider_event_hash: None,
        cursor: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-06-24T01:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: serde_json::json!({}),
        metadata: serde_json::json!({}),
    };

    for (provider, source_format, raw, expected_path) in [
        (
            CaptureProvider::Claude,
            CLAUDE_PROJECTS_SOURCE_FORMAT,
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "name": "Edit",
                        "input": {"file_path": "src/claude_file.rs"}
                    }]
                }
            }),
            "src/claude_file.rs",
        ),
        (
            CaptureProvider::OpenCode,
            OPENCODE_SQLITE_SOURCE_FORMAT,
            serde_json::json!({
                "content": [{
                    "type": "tool",
                    "name": "write",
                    "input": {"file": "src/opencode_file.rs"}
                }]
            }),
            "src/opencode_file.rs",
        ),
        (
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            serde_json::json!({
                "type": "gemini",
                "toolCalls": [{
                    "name": "write_file",
                    "args": {"path": "src/gemini_file.rs", "content": "proof"}
                }]
            }),
            "src/gemini_file.rs",
        ),
        (
            CaptureProvider::CopilotCli,
            COPILOT_CLI_SOURCE_FORMAT,
            serde_json::json!({
                "type": "tool.execution_start",
                "data": {
                    "toolName": "write_file",
                    "args": {"path": "src/copilot_file.rs"}
                }
            }),
            "src/copilot_file.rs",
        ),
        (
            CaptureProvider::FactoryAiDroid,
            FACTORY_DROID_SOURCE_FORMAT,
            serde_json::json!({
                "type": "message",
                "content": [{
                    "type": "tool_use",
                    "name": "write_file",
                    "input": {"path": "src/droid_file.rs"}
                }]
            }),
            "src/droid_file.rs",
        ),
        (
            CaptureProvider::ForgeCode,
            FORGECODE_SQLITE_SOURCE_FORMAT,
            serde_json::json!({
                "message": {
                    "text": {
                        "tool_calls": [{
                            "name": "write",
                            "arguments": {
                                "path": "src/forge_file.rs",
                                "content": "proof"
                            }
                        }]
                    }
                }
            }),
            "src/forge_file.rs",
        ),
    ] {
        let touches = provider_file_touches_from_raw_value(
            provider,
            "provider-session",
            source_format,
            None,
            &raw,
            &event,
            1,
        );
        assert_eq!(
            touches.first().map(|(_, file)| file.path.as_str()),
            Some(expected_path),
            "{provider:?} should extract an explicit tool file path"
        );
    }
}
