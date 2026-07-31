use std::{path::Path, sync::mpsc, thread, time::Duration};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, EventIdentityInput, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceObservation, SourceRecordLocator,
    TypedKey,
};
use ctx_history_index::{GenerationWriter, LexicalDocument, WriterOptions};
use serde_json::{json, Value};

use crate::{
    config::{AppConfig, DaemonMode},
    output::JsonOutputFormat,
    semantic::{
        daemon::{install_daemon_test_job_hooks, DaemonTestJobHooks},
        source_backed_refresh_coordinator::{
            coordinate_source_backed_refresh, source_backed_index_root,
            SourceBackedRefreshCoordinator, SourceBackedRefreshCurrent,
            SourceBackedRefreshExecution, SourceBackedRefreshMode, SourceBackedRefreshPublication,
            SourceBackedRefreshTimings,
        },
        source_epoch_status_report,
    },
    DaemonRunArgs,
};

use super::{
    daemon_job_should_backoff, daemon_mode_runs_source_backed_pro_catch_up,
    daemon_mode_runs_source_backed_relational_catch_up,
    daemon_mode_runs_source_backed_semantic_projection, daemon_semantic_job_path,
    daemon_source_backed_refresh_job_path, persist_pro_status, persist_relational_status,
    prepare_pro_retry_for_generation, read_daemon_job_status, read_pro_status,
    record_daemon_job_retry, restore_daemon_consumer_retries, run_daemon_once_with_activity,
    run_pro_catch_up_with_retry, write_daemon_job_status, DaemonRetryBackoff, DaemonRuntime,
};

const READINESS_QUERY: &str = "readiness-boundary-regression";

fn daemon_args() -> DaemonRunArgs {
    DaemonRunArgs {
        foreground: false,
        once: false,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: None,
        trigger_command: None,
        format: JsonOutputFormat::Json,
    }
}

fn publish_empty_core_generation(data_root: &Path) -> String {
    ctx_history_index::GenerationWriter::open(
        source_backed_index_root(data_root),
        ctx_history_index::WriterOptions::default(),
    )
    .unwrap()
    .commit(|_| true)
    .unwrap()
    .generation_id
}

fn publish_empty_authoritative_generation(index_root: &Path) -> SourceBackedRefreshPublication {
    let receipt = GenerationWriter::open(index_root, WriterOptions::default())
        .unwrap()
        .commit(|_| true)
        .unwrap();
    SourceBackedRefreshPublication {
        generation_id: receipt.generation_id.clone(),
        published_explicit_source_catalog:
            crate::commands::import::load_explicit_source_catalog_authority(index_root).unwrap(),
        source_manifest: Some(
            ctx_pro_host_protocol::SourceManifest::new(
                receipt.generation_id,
                Vec::new(),
                Vec::new(),
            )
            .unwrap(),
        ),
        resolver: Some(std::sync::Arc::new(
            ctx_history_capture::SourceBackedProviderRegistry::new().resolver_registry(),
        )),
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

fn readiness_source() -> ctx_history_core::SourceKey {
    ctx_history_core::SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("readiness-boundary.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn readiness_document(source: &ctx_history_core::SourceKey) -> LexicalDocument {
    let native_session = TypedKey::utf8("readiness-session").unwrap();
    let session_key = NativeSessionKey::native_id("session", native_session.clone()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item =
        NativeItemKey::native_id("message", TypedKey::utf8("readiness-event").unwrap()).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item,
        subrecord_selector: None,
    })
    .unwrap();
    LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator: SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: 0,
                byte_length: 128,
                physical_ordinal: 0,
                native_session_key: Some(native_session),
                native_event_key: Some(TypedKey::U64(0)),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            [9; 32],
        )
        .unwrap(),
        provider_session_id: Some("readiness-session".to_owned()),
        branch: Some("main".to_owned()),
        source_path: Some("/provider/codex/readiness-boundary.jsonl".to_owned()),
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: 0,
        occurred_at_unix_ms: Some(1_700_000_000_000),
        event_type: "message".to_owned(),
        role: Some("assistant".to_owned()),
        body: format!("exact lexical hit for {READINESS_QUERY}"),
        workspace: Some("ctx".to_owned()),
        cwd: Some("/work/ctx".to_owned()),
        touched_files: vec!["src/lib.rs".to_owned()],
    }
}

