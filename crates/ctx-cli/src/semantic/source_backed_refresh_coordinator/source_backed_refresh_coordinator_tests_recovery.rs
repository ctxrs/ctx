use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use ctx_history_capture::{
    DiscoveryPlatform, DiscoveryPlatformDirs, ProviderCatalogSupport, ProviderImportSupport,
    ProviderSource, ProviderSourceKind,
};

use super::*;

fn empty_publication(
    generation_id: String,
    catalog: ExplicitSourceCatalogAuthority,
) -> SourceBackedRefreshPublication {
    SourceBackedRefreshPublication {
        generation_id,
        published_explicit_source_catalog: catalog,
        source_manifest: None,
        resolver: None,
        scanned_routes: 0,
        unsupported_routes: 0,
        certified_source_count: 0,
        certified_source_bytes: 0,
        current: SourceBackedRefreshCurrent::default(),
        timings: SourceBackedRefreshTimings {
            discovery_us: 1,
            scan_stage_us: 1,
            commit_us: 1,
        },
    }
}

fn force_crash_after_empty_publication(data_root: &Path) -> String {
    let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(
        |execution: SourceBackedRefreshExecution<'_>| {
            let writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?;
            let receipt = writer.commit(|_| true)?;
            execution.report_progress("committed", 0, 0, None)?;
            panic!(
                "forced crash after lexical publication {}",
                receipt.generation_id
            );
        },
    ));
    coordinator.enqueue_periodic(data_root).unwrap();

    let crash = catch_unwind(AssertUnwindSafe(|| coordinator.run_next(data_root)));
    assert!(
        crash.is_err(),
        "the publication crash window must be forced"
    );
    let index = VerifiedIndex::open_pinned(source_backed_index_root(data_root)).unwrap();
    let job = read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root)).unwrap();
    assert_eq!(job["request_state"], "running");
    assert_eq!(job["progress"]["phase"], "committed");
    index.generation_id().to_owned()
}

fn write_custom_source(path: &Path, source_id: &str, marker: &str) -> ProviderSource {
    let records = [
        json!({
            "record_type": "manifest",
            "schema_version": "ctx-history-jsonl-v1",
        }),
        json!({
            "record_type": "source",
            "source_id": source_id,
            "provider_key": "catalog-provider",
            "source_format": "catalog-jsonl",
        }),
        json!({
            "record_type": "session",
            "source_id": source_id,
            "session_id": format!("{source_id}-session"),
            "started_at": "2026-07-30T12:00:00Z",
        }),
        json!({
            "record_type": "event",
            "source_id": source_id,
            "session_id": format!("{source_id}-session"),
            "event_index": 0,
            "event_type": "message",
            "role": "user",
            "occurred_at": "2026-07-30T12:00:01Z",
            "payload": {"text": marker},
            "preview": marker,
        }),
    ];
    fs::write(
        path,
        records
            .into_iter()
            .map(|record| record.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    ProviderSource {
        provider: CaptureProvider::Custom,
        path: path.to_owned(),
        exists: true,
        source_format: "ctx_history_jsonl_v1",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    }
}

fn publish_nonempty_catalog_generation(
    temp: &tempfile::TempDir,
    data_root: &Path,
    source: &ProviderSource,
) -> (String, ExplicitSourceCatalogAuthority) {
    let authority = crate::commands::import::upsert_explicit_source(data_root, source)
        .unwrap()
        .authority;
    let home = temp.path().join("fixture-home");
    let cwd = temp.path().join("fixture-cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let discovery = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let mut progress = |_: CaptureSourceBackedRefreshProgress| Ok::<(), SourceBackedRouteError>(());
    let publication = refresh_all_provider_sources(
        &discovery,
        DiscoveryReport {
            sources: Vec::new(),
            issues: Vec::new(),
        },
        StdDuration::ZERO,
        data_root,
        &source_backed_index_root(data_root),
        Some(&authority),
        &mut progress,
    )
    .unwrap();
    assert_eq!(publication.certified_source_count, 1);
    let generation_id = publication.generation_id.clone();
    let coordinator = SourceBackedRefreshCoordinator::new();
    coordinator.enqueue_periodic(data_root).unwrap();
    let run = coordinator
        .run_next_with(
            |_, _| Ok(publication),
            || Ok(Some(generation_id.clone())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), &run.job).unwrap();
    (generation_id, authority)
}

#[test]
fn verified_generation_recovers_forced_crash_to_terminal_receipt_and_pending_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let generation_id = force_crash_after_empty_publication(&data_root);

    reconcile_verified_source_epoch(&data_root).unwrap();
    reconcile_verified_source_epoch(&data_root).unwrap();

    let job = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root)).unwrap();
    assert_eq!(job["status"], "completed");
    assert_eq!(job["request_state"], "published");
    assert_eq!(job["published_generation"], generation_id);
    assert_eq!(job["receipt"]["published_generation"], generation_id);
    assert_eq!(job["generation_changed"], true);
    assert_eq!(
        job["published_explicit_source_catalog"],
        job["receipt"]["published_explicit_source_catalog"]
    );
    assert_eq!(job["receipt"]["current"]["current_source_count"], 0);
    assert_eq!(job["receipt"]["current"]["current_indexed_documents"], 0);

    let status =
        crate::semantic::source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();
    assert_eq!(status.report["lexical"]["status"], "ready");
    assert_eq!(status.report["lexical"]["generation_id"], generation_id);
    assert_eq!(status.report["catalog"]["status"], "ready");
    assert_eq!(status.report["refresh"]["status"], "ready");
    assert_eq!(status.report["relational"]["status"], "pending");
}

