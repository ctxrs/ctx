use crate::provider::codex::session::join_codex_import_worker;
use crate::test_support_paths::capture_repo_root;
use crate::tests::codex::catalog_harness::{
    elapsed_ms, incremental_codex_catch_up, incremental_perf_file_count,
    incremental_perf_noop_p95_threshold_ms, incremental_perf_noop_us_per_file_threshold,
    incremental_perf_repeats, rounded, synthetic_codex_session_tree, timing_stats,
    write_synthetic_codex_session,
};
use crate::tests::support::fixtures::jsonl::write_oversized_jsonl_line;
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{
    catalog_codex_session_tree, import_codex_session_tree, CaptureError,
    CodexSessionCatalogOptions, CodexSessionImportOptions,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, CaptureProvider, EventRole, EventType, Fidelity};
use ctx_history_store::Store;
use serde_json::json;
use std::fs;
use std::path::PathBuf;

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
    assert_eq!(first.imported_events, 8);
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
    assert_eq!(second.skipped_edges, 0);

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
    assert_eq!(parent_events.len(), 6);
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
fn codex_parallel_join_panic_is_a_typed_system_failure() {
    let error = std::thread::scope(|scope| {
        let handle = scope.spawn(|| -> crate::Result<()> {
            panic!("intentional Codex normalization worker panic")
        });
        join_codex_import_worker(handle)
    })
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::WorkerPanicked("Codex import")
    ));
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
fn codex_catalog_re_pends_unchanged_sources_from_older_normalization_revisions() {
    let temp = tempdir();
    let root = temp.path().join("sessions");
    synthetic_codex_session_tree(&root, 1);
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source_root = root.display().to_string();

    catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-07-18T12:00:00Z".parse().unwrap(),
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    let mut legacy = store
        .list_catalog_sessions_for_source(CaptureProvider::Codex, &source_root)
        .unwrap()
        .pop()
        .unwrap();
    legacy
        .metadata
        .as_object_mut()
        .unwrap()
        .remove("normalization_capture_revision");
    legacy
        .metadata
        .as_object_mut()
        .unwrap()
        .remove("normalization_policy_revision");
    store.upsert_catalog_sessions(&[legacy.clone()]).unwrap();
    store
        .mark_catalog_source_observation_indexed(
            &legacy,
            None,
            Some(1),
            "2026-07-18T12:00:30Z"
                .parse::<DateTime<Utc>>()
                .unwrap()
                .timestamp_millis(),
        )
        .unwrap();
    assert!(store
        .list_pending_catalog_sessions_without_local_projection(
            CaptureProvider::Codex,
            &source_root
        )
        .unwrap()
        .is_empty());

    let repaired = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-07-18T12:01:00Z".parse().unwrap(),
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(repaired.cached_sessions, 0);
    assert_eq!(repaired.parsed_sessions, 1);
    let pending = store
        .list_pending_catalog_sessions_without_local_projection(
            CaptureProvider::Codex,
            &source_root,
        )
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].metadata["normalization_capture_revision"], 7);
    assert_eq!(pending[0].metadata["normalization_policy_revision"], 3);

    let cached = catalog_codex_session_tree(
        &root,
        &store,
        CodexSessionCatalogOptions {
            source_root: Some(root.clone()),
            cataloged_at: "2026-07-18T12:02:00Z".parse().unwrap(),
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    assert_eq!(cached.cached_sessions, 1);
    assert_eq!(cached.parsed_sessions, 0);
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