fn readiness_certificate(source: &ctx_history_core::SourceKey) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "codex-parser-v1",
        [7; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 128,
            ..ScannedSourceCounts::default()
        },
        Some(SourceFrontier::new("jsonl-byte-offset", TypedKey::U64(128), 128, [7; 32]).unwrap()),
    )
    .unwrap()
}

fn publish_readiness_generation(index_root: &Path) -> SourceBackedRefreshPublication {
    let source = readiness_source();
    let mut writer = GenerationWriter::open(
        index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_document(readiness_document(&source)).unwrap();
    writer
        .certify_source(readiness_certificate(&source))
        .unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    SourceBackedRefreshPublication {
        generation_id: receipt.generation_id,
        published_explicit_source_catalog:
            crate::commands::import::load_explicit_source_catalog_authority(index_root).unwrap(),
        source_manifest: None,
        resolver: None,
        scanned_routes: 1,
        unsupported_routes: 0,
        certified_source_count: 1,
        certified_source_bytes: 128,
        current: SourceBackedRefreshCurrent {
            source_count: 1,
            indexed_documents: 1,
            complete_records: 1,
            retained_records: 1,
            certified_source_bytes: 128,
            ..SourceBackedRefreshCurrent::default()
        },
        timings: SourceBackedRefreshTimings {
            discovery_us: 1,
            scan_stage_us: 2,
            commit_us: 3,
        },
    }
}

fn pending_relational_status(generation: &str) -> Value {
    json!({
        "schema_version": 1,
        "owner": "daemon",
        "kind": "source_backed_relational_catch_up",
        "status": "pending",
        "pending": true,
        "retryable": true,
        "core_generation_id": generation,
        "active_core_generation_id": null,
        "receipt_core_generation_id": null,
        "projection_status": null,
        "build_generation": null,
        "attempts": 1,
        "last_attempt_at_ms": 1,
        "error_code": null,
        "last_error": null,
    })
}

fn pinned_generation(data_root: &Path) -> String {
    coordinate_source_backed_refresh(data_root, SourceBackedRefreshMode::Off)
        .unwrap()
        .pin
        .generation_id()
        .to_owned()
}

fn relational_status(generation: &str, status: &str) -> Value {
    let completed = status == "completed";
    json!({
        "schema_version": 1,
        "owner": "daemon",
        "kind": "source_backed_relational_catch_up",
        "status": status,
        "pending": !completed,
        "retryable": !completed,
        "core_generation_id": generation,
        "active_core_generation_id": completed.then_some(generation),
        "receipt_core_generation_id": completed.then_some(generation),
        "projection_status": completed.then_some("ready"),
        "build_generation": completed.then_some(1),
        "attempts": 1,
        "last_attempt_at_ms": 1,
        "error_code": (!completed).then_some("injected_failure"),
        "last_error": (!completed).then_some("injected relational failure"),
    })
}

#[test]
fn core_publication_is_ready_and_searchable_while_relational_receipt_is_pending() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().to_path_buf();
    let worker_root = data_root.clone();
    let (started_sender, started_receiver) = mpsc::sync_channel(1);
    let (release_sender, release_receiver) = mpsc::channel();

    let worker = thread::spawn(move || {
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _hooks = install_daemon_test_job_hooks(DaemonTestJobHooks {
            calls: calls.clone(),
            history_refresh: None,
            relational_projection: Some(json!({
                "status": "completed",
                "pending": false,
                "retryable": false,
                "did_work": true,
            })),
            semantic_index: None,
            relational_blocker: Some(std::rc::Rc::new(move || {
                started_sender
                    .send(())
                    .expect("report blocked relational test job");
                release_receiver
                    .recv_timeout(Duration::from_secs(10))
                    .expect("release blocked relational test job");
            })),
        });
        let coordinator = SourceBackedRefreshCoordinator::with_executor(std::sync::Arc::new(
            move |execution: SourceBackedRefreshExecution<'_>| {
                let publication = publish_readiness_generation(execution.index_root);
                execution.report_progress("committed", 1, 1, None)?;
                persist_relational_status(
                    execution.data_root,
                    &pending_relational_status(&publication.generation_id),
                )?;
                Ok(publication)
            },
        ));
        let mut runtime = DaemonRuntime::default();
        let core = run_daemon_once_with_activity(
            &daemon_args(),
            &worker_root,
            &mut runtime,
            None,
            false,
            None,
            Some(&coordinator),
        )
        .unwrap();
        let sidecar = run_daemon_once_with_activity(
            &daemon_args(),
            &worker_root,
            &mut runtime,
            None,
            false,
            None,
            Some(&coordinator),
        )
        .unwrap();
        let calls = calls.borrow().clone();
        (
            core,
            sidecar,
            calls,
            runtime.sidecar_drain.generation.clone(),
        )
    });

    started_receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("relational catch-up did not reach the deterministic blocker");

    let core_job = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("Core terminal receipt");
    let relational_job =
        super::read_relational_status(&data_root).expect("relational pending receipt");
    let refresh_off =
        coordinate_source_backed_refresh(&data_root, SourceBackedRefreshMode::Off).unwrap();
    let pinned_generation = refresh_off.pin.generation_id().to_owned();
    let hits = refresh_off
        .pin
        .into_index()
        .search_event_candidates(READINESS_QUERY, 10)
        .unwrap();
    let status = source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();

    release_sender
        .send(())
        .expect("release relational catch-up");
    let (core, sidecar, calls, drain_generation) =
        worker.join().expect("join blocked relational worker");

    let published_generation = core_job["published_generation"]
        .as_str()
        .expect("published generation");
    assert_eq!(core_job["status"], "completed", "{core_job:#}");
    assert_eq!(core_job["request_state"], "published", "{core_job:#}");
    assert_eq!(core_job["progress"]["phase"], "published", "{core_job:#}");
    assert_eq!(core_job["progress"]["completed_sources"], 1);
    assert_eq!(core_job["progress"]["total_sources"], 1);
    assert!(core_job.get("relational_projection").is_none());
    assert!(core_job.get("pro_projection").is_none());
    assert!(core_job.get("semantic_projection").is_none());

    assert_eq!(relational_job["status"], "pending", "{relational_job:#}");
    assert_eq!(relational_job["attempts"], 1);
    assert_eq!(
        relational_job["core_generation_id"], published_generation,
        "{relational_job:#}"
    );
    assert_eq!(pinned_generation, published_generation);
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].event.provider_session_id.as_deref(),
        Some("readiness-session")
    );

    assert_eq!(
        status.report["lexical"]["status"], "ready",
        "{:#}",
        status.report
    );
    assert_eq!(
        status.report["lexical"]["generation_id"],
        published_generation
    );
    assert_eq!(status.report["refresh"]["status"], "ready");
    assert_eq!(status.report["refresh"]["request_state"], "published");
    assert_eq!(status.report["relational"]["status"], "pending");
    assert_eq!(status.report["relational"]["catch_up"]["status"], "pending");
    assert_eq!(
        status.report["relational"]["catch_up"]["core_generation_id"],
        published_generation
    );

    assert!(core.did_work);
    assert!(!core.failed);
    assert!(core.continue_immediately);
    assert!(!sidecar.failed);
    assert!(sidecar.continue_immediately);
    assert_eq!(calls, vec!["relational_projection"]);
    assert_eq!(drain_generation.as_deref(), Some(published_generation));
}

