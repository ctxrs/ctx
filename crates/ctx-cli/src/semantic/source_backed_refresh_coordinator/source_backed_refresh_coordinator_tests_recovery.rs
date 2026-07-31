use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
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