#[test]
fn committed_noop_crash_recovers_same_generation_then_replays_without_identity_churn() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let initial = ctx_history_index::GenerationWriter::open(&index_root, WriterOptions::default())
        .unwrap()
        .commit(|_| true)
        .unwrap()
        .generation_id;

    let recovered_generation = force_crash_after_empty_publication(&data_root);
    assert_eq!(recovered_generation, initial);
    reconcile_verified_source_epoch(&data_root).unwrap();
    let recovered =
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root)).unwrap();
    assert_eq!(recovered["status"], "completed");
    assert_eq!(recovered["generation_changed"], false);
    assert_eq!(recovered["published_generation"], initial);

    let calls = Arc::new(AtomicUsize::new(0));
    let executor_calls = Arc::clone(&calls);
    let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            executor_calls.fetch_add(1, Ordering::SeqCst);
            let receipt = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .commit(|_| true)?;
            execution.report_progress("committed", 0, 0, None)?;
            let catalog = execution
                .explicit_source_catalog
                .cloned()
                .ok_or_else(|| anyhow!("replay catalog authority was not frozen"))?;
            Ok(empty_publication(receipt.generation_id, catalog))
        },
    ));
    coordinator.enqueue_periodic(&data_root).unwrap();
    let replay = coordinator.run_next(&data_root).expect("no-op replay");

    assert!(!replay.failed);
    assert!(!replay.did_work);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(replay.job["published_generation"], initial);
    assert_eq!(replay.job["receipt"]["published_generation"], initial);
    assert_eq!(replay.job["generation_changed"], false);
    let persisted =
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root)).unwrap();
    assert_eq!(persisted["request_state"], "published");
    assert_eq!(persisted["receipt"]["published_generation"], initial);
    assert_eq!(
        VerifiedIndex::open_pinned(&index_root)
            .unwrap()
            .generation_id(),
        initial
    );
}