fn install_jobs(
    calls: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    relational_projection: Option<Value>,
    semantic_index: Option<Value>,
) -> super::super::daemon::DaemonTestJobHookGuard {
    install_daemon_test_job_hooks(DaemonTestJobHooks {
        calls,
        history_refresh: None,
        relational_projection,
        semantic_index,
        relational_blocker: None,
    })
}

#[test]
fn core_only_once_then_persistent_scheduler_drains_relational_before_optional_sidecars() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = SourceBackedRefreshCoordinator::with_executor(std::sync::Arc::new(
        |execution: SourceBackedRefreshExecution<'_>| {
            Ok(publish_empty_authoritative_generation(execution.index_root))
        },
    ));
    let mut runtime = DaemonRuntime::default();
    let mut once_args = daemon_args();
    once_args.once = true;
    let core = run_daemon_once_with_activity(
        &once_args,
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        Some(&coordinator),
    )
    .unwrap();
    let core_job =
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(temp.path())).unwrap();
    let generation = core_job["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(core.continue_immediately);
    assert_eq!(
        runtime.sidecar_drain.generation.as_deref(),
        Some(generation.as_str())
    );
    assert!(!coordinator.has_pending_request());
    assert!(super::relational_generation_needs_catch_up(
        temp.path(),
        &generation
    ));
    assert!(super::pro_generation_needs_catch_up(
        temp.path(),
        &generation
    ));
    assert!(super::semantic_generation_needs_catch_up(
        temp.path(),
        &generation
    ));
    assert!(runtime
        .sidecar_drain
        .relational_attempted_generation
        .is_none());
    assert!(runtime.sidecar_drain.pro_attempted_generation.is_none());
    assert!(runtime
        .sidecar_drain
        .semantic_attempted_generation
        .is_none());
    assert!(super::read_relational_status(temp.path()).is_none());
    assert!(read_pro_status(temp.path()).is_none());
    assert!(!daemon_semantic_job_path(temp.path()).exists());
    assert_eq!(pinned_generation(temp.path()), generation);

    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let _hooks = install_jobs(
        calls.clone(),
        Some(relational_status(&generation, "error")),
        Some(json!({
            "status": "ready",
            "source_generation_ready": true,
            "source_work_remaining": false,
        })),
    );

    let relational = run_daemon_once_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(relational.continue_immediately);
    assert_eq!(&*calls.borrow(), &["relational_projection"]);
    assert!(read_pro_status(temp.path()).is_none());
    assert!(!daemon_semantic_job_path(temp.path()).exists());
    assert_eq!(runtime.relational_retry.consecutive_failures, 1);
    assert_eq!(
        runtime
            .sidecar_drain
            .relational_attempted_generation
            .as_deref(),
        Some(generation.as_str())
    );
    assert_eq!(pinned_generation(temp.path()), generation);

    let pro = run_daemon_once_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(pro.continue_immediately);
    assert_eq!(&*calls.borrow(), &["relational_projection"]);
    let pro_status = read_pro_status(temp.path()).expect("Pro attempt receipt");
    assert_eq!(pro_status["status"], "error", "{pro_status:#}");
    assert_eq!(pro_status["retryable"], false, "{pro_status:#}");
    assert_eq!(pro_status["attempts"], 1);
    assert_eq!(pro_status["core_generation_id"], generation);
    assert!(!daemon_semantic_job_path(temp.path()).exists());
    assert_eq!(
        runtime.sidecar_drain.pro_attempted_generation.as_deref(),
        Some(generation.as_str())
    );
    assert_eq!(pinned_generation(temp.path()), generation);

    let semantic = run_daemon_once_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(semantic.continue_immediately);
    assert_eq!(
        &*calls.borrow(),
        &["relational_projection", "semantic_index"]
    );
    let semantic_status = read_daemon_job_status(&daemon_semantic_job_path(temp.path())).unwrap();
    assert_eq!(semantic_status["status"], "ready");
    assert_eq!(semantic_status["core_generation_id"], generation);
    assert_eq!(
        runtime
            .sidecar_drain
            .semantic_attempted_generation
            .as_deref(),
        Some(generation.as_str())
    );
    assert_eq!(runtime.relational_retry.consecutive_failures, 1);
    assert_eq!(runtime.pro_retry.consecutive_failures, 0);
    assert_eq!(runtime.semantic_retry.consecutive_failures, 0);
    assert_eq!(pinned_generation(temp.path()), generation);
}

#[test]
fn nonretryable_pro_attempt_is_generation_guarded_without_starving_relational() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = SourceBackedRefreshCoordinator::with_executor(std::sync::Arc::new(
        |execution: SourceBackedRefreshExecution<'_>| {
            Ok(publish_empty_authoritative_generation(execution.index_root))
        },
    ));
    let mut runtime = DaemonRuntime::default();

    let core = run_daemon_once_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    let generation = read_daemon_job_status(&daemon_source_backed_refresh_job_path(temp.path()))
        .unwrap()["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(core.continue_immediately);

    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let _hooks = install_jobs(
        calls.clone(),
        Some(relational_status(&generation, "completed")),
        None,
    );
    let relational = run_daemon_once_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(relational.continue_immediately);
    assert_eq!(&*calls.borrow(), &["relational_projection"]);
    assert!(read_pro_status(temp.path()).is_none());
    let relational_status = super::read_relational_status(temp.path()).unwrap();
    assert_eq!(relational_status["status"], "completed");
    assert_eq!(relational_status["core_generation_id"], generation);
    assert_eq!(relational_status["receipt_core_generation_id"], generation);

    let pro = run_daemon_once_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(pro.continue_immediately);
    let first_pro_status = read_pro_status(temp.path()).unwrap();
    assert_eq!(first_pro_status["status"], "error");
    assert_eq!(first_pro_status["retryable"], false);
    assert_eq!(first_pro_status["attempts"], 1);
    assert_eq!(first_pro_status["core_generation_id"], generation);
    assert_eq!(
        runtime.sidecar_drain.pro_attempted_generation.as_deref(),
        Some(generation.as_str())
    );

    let drained = run_daemon_once_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(!drained.did_work);
    assert!(!drained.continue_immediately);
    assert_eq!(
        read_pro_status(temp.path()).unwrap(),
        first_pro_status,
        "the same Core drain must not submit a second terminal Pro attempt"
    );
    assert_eq!(&*calls.borrow(), &["relational_projection"]);
    assert_eq!(pinned_generation(temp.path()), generation);
}