#[test]
fn changed_nonempty_catalog_recovers_only_generation_bound_a_and_queues_b() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let source_a_path = temp.path().join("catalog-a.jsonl");
    let source_a = write_custom_source(&source_a_path, "catalog-a", "catalog-a-marker");
    let (generation_id, catalog_a) =
        publish_nonempty_catalog_generation(&temp, &data_root, &source_a);

    fs::remove_file(&source_a_path).unwrap();
    let source_b_path = temp.path().join("catalog-b.jsonl");
    let source_b = write_custom_source(&source_b_path, "catalog-b", "catalog-b-marker");
    let catalog_b = crate::commands::import::upsert_explicit_source(&data_root, &source_b)
        .unwrap()
        .authority;
    write_custom_source(&source_a_path, "catalog-a", "catalog-a-marker");
    assert_ne!(catalog_a, catalog_b);

    let coordinator = SourceBackedRefreshCoordinator::new();
    coordinator.recover_published_resolver(&data_root).unwrap();

    let retained = coordinator
        .retained_published_generation()
        .expect("generation A resolver remains safely reconstructable");
    assert_eq!(retained.generation_id(), generation_id);
    assert_eq!(
        retained
            .published_explicit_source_catalog()
            .expect("recovered resolver catalog authority"),
        &catalog_a
    );
    assert_ne!(
        retained.published_explicit_source_catalog(),
        Some(&catalog_b)
    );
    assert_eq!(
        retained
            .source_manifest()
            .expect("generation-bound source manifest")
            .core_generation_id,
        generation_id
    );
    assert!(coordinator.has_pending_request());
    let queued =
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root)).unwrap();
    assert_eq!(queued["request_state"], "queued");
    assert_eq!(
        queued["requested_explicit_source_catalog"],
        catalog_b.to_json()
    );
    assert_eq!(
        queued["retained_publication"]["published_explicit_source_catalog"],
        catalog_a.to_json()
    );
    let status =
        crate::semantic::source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();
    assert_eq!(status.report["lexical"]["status"], "ready");
    assert_eq!(status.report["catalog"]["status"], "pending");
    assert_eq!(status.report["resolver"]["status"], "pending");
    assert_eq!(status.report["refresh"]["status"], "pending");
}

#[test]
fn deleted_published_source_never_installs_live_b_for_generation_a() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let source_a_path = temp.path().join("catalog-a.jsonl");
    let source_a = write_custom_source(&source_a_path, "catalog-a", "catalog-a-marker");
    let (generation_id, catalog_a) =
        publish_nonempty_catalog_generation(&temp, &data_root, &source_a);

    fs::remove_file(&source_a_path).unwrap();
    let source_b_path = temp.path().join("catalog-b.jsonl");
    let source_b = write_custom_source(&source_b_path, "catalog-b", "catalog-b-marker");
    let catalog_b = crate::commands::import::upsert_explicit_source(&data_root, &source_b)
        .unwrap()
        .authority;
    assert_ne!(catalog_a, catalog_b);

    let coordinator = SourceBackedRefreshCoordinator::new();
    coordinator.recover_published_resolver(&data_root).unwrap();

    assert!(
        coordinator.retained_published_generation().is_none(),
        "live catalog B must not be installed for immutable generation A"
    );
    assert!(coordinator.has_pending_request());
    let queued =
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root)).unwrap();
    assert_eq!(queued["request_state"], "queued");
    assert_eq!(queued["published_generation"], generation_id);
    assert_eq!(
        queued["requested_explicit_source_catalog"],
        catalog_b.to_json()
    );
    assert_eq!(
        queued["retained_publication"]["published_explicit_source_catalog"],
        catalog_a.to_json()
    );
    let status =
        crate::semantic::source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();
    assert_eq!(status.report["lexical"]["status"], "ready");
    assert_eq!(status.report["catalog"]["status"], "pending");
    assert_eq!(status.report["resolver"]["status"], "pending");
    assert_eq!(status.report["refresh"]["status"], "pending");
}