#[test]
fn source_refresh_only_mode_excludes_source_backed_pro_catch_up() {
    assert!(daemon_mode_runs_source_backed_pro_catch_up(
        DaemonMode::Full
    ));
    assert!(!daemon_mode_runs_source_backed_pro_catch_up(
        DaemonMode::SourceRefreshOnly
    ));
}

#[test]
fn source_refresh_only_mode_excludes_source_backed_relational_catch_up() {
    assert!(daemon_mode_runs_source_backed_relational_catch_up(
        DaemonMode::Full
    ));
    assert!(!daemon_mode_runs_source_backed_relational_catch_up(
        DaemonMode::SourceRefreshOnly
    ));
}

#[test]
fn source_refresh_only_mode_excludes_source_backed_semantic_projection() {
    assert!(daemon_mode_runs_source_backed_semantic_projection(
        DaemonMode::Full
    ));
    assert!(!daemon_mode_runs_source_backed_semantic_projection(
        DaemonMode::SourceRefreshOnly
    ));
}

#[test]
fn source_refresh_only_tick_creates_no_consumer_catch_up_status() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = AppConfig::default();
    config.daemon.mode = DaemonMode::SourceRefreshOnly;
    let mut runtime = DaemonRuntime {
        config,
        ..DaemonRuntime::default()
    };
    let args = DaemonRunArgs {
        foreground: false,
        once: true,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: None,
        trigger_command: None,
        format: JsonOutputFormat::Json,
    };

    let iteration =
        run_daemon_once_with_activity(&args, temp.path(), &mut runtime, None, true, None, None)
            .unwrap();

    assert!(!iteration.did_work);
    assert!(!iteration.failed);
    assert!(!temp.path().join("daemon/jobs/pro-catch-up.json").exists());
    assert!(!temp
        .path()
        .join("daemon/jobs/relational-catch-up.json")
        .exists());
    assert!(!super::daemon_semantic_job_path(temp.path()).exists());
}

#[test]
fn pro_projection_error_never_puts_core_refresh_into_backoff() {
    let core_job = json!({
        "status": "completed",
        "published_generation": "a".repeat(64),
        "pro_projection": {
            "status": "error",
            "pending": true,
            "retryable": true,
            "error_code": "pro_not_installed",
        },
    });
    let mut backoff = DaemonRetryBackoff::default();

    assert!(!daemon_job_should_backoff(&core_job));
    let recorded = record_daemon_job_retry(&mut backoff, core_job);

    assert_eq!(recorded["status"], "completed");
    assert_eq!(recorded["pro_projection"]["status"], "error");
    assert_eq!(backoff.consecutive_failures, 0);
}

fn failed_pro_status(generation: &str) -> Value {
    json!({
        "schema_version": 1,
        "owner": "daemon",
        "kind": "source_backed_pro_catch_up",
        "status": "error",
        "pending": true,
        "retryable": true,
        "core_generation_id": generation,
        "receipt_core_generation_id": null,
        "attempts": 1,
        "last_attempt_at_ms": 1,
        "error_code": "helper_crashed",
        "last_error": "fixture failure",
    })
}

#[test]
fn pro_failure_backoff_is_independent_and_skips_until_due() {
    let temp = tempfile::tempdir().unwrap();
    let generation = "a".repeat(64);
    let mut runtime = DaemonRuntime::default();
    runtime.history_retry.record_failure();
    runtime.semantic_retry.record_failure();
    let history_failures = runtime.history_retry.consecutive_failures;
    let semantic_failures = runtime.semantic_retry.consecutive_failures;

    let status = record_daemon_job_retry(&mut runtime.pro_retry, failed_pro_status(&generation));
    persist_pro_status(temp.path(), &status).unwrap();
    assert_eq!(runtime.pro_retry.consecutive_failures, 1);
    assert!(!runtime.pro_retry.ready());
    assert_eq!(runtime.history_retry.consecutive_failures, history_failures);
    assert_eq!(
        runtime.semantic_retry.consecutive_failures,
        semantic_failures
    );

    let skipped =
        run_pro_catch_up_with_retry(temp.path(), &mut runtime, &generation, None).unwrap();
    assert!(!skipped.did_work);
    assert_eq!(skipped.status["reason"], "retry_backoff");
    assert_eq!(skipped.status["consecutive_failures"], 1);
    assert_eq!(read_pro_status(temp.path()).unwrap()["status"], "error");
}

#[test]
fn pro_retry_restores_across_restart_and_core_backoff_does_not_gate_it() {
    let temp = tempfile::tempdir().unwrap();
    let generation = "b".repeat(64);
    let mut first = DaemonRuntime::default();
    let status = record_daemon_job_retry(&mut first.pro_retry, failed_pro_status(&generation));
    persist_pro_status(temp.path(), &status).unwrap();

    let mut restarted = DaemonRuntime::default();
    restarted.history_retry.record_failure();
    restore_daemon_consumer_retries(&mut restarted, temp.path());
    assert!(!restarted.history_retry.ready());
    assert!(!restarted.pro_retry.ready());
    assert_eq!(restarted.pro_retry.consecutive_failures, 1);

    restarted.pro_retry.retry_not_before = None;
    restarted.pro_retry.retry_not_before_at_ms = None;
    prepare_pro_retry_for_generation(&mut restarted, temp.path(), &generation);
    assert!(
        restarted.pro_retry.ready(),
        "Core history backoff must not block a due Pro retry"
    );
    assert!(!restarted.history_retry.ready());
}

#[test]
fn successful_pro_retry_resets_only_pro_state() {
    let mut runtime = DaemonRuntime::default();
    runtime.history_retry.record_failure();
    runtime.semantic_retry.record_failure();
    runtime.pro_retry.record_failure();
    let history_failures = runtime.history_retry.consecutive_failures;
    let semantic_failures = runtime.semantic_retry.consecutive_failures;

    let completed = record_daemon_job_retry(
        &mut runtime.pro_retry,
        json!({
            "status": "completed",
            "pending": false,
            "retryable": false,
        }),
    );
    assert_eq!(completed["status"], "completed");
    assert_eq!(runtime.pro_retry.consecutive_failures, 0);
    assert_eq!(runtime.history_retry.consecutive_failures, history_failures);
    assert_eq!(
        runtime.semantic_retry.consecutive_failures,
        semantic_failures
    );
}

#[test]
fn relational_projection_error_never_puts_core_refresh_into_backoff() {
    let core_job = json!({
        "status": "completed",
        "published_generation": "a".repeat(64),
        "relational_projection": {
            "status": "error",
            "pending": true,
            "retryable": true,
            "error_code": "source_relational_projection_unavailable",
        },
    });
    let mut backoff = DaemonRetryBackoff::default();

    assert!(!daemon_job_should_backoff(&core_job));
    let recorded = record_daemon_job_retry(&mut backoff, core_job);

    assert_eq!(recorded["status"], "completed");
    assert_eq!(recorded["relational_projection"]["status"], "error");
    assert_eq!(backoff.consecutive_failures, 0);
}

#[test]
fn relational_retry_runs_across_core_noop_backoff_and_recovers_independently() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(temp.path()),
        &json!({
            "status": "completed",
            "reason": "unchanged",
            "published_generation": generation,
        }),
    )
    .unwrap();
    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut first = DaemonRuntime::default();
    first.history_retry.record_failure();
    let history_failures = first.history_retry.consecutive_failures;
    {
        let _hooks = install_jobs(
            calls.clone(),
            Some(relational_status(&generation, "error")),
            None,
        );
        let iteration = run_daemon_once_with_activity(
            &daemon_args(),
            temp.path(),
            &mut first,
            None,
            false,
            None,
            None,
        )
        .unwrap();
        assert!(!iteration.failed, "derived failure cannot revoke Core");
    }
    assert_eq!(&*calls.borrow(), &["relational_projection"]);
    assert_eq!(first.relational_retry.consecutive_failures, 1);
    assert_eq!(first.history_retry.consecutive_failures, history_failures);
    assert_eq!(first.semantic_retry.consecutive_failures, 0);

    let mut restarted = DaemonRuntime::default();
    restarted.history_retry.record_failure();
    restore_daemon_consumer_retries(&mut restarted, temp.path());
    assert!(!restarted.relational_retry.ready());
    restarted.relational_retry.retry_not_before = None;
    restarted.relational_retry.retry_not_before_at_ms = None;
    calls.borrow_mut().clear();
    {
        let _hooks = install_jobs(
            calls.clone(),
            Some(relational_status(&generation, "completed")),
            None,
        );
        let iteration = run_daemon_once_with_activity(
            &daemon_args(),
            temp.path(),
            &mut restarted,
            None,
            false,
            None,
            None,
        )
        .unwrap();
        assert!(!iteration.failed);
    }
    assert_eq!(&*calls.borrow(), &["relational_projection"]);
    assert_eq!(restarted.relational_retry.consecutive_failures, 0);
    assert_eq!(restarted.history_retry.consecutive_failures, 1);
    assert_eq!(
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(temp.path())).unwrap()
            ["status"],
        "completed"
    );
}