#[test]
fn repeated_same_generation_recovery_coalesces_exactly_one_noop_replay() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let generation_id =
        ctx_history_index::GenerationWriter::open(&index_root, WriterOptions::default())
            .unwrap()
            .commit(|_| true)
            .unwrap()
            .generation_id;
    let catalog = load_explicit_source_catalog_authority(&data_root).unwrap();
    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(&data_root),
        &json!({
            "mode": "background",
            "owner": "daemon",
            "kind": "source_backed",
            "status": "running",
            "request_id": "same-generation-without-commit",
            "request_state": "running",
            "previous_generation": generation_id,
            "published_generation": generation_id,
            "requested_explicit_source_catalog": catalog.to_json(),
            "progress": {
                "phase": "refreshing",
                "completed_sources": 0,
                "total_sources": 0,
            },
            "daemon_mode": "source-refresh-only",
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
        }),
    )
    .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let executor_calls = Arc::clone(&calls);
    let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            executor_calls.fetch_add(1, Ordering::SeqCst);
            let receipt = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .commit(|_| true)?;
            let catalog = execution
                .explicit_source_catalog
                .cloned()
                .ok_or_else(|| anyhow!("recovered no-op catalog authority was not frozen"))?;
            Ok(empty_publication(receipt.generation_id, catalog))
        },
    ));

    coordinator.recover_published_resolver(&data_root).unwrap();
    let first = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root)).unwrap();
    coordinator.recover_published_resolver(&data_root).unwrap();
    let second =
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root)).unwrap();

    assert_eq!(first["request_id"], second["request_id"]);
    assert_eq!(first["coalesced_requests"], 0);
    assert_eq!(second["coalesced_requests"], 1);
    assert_eq!(second["request_state"], "queued");
    let replay = coordinator.run_next(&data_root).expect("one no-op replay");
    assert!(!replay.failed);
    assert!(!replay.did_work);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(replay.job["published_generation"], generation_id);
    assert_eq!(
        VerifiedIndex::open_pinned(index_root)
            .unwrap()
            .generation_id(),
        generation_id
    );
    assert!(!coordinator.has_pending_request());
}

#[test]
fn interrupted_unpublished_refresh_is_restored_as_real_queued_work() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let job_path = daemon_source_backed_refresh_job_path(&data_root);
    let catalog = load_explicit_source_catalog_authority(&data_root).unwrap();
    write_daemon_job_status(
        &job_path,
        &json!({
            "mode": "background",
            "owner": "daemon",
            "kind": "source_backed",
            "status": "running",
            "request_id": "interrupted-request",
            "request_state": "running",
            "requested_explicit_source_catalog": catalog.to_json(),
            "progress": {
                "phase": "refreshing",
                "completed_sources": 0,
                "total_sources": 1,
            },
            "daemon_mode": "source-refresh-only",
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
        }),
    )
    .unwrap();

    reconcile_verified_source_epoch(&data_root).unwrap();
    let queued = read_daemon_job_status(&job_path).unwrap();
    assert_eq!(queued["request_state"], "queued");
    assert_eq!(queued["progress"]["phase"], "queued");

    let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(
        |execution: SourceBackedRefreshExecution<'_>| {
            let receipt = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .commit(|_| true)?;
            execution.report_progress("committed", 0, 0, None)?;
            let catalog = execution
                .explicit_source_catalog
                .cloned()
                .ok_or_else(|| anyhow!("recovered catalog authority was not restored"))?;
            Ok(empty_publication(receipt.generation_id, catalog))
        },
    ));
    coordinator.recover_published_resolver(&data_root).unwrap();
    assert!(coordinator.has_pending_request());
    let restored = read_daemon_job_status(&job_path).unwrap();
    assert_eq!(restored["request_state"], "queued");
    assert_ne!(restored["request_id"], "interrupted-request");

    let run = coordinator.run_next(&data_root).expect("recovered replay");
    assert!(!run.failed);
    assert_eq!(run.job["request_state"], "published");
    assert!(!coordinator.has_pending_request());
}