#[test]
fn semantic_retry_runs_across_core_backoff_while_relational_waits_and_recovers_alone() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    let mut relational_retry = DaemonRetryBackoff::default();
    let relational = record_daemon_job_retry(
        &mut relational_retry,
        relational_status(&generation, "error"),
    );
    persist_relational_status(temp.path(), &relational).unwrap();
    write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(temp.path()),
        &json!({
            "status": "completed",
            "reason": "unchanged",
            "published_generation": generation,
        }),
    )
    .unwrap();

    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut first = DaemonRuntime::default();
    first.history_retry.record_failure();
    first.relational_retry.restore(Some(&relational));
    {
        let _hooks = install_jobs(
            calls.clone(),
            None,
            Some(json!({
                "status": "failed",
                "failure_class": "retryable",
                "retryable": true,
                "last_error": "injected semantic failure",
            })),
        );
        let iteration = run_daemon_once_with_activity(
            &daemon_args(),
            temp.path(),
            &mut first,
            None,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(!iteration.failed, "semantic failure cannot revoke Core");
    }
    assert_eq!(&*calls.borrow(), &["semantic_index"]);
    assert_eq!(first.semantic_retry.consecutive_failures, 1);
    assert_eq!(first.relational_retry.consecutive_failures, 1);
    assert_eq!(first.history_retry.consecutive_failures, 1);

    let mut restarted = DaemonRuntime::default();
    restarted.history_retry.record_failure();
    restore_daemon_consumer_retries(&mut restarted, temp.path());
    assert!(!restarted.relational_retry.ready());
    assert!(!restarted.semantic_retry.ready());
    restarted.semantic_retry.retry_not_before = None;
    restarted.semantic_retry.retry_not_before_at_ms = None;
    calls.borrow_mut().clear();
    {
        let _hooks = install_jobs(
            calls.clone(),
            None,
            Some(json!({
                "status": "ready",
                "source_generation_ready": true,
                "source_work_remaining": false,
            })),
        );
        let iteration = run_daemon_once_with_activity(
            &daemon_args(),
            temp.path(),
            &mut restarted,
            None,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(!iteration.failed);
    }
    assert_eq!(&*calls.borrow(), &["semantic_index"]);
    assert_eq!(restarted.semantic_retry.consecutive_failures, 0);
    assert_eq!(restarted.relational_retry.consecutive_failures, 1);
    assert_eq!(restarted.history_retry.consecutive_failures, 1);
    let semantic = read_daemon_job_status(&daemon_semantic_job_path(temp.path())).unwrap();
    assert_eq!(semantic["status"], "ready");
    assert_eq!(semantic["core_generation_id"], generation);
}

#[test]
fn semantic_projection_error_never_puts_core_refresh_into_backoff() {
    let core_job = json!({
        "status": "completed",
        "published_generation": "a".repeat(64),
        "semantic_projection": {
            "status": "failed",
            "retryable": true,
            "failure_class": "transient",
            "last_error": "fixture semantic failure",
        },
    });
    let mut backoff = DaemonRetryBackoff::default();

    assert!(!daemon_job_should_backoff(&core_job));
    let recorded = record_daemon_job_retry(&mut backoff, core_job);

    assert_eq!(recorded["status"], "completed");
    assert_eq!(recorded["semantic_projection"]["status"], "failed");
    assert_eq!(backoff.consecutive_failures, 0);
}
