#[path = "daemon_scheduler_tests/background_refresh.rs"]
mod background_refresh;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier, Mutex,
    },
};

use ctx_history_capture::{
    SourceBackedRefreshScope, SourceBackedRouteError, SourceBackedRouteErrorKind,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceFrontier, SourceObservation, TypedKey,
};
use ctx_history_index::{
    GenerationWriter, SourceRouteIdentity, WriterOptions, MAX_SEMANTIC_EVENT_PAGE_ITEMS,
};
use ctx_pro_host_protocol::ProFilesystemLayout;
use serde_json::{json, Value};

use crate::{
    config::{AppConfig, DaemonMode},
    output::JsonOutputFormat,
    semantic::{
        daemon::{daemon_wait_duration, install_daemon_test_job_hooks, DaemonTestJobHooks},
        source_backed_refresh_coordinator::EventWatermark,
        source_backed_refresh_coordinator::{
            coordinate_source_backed_refresh, source_backed_index_root, CoreRefreshEngine,
            SourceBackedRefreshCurrent, SourceBackedRefreshExecution, SourceBackedRefreshMode,
            SourceBackedRefreshPublication, SourceBackedRefreshRouteResult,
            SourceBackedRefreshSourceFailure, SourceBackedRefreshTimings,
        },
        source_epoch_status_report,
    },
    DaemonRunArgs,
};

use super::{
    background_refresh_rest, daemon_consumer_retry_due, daemon_core_refresh_job_path,
    daemon_job_should_backoff, daemon_mode_runs_core_pro_catch_up,
    daemon_mode_runs_core_semantic_projection, daemon_semantic_job_path,
    newer_active_pro_generation_is_pending, persist_pro_status, pin_scheduled_pro_target,
    prepare_pro_retry_for_generation, preserve_daemon_background_refresh_recovery_provenance,
    read_daemon_job_status, read_pro_status, record_daemon_job_retry, record_source_refresh_retry,
    restore_daemon_background_refresh_cadence, restore_daemon_consumer_retries,
    run_daemon_scheduler_cycle_with_activity, run_pending_core_pro_catch_up,
    run_pending_core_pro_catch_up_with, run_pending_core_refresh, run_pro_catch_up_with_retry,
    write_daemon_job_status, DaemonBackgroundRefreshCadence, DaemonRetryBackoff, DaemonRuntime,
    SourceBackedProCatchUpRun, SourceBackedProCoreAuthority, DAEMON_BACKGROUND_REFRESH_MAX_REST,
    DAEMON_BACKGROUND_REFRESH_MIN_REST,
};

const READINESS_QUERY: &str = "readiness-boundary-regression";

fn daemon_args() -> DaemonRunArgs {
    DaemonRunArgs {
        foreground: false,
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
    .into_writer()
    .unwrap()
    .commit(|_| true)
    .unwrap()
    .generation_id
}

#[path = "daemon_scheduler_tests/refresh_retry.rs"]
mod refresh_retry;

fn publish_empty_authoritative_generation(index_root: &Path) -> SourceBackedRefreshPublication {
    let receipt = GenerationWriter::open(index_root, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap()
        .commit(|_| true)
        .unwrap();
    SourceBackedRefreshPublication {
        route_results: Vec::new(),
        zero_source_authority: Vec::new(),
        catalog_route_bindings: Vec::new(),
        verified_index: None,
        generation_id: receipt.generation_id.clone(),
        published_explicit_source_catalog: None,
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

fn readiness_record(source: &ctx_history_core::SourceKey) -> CoreRecord {
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
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        0,
        "message",
        "primary",
        true,
        "daemon-scheduler-test-v1",
        format!("exact lexical hit for {READINESS_QUERY}"),
    )
    .unwrap();
    record.provider_session_id = Some("readiness-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(0));
    record.branch = Some("main".to_owned());
    record.occurred_at_unix_ms = Some(1_700_000_000_000);
    record.role = Some("assistant".to_owned());
    record.workspace = Some("ctx".to_owned());
    record.cwd = Some("/work/ctx".to_owned());
    record.validate_contract().unwrap();
    record
}

fn semantic_catch_up_record(source: &ctx_history_core::SourceKey, sequence: u64) -> CoreRecord {
    let native_session = TypedKey::utf8("semantic-catch-up-session").unwrap();
    let session_key = NativeSessionKey::native_id("session", native_session).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let native_item = NativeItemKey::native_id(
        "message",
        TypedKey::utf8(format!("semantic-catch-up-event-{sequence}")).unwrap(),
    )
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        sequence,
        "message",
        "primary",
        true,
        "daemon-scheduler-semantic-catch-up-test-v1",
        format!("eligible semantic catch-up event {sequence}"),
    )
    .unwrap();
    record.provider_session_id = Some("semantic-catch-up-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(sequence));
    record.occurred_at_unix_ms = Some(1_700_000_000_000 + sequence as i64);
    record.role = Some("user".to_owned());
    record.validate_contract().unwrap();
    record
}

fn publish_semantic_catch_up_generation(data_root: &Path, event_count: u64) -> String {
    let source = readiness_source();
    let mut writer = GenerationWriter::open(
        source_backed_index_root(data_root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 0..event_count {
        writer
            .add_core_record(semantic_catch_up_record(&source, sequence))
            .unwrap();
    }
    let certified_bytes = event_count * 128;
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![2]).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify_with_frontier(
                observation.clone(),
                observation,
                "codex-parser-v1",
                [8; 32],
                ScannedSourceCounts {
                    complete_records: event_count,
                    retained_records: event_count,
                    indexed_documents: event_count,
                    certified_bytes,
                    ..ScannedSourceCounts::default()
                },
                Some(
                    SourceFrontier::new(
                        "jsonl-byte-offset",
                        TypedKey::U64(certified_bytes),
                        certified_bytes,
                        [8; 32],
                    )
                    .unwrap(),
                ),
            )
            .unwrap(),
        )
        .unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    assert_eq!(receipt.indexed_documents, event_count);
    receipt.generation_id
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
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(readiness_record(&source)).unwrap();
    writer
        .certify_source(readiness_certificate(&source))
        .unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    SourceBackedRefreshPublication {
        route_results: vec![SourceBackedRefreshRouteResult::succeeded(
            "ab".repeat(32),
            true,
        )],
        zero_source_authority: Vec::new(),
        catalog_route_bindings: Vec::new(),
        verified_index: None,
        generation_id: receipt.generation_id,
        published_explicit_source_catalog: None,
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

fn pinned_generation(data_root: &Path) -> String {
    coordinate_source_backed_refresh(data_root, SourceBackedRefreshMode::Off)
        .unwrap()
        .pin
        .generation_id()
        .to_owned()
}

#[test]
fn core_publication_is_ready_and_searchable_before_consumer_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().to_path_buf();
    let coordinator = CoreRefreshEngine::with_executor(std::sync::Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let publication = publish_readiness_generation(execution.index_root);
            execution.report_progress("committed", 1, 1, None, None, None)?;
            Ok(publication)
        },
    ));
    coordinator.enqueue_for_test(None);
    let mut runtime = DaemonRuntime::default();
    let core = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        &data_root,
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();

    let core_job = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root))
        .expect("Core terminal receipt");
    let refresh_off =
        coordinate_source_backed_refresh(&data_root, SourceBackedRefreshMode::Off).unwrap();
    let pinned_generation = refresh_off.pin.generation_id().to_owned();
    let hits = refresh_off
        .pin
        .into_index()
        .search_event_candidates(READINESS_QUERY, 10)
        .unwrap();
    let status = source_epoch_status_report(&data_root, &AppConfig::default()).unwrap();

    let published_generation = core_job["published_generation"]
        .as_str()
        .expect("published generation");
    assert_eq!(core_job["status"], "completed", "{core_job:#}");
    assert_eq!(core_job["request_state"], "published", "{core_job:#}");
    assert_eq!(core_job["progress"]["phase"], "published", "{core_job:#}");
    assert_eq!(core_job["progress"]["completed_sources"], 1);
    assert_eq!(core_job["progress"]["total_sources"], 1);
    assert!(core_job.get("pro_projection").is_none());
    assert!(core_job.get("semantic_projection").is_none());
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

    assert!(core.did_work);
    assert!(!core.failed);
    assert!(core.continue_immediately);
    assert_eq!(
        runtime.sidecar_drain.generation.as_deref(),
        Some(published_generation)
    );
}

#[test]
fn healthy_idle_scheduler_performs_zero_source_refresh_scans() {
    let temp = tempfile::tempdir().unwrap();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let executor_calls = std::sync::Arc::clone(&calls);
    let coordinator = CoreRefreshEngine::with_executor(std::sync::Arc::new(
        move |_: SourceBackedRefreshExecution<'_>| {
            executor_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(anyhow::anyhow!("idle executor must not run"))
        },
    ));
    let mut runtime = DaemonRuntime::default();

    let idle = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();

    assert!(!idle.did_work);
    assert!(!idle.failed);
    assert!(!idle.continue_immediately);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!daemon_core_refresh_job_path(temp.path()).exists());
}

#[test]
fn startup_seeded_manual_all_continuation_scans_each_route_once() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    GenerationWriter::open(data_root.join("search/lexical"), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap()
        .commit(|_| true)
        .unwrap();
    let route =
        |byte: u8| SourceRouteIdentity::from_sha256(format!("{byte:02x}").repeat(32)).unwrap();
    let routes = BTreeSet::from([route(0x81), route(0x82), route(0x83)]);
    let startup_routes = routes.iter().take(2).cloned().collect::<BTreeSet<_>>();
    let scans = Arc::new(Mutex::new(BTreeMap::<SourceRouteIdentity, usize>::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_routes = routes.clone();
    let executor_scans = Arc::clone(&scans);
    let executor_calls = Arc::clone(&calls);
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let selected = match &execution.scope {
                SourceBackedRefreshScope::All => executor_routes
                    .difference(&execution.covered_route_ids)
                    .cloned()
                    .collect::<BTreeSet<_>>(),
                SourceBackedRefreshScope::Exact(routes) => routes.clone(),
            };
            for route in &selected {
                *executor_scans
                    .lock()
                    .unwrap()
                    .entry(route.clone())
                    .or_default() += 1;
            }
            if executor_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                executor_entered.wait();
                executor_release.wait();
            }
            let receipt = GenerationWriter::open(execution.index_root, WriterOptions::default())?
                .into_writer()
                .map_err(crate::semantic::committed_generation_recovery_error)?
                .commit(|_| true)?;
            let route_results = selected
                .iter()
                .map(|route| {
                    SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true)
                })
                .collect::<Vec<_>>();
            let mut publication = SourceBackedRefreshPublication {
                route_results,
                zero_source_authority: Vec::new(),
                catalog_route_bindings: Vec::new(),
                verified_index: None,
                generation_id: receipt.generation_id,
                published_explicit_source_catalog: execution.explicit_source_catalog.cloned(),
                unsupported_routes: 0,
                certified_source_count: 0,
                certified_source_bytes: 0,
                current: SourceBackedRefreshCurrent::default(),
                timings: SourceBackedRefreshTimings {
                    discovery_us: 1,
                    scan_stage_us: 1,
                    commit_us: 1,
                },
            };
            execution.covered_publication.apply(&mut publication);
            Ok(publication)
        },
    )));
    coordinator.initialize_watch_route_authority(routes.clone());
    coordinator.schedule_startup_route_reconciliation(
        startup_routes,
        EventWatermark::new(1, 0),
        super::source_route_ledger_now_ms().saturating_sub(1_000),
    );
    let authority =
        crate::commands::import::load_explicit_source_catalog_authority(&data_root).unwrap();

    let (manual_request_id, mut runtime) = std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_root = data_root.clone();
        let handle = scope.spawn(move || {
            let mut config = AppConfig::default();
            config.daemon.mode = DaemonMode::SourceRefreshOnly;
            let mut runtime = DaemonRuntime {
                config,
                ..DaemonRuntime::default()
            };
            let iteration = run_daemon_scheduler_cycle_with_activity(
                &daemon_args(),
                &runner_root,
                &mut runtime,
                None,
                false,
                None,
                Some(&runner),
            )
            .unwrap();
            (iteration, runtime)
        });
        entered.wait();
        let response = coordinator
            .handle_ipc_request_with_admission_fence_for_test(
                &data_root,
                &json!({
                    "schema_version": 1,
                    "op": "source_refresh_request",
                    "mode": "wait",
                    "operation": "import",
                    "explicit_source_catalog": authority.to_json(),
                    "fresh_after_admitted_snapshot": true,
                }),
                BTreeMap::new(),
            )
            .unwrap()
            .expect("manual all continuation response");
        let request_id = response["request_id"].as_str().unwrap().to_owned();
        release.wait();
        let (first, runtime) = handle.join().unwrap();
        assert!(!first.failed);
        assert!(
            !first.continue_immediately,
            "the daemon loop must drain watcher events before the queued all-route successor"
        );
        (request_id, runtime)
    });

    let wait_started = std::time::Instant::now();
    assert_eq!(
        daemon_wait_duration(
            &runtime,
            Some(&coordinator),
            wait_started + std::time::Duration::from_secs(30),
            None,
            None,
            wait_started,
        ),
        std::time::Duration::ZERO,
        "a successor exposed after the watcher-drain yield must remain immediately runnable"
    );
    let successor = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        &data_root,
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();

    assert!(!successor.failed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        *scans.lock().unwrap(),
        routes
            .iter()
            .cloned()
            .map(|route| (route, 1))
            .collect::<BTreeMap<_, _>>()
    );
    let terminal = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(terminal["request_id"], manual_request_id);
    assert_eq!(terminal["request_state"], "published");
    assert_eq!(terminal["scanned_routes"], routes.len());
    assert_eq!(terminal["receipt"]["selected_route_total"], routes.len());
    assert!(!coordinator.has_pending_request());
    assert!(!coordinator.has_scheduled_route_work());
}

fn install_jobs(
    calls: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
    semantic_index: Option<Value>,
) -> super::super::daemon::DaemonTestJobHookGuard {
    install_daemon_test_job_hooks(DaemonTestJobHooks {
        calls,
        semantic_index,
    })
}

#[test]
fn one_core_cycle_then_scheduler_drains_optional_consumers() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::with_executor(std::sync::Arc::new(
        |execution: SourceBackedRefreshExecution<'_>| {
            Ok(publish_empty_authoritative_generation(execution.index_root))
        },
    ));
    coordinator.enqueue_for_test(None);
    let mut runtime = DaemonRuntime::default();
    let core = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        Some(&coordinator),
    )
    .unwrap();
    let core_job = read_daemon_job_status(&daemon_core_refresh_job_path(temp.path())).unwrap();
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
    assert!(super::semantic_generation_needs_catch_up(
        temp.path(),
        &generation
    ));
    assert!(runtime.sidecar_drain.pro_attempted_generation.is_none());
    assert!(runtime
        .sidecar_drain
        .semantic_attempted_generation
        .is_none());
    assert!(read_pro_status(temp.path()).is_none());
    assert!(!daemon_semantic_job_path(temp.path()).exists());
    assert_eq!(pinned_generation(temp.path()), generation);

    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let _hooks = install_jobs(
        calls.clone(),
        Some(json!({
            "status": "ready",
            "source_generation_ready": true,
            "source_work_remaining": false,
        })),
    );

    let pro = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(!pro.continue_immediately);
    assert!(calls.borrow().is_empty());
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

    let semantic = run_daemon_scheduler_cycle_with_activity(
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
    assert_eq!(&*calls.borrow(), &["semantic_index"]);
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
    assert_eq!(runtime.pro_retry.consecutive_failures, 0);
    assert_eq!(runtime.semantic_retry.consecutive_failures, 0);
    assert_eq!(pinned_generation(temp.path()), generation);
}

#[test]
fn idle_semantic_catch_up_continues_past_one_page_and_drains_to_terminal() {
    const ELIGIBLE_EVENTS: u64 = MAX_SEMANTIC_EVENT_PAGE_ITEMS as u64 + 1;

    let temp = tempfile::tempdir().unwrap();
    let generation = publish_semantic_catch_up_generation(temp.path(), ELIGIBLE_EVENTS);
    let index = coordinate_source_backed_refresh(temp.path(), SourceBackedRefreshMode::Off)
        .unwrap()
        .pin
        .into_index();
    let first_page = index
        .core_semantic_event_page(None, MAX_SEMANTIC_EVENT_PAGE_ITEMS)
        .unwrap();
    assert_eq!(first_page.eligible_total, ELIGIBLE_EVENTS);
    assert_eq!(first_page.items.len(), MAX_SEMANTIC_EVENT_PAGE_ITEMS);
    assert!(!first_page.terminal);
    let terminal_page = index
        .core_semantic_event_page(
            first_page.next_cursor.as_ref(),
            MAX_SEMANTIC_EVENT_PAGE_ITEMS,
        )
        .unwrap();
    assert_eq!(terminal_page.items.len(), 1);
    assert!(terminal_page.terminal);

    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut runtime = DaemonRuntime::default();
    runtime.sidecar_drain.generation = Some(generation.clone());
    runtime.sidecar_drain.pro_attempted_generation = Some(generation.clone());

    {
        let _jobs = install_jobs(
            calls.clone(),
            Some(json!({
                "status": "budget_exhausted",
                "source_records_decoded": MAX_SEMANTIC_EVENT_PAGE_ITEMS,
                "source_generation_ready": false,
                "source_work_remaining": true,
            })),
        );
        let first = run_daemon_scheduler_cycle_with_activity(
            &daemon_args(),
            temp.path(),
            &mut runtime,
            None,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(first.did_work);
        assert!(first.continue_immediately);
    }
    let first_status = read_daemon_job_status(&daemon_semantic_job_path(temp.path())).unwrap();
    assert_eq!(first_status["core_generation_id"], generation);
    assert_eq!(
        first_status["source_records_decoded"],
        MAX_SEMANTIC_EVENT_PAGE_ITEMS
    );
    assert_eq!(first_status["source_work_remaining"], true);

    {
        let _jobs = install_jobs(
            calls.clone(),
            Some(json!({
                "status": "ready",
                "source_records_decoded": 1,
                "source_generation_ready": true,
                "source_work_remaining": false,
            })),
        );
        let terminal = run_daemon_scheduler_cycle_with_activity(
            &daemon_args(),
            temp.path(),
            &mut runtime,
            None,
            true,
            None,
            None,
        )
        .unwrap();
        assert!(terminal.did_work);
        assert!(terminal.continue_immediately);
    }
    let terminal_status = read_daemon_job_status(&daemon_semantic_job_path(temp.path())).unwrap();
    assert_eq!(terminal_status["status"], "ready");
    assert_eq!(terminal_status["core_generation_id"], generation);
    assert_eq!(terminal_status["source_records_decoded"], 1);
    assert_eq!(terminal_status["source_work_remaining"], false);
    assert_eq!(&*calls.borrow(), &["semantic_index", "semantic_index"]);

    let drained = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        None,
    )
    .unwrap();
    assert!(!drained.did_work);
    assert!(!drained.continue_immediately);
    assert!(runtime.sidecar_drain.generation.is_none());
    assert_eq!(&*calls.borrow(), &["semantic_index", "semantic_index"]);
}

#[test]
fn nonretryable_pro_attempt_is_generation_guarded() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::with_executor(std::sync::Arc::new(
        |execution: SourceBackedRefreshExecution<'_>| {
            Ok(publish_empty_authoritative_generation(execution.index_root))
        },
    ));
    coordinator.enqueue_for_test(None);
    let mut runtime = DaemonRuntime::default();

    let core = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    let generation = read_daemon_job_status(&daemon_core_refresh_job_path(temp.path())).unwrap()
        ["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(core.continue_immediately);

    let pro = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(!pro.continue_immediately);
    let first_pro_status = read_pro_status(temp.path()).unwrap();
    assert_eq!(first_pro_status["status"], "error");
    assert_eq!(first_pro_status["retryable"], false);
    assert_eq!(first_pro_status["attempts"], 1);
    assert_eq!(first_pro_status["core_generation_id"], generation);
    assert_eq!(
        runtime.sidecar_drain.pro_attempted_generation.as_deref(),
        Some(generation.as_str())
    );

    let drained = run_daemon_scheduler_cycle_with_activity(
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
    assert_eq!(pinned_generation(temp.path()), generation);
}

#[test]
fn local_completed_pro_status_cannot_suppress_scheduler_validation() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::with_executor(std::sync::Arc::new(
        |execution: SourceBackedRefreshExecution<'_>| {
            Ok(publish_empty_authoritative_generation(execution.index_root))
        },
    ));
    coordinator.enqueue_for_test(None);
    let mut runtime = DaemonRuntime::default();
    let core = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(core.continue_immediately);
    let generation = coordinator
        .pinned_core_publication()
        .expect("pinned Core publication")
        .generation_id()
        .to_owned();
    runtime.sidecar_drain.generation = None;
    runtime.sidecar_drain.pro_attempted_generation = None;
    persist_pro_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "owner": "daemon",
            "kind": "source_backed_pro_catch_up",
            "status": "completed",
            "pending": false,
            "retryable": false,
            "core_generation_id": generation,
            "receipt_core_generation_id": generation,
            "attempts": 1,
            "last_attempt_at_ms": 1,
            "last_attempt_duration_us": 1,
            "error_code": null,
            "last_error": null,
        }),
    )
    .unwrap();

    let scheduled =
        run_pending_core_pro_catch_up(temp.path(), &mut runtime, Some(&coordinator)).unwrap();

    let scheduled = scheduled.expect("local completion must still validate the helper");
    assert!(!scheduled.did_work);
    assert!(
        !scheduled.continue_immediately,
        "validated replay must wait for the normal daemon interval"
    );
    assert_eq!(
        runtime.sidecar_drain.pro_attempted_generation.as_deref(),
        Some(generation.as_str())
    );
}

#[test]
fn pro_catch_up_requests_immediate_drain_after_work_or_a_successful_yield() {
    let replay = super::core_pro_catch_up_iteration(false, false);
    assert!(!replay.did_work);
    assert!(!replay.continue_immediately);

    let materialized = super::core_pro_catch_up_iteration(true, false);
    assert!(materialized.did_work);
    assert!(materialized.continue_immediately);

    let pending = super::core_pro_catch_up_iteration(false, true);
    assert!(!pending.did_work);
    assert!(pending.continue_immediately);

    let status = json!({
        "status": "pending",
        "pending": true,
        "retryable": false,
        "reason": "finalizing",
    });
    assert!(!daemon_job_should_backoff(&status));
    let mut backoff = DaemonRetryBackoff::default();
    let recorded = record_daemon_job_retry(&mut backoff, status);
    assert_eq!(recorded["reason"], "finalizing");
    assert_eq!(backoff.consecutive_failures, 0);
}

#[test]
fn durable_finalization_pending_bypasses_same_generation_attempt_guard() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_semantic_catch_up_generation(temp.path(), 1);
    persist_pro_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "owner": "daemon",
            "kind": "source_backed_pro_catch_up",
            "status": "pending",
            "pending": true,
            "retryable": false,
            "core_generation_id": generation,
            "receipt_core_generation_id": null,
            "finalization_progress": {
                "materialization_id": "a".repeat(64),
                "core_generation_id": generation,
                "finish_request_digest": "c".repeat(64),
                "materializer_revision": "materializer-v1",
                "phase": "emit_replay",
                "cursor_sha256": "b".repeat(64),
            },
            "attempts": 1,
            "last_attempt_at_ms": 1,
            "last_attempt_duration_us": 1,
            "error_code": null,
            "last_error": null,
            "reason": "finalizing",
            "consecutive_failures": 0,
            "retry_after_ms": null,
            "retry_not_before_at_ms": null,
        }),
    )
    .unwrap();
    let mut runtime = DaemonRuntime::default();
    runtime.sidecar_drain.pro_attempted_generation = Some(generation.clone());

    let scheduled = run_pending_core_pro_catch_up(temp.path(), &mut runtime, None).unwrap();

    assert!(
        scheduled.is_some(),
        "durable finalization must retain and reuse the stable Core pin"
    );
    assert_eq!(
        runtime.sidecar_drain.pro_attempted_generation.as_deref(),
        Some(generation.as_str())
    );
    assert_eq!(pinned_generation(temp.path()), generation);
}

#[test]
fn fresh_restart_pins_durable_finalizing_target_before_newer_active_core() {
    let temp = tempfile::tempdir().unwrap();
    let finalizing_generation = publish_semantic_catch_up_generation(temp.path(), 1);
    persist_pro_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "owner": "daemon",
            "kind": "source_backed_pro_catch_up",
            "status": "pending",
            "pending": true,
            "retryable": false,
            "core_generation_id": finalizing_generation,
            "receipt_core_generation_id": null,
            "finalization_progress": {
                "materialization_id": "a".repeat(64),
                "core_generation_id": finalizing_generation,
                "finish_request_digest": "c".repeat(64),
                "materializer_revision": "materializer-v1",
                "phase": "emit_replay",
                "cursor_sha256": "b".repeat(64),
            },
            "attempts": 1,
            "last_attempt_at_ms": 1,
            "last_attempt_duration_us": 1,
            "error_code": null,
            "last_error": null,
            "reason": "finalizing",
            "consecutive_failures": 0,
            "retry_after_ms": null,
            "retry_not_before_at_ms": null,
        }),
    )
    .unwrap();
    let active_generation = publish_semantic_catch_up_generation(temp.path(), 2);
    assert_ne!(active_generation, finalizing_generation);

    let (scheduled_generation, scheduled_pin) = pin_scheduled_pro_target(temp.path())
        .unwrap()
        .expect("durable finalization target");
    assert_eq!(scheduled_generation, finalizing_generation);
    assert_eq!(scheduled_pin.generation_id(), finalizing_generation);
    assert!(newer_active_pro_generation_is_pending(temp.path(), &finalizing_generation).unwrap());
    let queued = super::core_pro_catch_up_iteration(false, true);
    assert!(queued.continue_immediately);
}

#[test]
fn completing_old_finalizing_target_selects_newer_active_core_on_next_scheduler_turn() {
    let temp = tempfile::tempdir().unwrap();
    let finalizing_generation = publish_semantic_catch_up_generation(temp.path(), 1);
    persist_pro_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "owner": "daemon",
            "kind": "source_backed_pro_catch_up",
            "status": "pending",
            "pending": true,
            "retryable": false,
            "core_generation_id": finalizing_generation,
            "receipt_core_generation_id": null,
            "finalization_progress": {
                "materialization_id": "a".repeat(64),
                "core_generation_id": finalizing_generation,
                "finish_request_digest": "c".repeat(64),
                "materializer_revision": "materializer-v1",
                "phase": "emit_replay",
                "cursor_sha256": "b".repeat(64),
            },
            "attempts": 1,
            "last_attempt_at_ms": 1,
            "last_attempt_duration_us": 1,
            "error_code": null,
            "last_error": null,
            "reason": "finalizing",
            "consecutive_failures": 0,
            "retry_after_ms": null,
            "retry_not_before_at_ms": null,
        }),
    )
    .unwrap();
    let active_generation = publish_semantic_catch_up_generation(temp.path(), 2);
    assert_ne!(active_generation, finalizing_generation);

    fn completed_status(generation: &str, attempts: u64) -> Value {
        json!({
            "schema_version": 1,
            "owner": "daemon",
            "kind": "source_backed_pro_catch_up",
            "status": "completed",
            "pending": false,
            "retryable": false,
            "core_generation_id": generation,
            "receipt_core_generation_id": generation,
            "attempts": attempts,
            "last_attempt_at_ms": 2,
            "last_attempt_duration_us": 1,
            "error_code": null,
            "last_error": null,
        })
    }

    let mut runtime = DaemonRuntime::default();
    let mut selected = Vec::new();
    let old = run_pending_core_pro_catch_up_with(
        temp.path(),
        &mut runtime,
        None,
        |data_root, _runtime, generation, authority| {
            assert_eq!(generation, finalizing_generation);
            assert_eq!(authority.generation_id(), finalizing_generation);
            selected.push(generation.to_owned());
            let status = completed_status(generation, 2);
            persist_pro_status(data_root, &status)?;
            Ok(SourceBackedProCatchUpRun {
                status,
                did_work: true,
                continuation_pending: false,
            })
        },
    )
    .unwrap()
    .expect("old finalizing target scheduler turn");
    assert!(old.continue_immediately);
    assert!(runtime.sidecar_drain.pro_attempted_generation.is_none());

    let newer = run_pending_core_pro_catch_up_with(
        temp.path(),
        &mut runtime,
        None,
        |data_root, _runtime, generation, authority| {
            assert_eq!(generation, active_generation);
            assert_eq!(authority.generation_id(), active_generation);
            selected.push(generation.to_owned());
            let status = completed_status(generation, 1);
            persist_pro_status(data_root, &status)?;
            Ok(SourceBackedProCatchUpRun {
                status,
                did_work: true,
                continuation_pending: false,
            })
        },
    )
    .unwrap()
    .expect("newer active Core scheduler turn");

    assert!(newer.did_work);
    assert_eq!(selected, [finalizing_generation, active_generation.clone()]);
    assert_eq!(
        runtime.sidecar_drain.pro_attempted_generation.as_deref(),
        Some(active_generation.as_str())
    );
    assert_eq!(
        read_pro_status(temp.path()).unwrap()["core_generation_id"],
        active_generation
    );
}

#[test]
fn foreground_query_defers_the_next_finalization_quantum() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    let finalizing = json!({
        "schema_version": 1,
        "owner": "daemon",
        "kind": "source_backed_pro_catch_up",
        "status": "pending",
        "pending": true,
        "retryable": false,
        "core_generation_id": generation,
        "receipt_core_generation_id": null,
        "finalization_progress": {
            "materialization_id": "a".repeat(64),
            "core_generation_id": generation,
            "finish_request_digest": "c".repeat(64),
            "materializer_revision": "materializer-v1",
            "phase": "emit_replay",
            "cursor_sha256": "b".repeat(64),
        },
        "attempts": 1,
        "last_attempt_at_ms": 1,
        "last_attempt_duration_us": 1,
        "error_code": null,
        "last_error": null,
        "reason": "finalizing",
        "consecutive_failures": 0,
        "retry_after_ms": null,
        "retry_not_before_at_ms": null,
    });
    persist_pro_status(temp.path(), &finalizing).unwrap();
    let activity = Arc::new(crate::semantic::query_service::DaemonQueryActivity::new());
    let _request = activity.begin_request().expect("foreground query");
    let mut runtime = DaemonRuntime::default();

    let deferred = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        Some(activity.as_ref()),
        None,
    )
    .unwrap();

    assert!(!deferred.did_work);
    assert!(!deferred.continue_immediately);
    assert_eq!(read_pro_status(temp.path()).unwrap(), finalizing);
}

#[test]
fn source_refresh_only_mode_excludes_source_backed_pro_catch_up() {
    assert!(daemon_mode_runs_core_pro_catch_up(DaemonMode::Full));
    assert!(!daemon_mode_runs_core_pro_catch_up(
        DaemonMode::SourceRefreshOnly
    ));
}

#[test]
fn source_refresh_only_mode_excludes_source_backed_semantic_projection() {
    assert!(daemon_mode_runs_core_semantic_projection(DaemonMode::Full));
    assert!(!daemon_mode_runs_core_semantic_projection(
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
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: None,
        trigger_command: None,
        format: JsonOutputFormat::Json,
    };

    let iteration = run_daemon_scheduler_cycle_with_activity(
        &args,
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        None,
    )
    .unwrap();

    assert!(!iteration.did_work);
    assert!(!iteration.failed);
    assert!(!temp.path().join("daemon/jobs/pro-catch-up.json").exists());
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
    let generation = publish_empty_core_generation(temp.path());
    let durable = super::pin_published_generation(temp.path())
        .unwrap()
        .expect("durable Core generation");
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

    let skipped = run_pro_catch_up_with_retry(
        temp.path(),
        &mut runtime,
        &generation,
        SourceBackedProCoreAuthority::Durable(&durable),
    )
    .unwrap();
    assert!(!skipped.did_work);
    assert_eq!(skipped.status["reason"], "retry_backoff");
    assert_eq!(skipped.status["consecutive_failures"], 1);
    assert_eq!(read_pro_status(temp.path()).unwrap()["status"], "error");
}

#[test]
fn persisted_due_pro_retry_reopens_durable_core_after_process_restart() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    let mut first = DaemonRuntime::default();
    let mut status = record_daemon_job_retry(&mut first.pro_retry, failed_pro_status(&generation));
    status["retry_not_before_at_ms"] = json!(ctx_history_core::utc_now().timestamp_millis() - 1);
    persist_pro_status(temp.path(), &status).unwrap();

    let coordinator = CoreRefreshEngine::new();
    assert!(coordinator.pinned_core_publication().is_none());
    let mut restarted = DaemonRuntime::default();
    restore_daemon_consumer_retries(&mut restarted, temp.path());
    assert!(daemon_consumer_retry_due(&restarted));
    assert!(restarted.pro_retry.ready());

    let iteration = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut restarted,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(!iteration.failed);
    let retried = read_pro_status(temp.path()).expect("retried Pro status");
    assert_eq!(retried["core_generation_id"], generation);
    assert_eq!(retried["attempts"], 2);
    assert_eq!(
        restarted.sidecar_drain.pro_attempted_generation.as_deref(),
        Some(generation.as_str())
    );
}

#[test]
fn process_restart_checks_durable_core_once_without_a_preexisting_retry() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    let coordinator = CoreRefreshEngine::new();
    assert!(coordinator.pinned_core_publication().is_none());
    let mut restarted = DaemonRuntime::default();

    let first = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut restarted,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    let first_status = read_pro_status(temp.path()).expect("initial durable Pro check");

    let second = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut restarted,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();

    assert!(!first.failed);
    assert_eq!(first_status["core_generation_id"], generation);
    assert_eq!(first_status["attempts"], 1);
    assert!(!second.failed);
    assert_eq!(
        read_pro_status(temp.path()).unwrap(),
        first_status,
        "steady ticks must not resubmit Pro catch-up"
    );
}

#[test]
fn prior_not_installed_result_does_not_suppress_first_install_check() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    persist_pro_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "owner": "daemon",
            "kind": "source_backed_pro_catch_up",
            "status": "error",
            "pending": true,
            "retryable": false,
            "core_generation_id": generation,
            "receipt_core_generation_id": null,
            "attempts": 1,
            "last_attempt_at_ms": 1,
            "error_code": "pro_not_installed",
            "last_error": "fixture helper was not installed",
        }),
    )
    .unwrap();
    let coordinator = CoreRefreshEngine::new();
    let mut after_install = DaemonRuntime::default();
    restore_daemon_consumer_retries(&mut after_install, temp.path());
    assert_eq!(after_install.pro_retry.consecutive_failures, 0);
    after_install.sidecar_drain.pro_attempted_generation = Some(generation.clone());
    let helper = ProFilesystemLayout::new(temp.path()).helper_path();
    std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
    std::fs::write(&helper, b"newly installed helper fixture").unwrap();

    run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut after_install,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    let checked = read_pro_status(temp.path()).expect("post-install Pro check");
    assert_eq!(checked["core_generation_id"], generation);
    assert_eq!(checked["attempts"], 2);
}

#[test]
fn newly_installed_pro_is_checked_against_durable_core_after_daemon_restart() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    persist_pro_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "owner": "daemon",
            "kind": "source_backed_pro_catch_up",
            "status": "error",
            "pending": true,
            "retryable": false,
            "core_generation_id": generation,
            "receipt_core_generation_id": null,
            "attempts": 1,
            "last_attempt_at_ms": 1,
            "error_code": "pro_not_installed",
            "last_error": "fixture helper was removed",
        }),
    )
    .unwrap();
    let helper = ProFilesystemLayout::new(temp.path()).helper_path();
    std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
    std::fs::write(&helper, b"reinstalled helper fixture").unwrap();

    let coordinator = CoreRefreshEngine::new();
    let mut restarted = DaemonRuntime::default();
    restore_daemon_consumer_retries(&mut restarted, temp.path());
    assert!(restarted.sidecar_drain.pro_attempted_generation.is_none());

    run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut restarted,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    let checked = read_pro_status(temp.path()).expect("post-reinstall Pro check");
    assert_eq!(checked["core_generation_id"], generation);
    assert_eq!(checked["attempts"], 2);
    assert_eq!(
        restarted.sidecar_drain.pro_attempted_generation.as_deref(),
        Some(generation.as_str())
    );
}

#[test]
fn active_queries_defer_due_consumer_retry_only_until_fairness_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let _hooks = install_jobs(
        calls.clone(),
        Some(json!({
            "status": "ready",
            "source_generation_ready": true,
            "source_work_remaining": false,
        })),
    );
    let activity = std::sync::Arc::new(super::DaemonQueryActivity::new());
    let _request = activity.begin_request().expect("foreground query");
    let mut runtime = DaemonRuntime::default();
    runtime.semantic_retry.consecutive_failures = 1;
    runtime.semantic_retry.retry_not_before = Some(std::time::Instant::now());
    runtime.sidecar_drain.generation = Some(generation.clone());
    runtime.sidecar_drain.pro_attempted_generation = Some(generation.clone());

    let deferred = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        Some(activity.as_ref()),
        None,
    )
    .unwrap();
    assert!(!deferred.did_work);
    assert!(runtime.consumer_retry_deferral.retry_at.is_some());
    assert!(calls.borrow().is_empty());

    runtime.consumer_retry_deferral.retry_at = Some(std::time::Instant::now());
    let fair = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        Some(activity.as_ref()),
        None,
    )
    .unwrap();

    assert!(!fair.failed);
    assert!(runtime.consumer_retry_deferral.retry_at.is_none());
    assert_eq!(&*calls.borrow(), &["semantic_index"]);
    assert_eq!(runtime.semantic_retry.consecutive_failures, 0);
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
fn semantic_retry_runs_across_core_backoff_and_recovers_independently() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    write_daemon_job_status(
        &daemon_core_refresh_job_path(temp.path()),
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
    first.sidecar_drain.pro_attempted_generation = Some(generation.clone());
    {
        let _hooks = install_jobs(
            calls.clone(),
            Some(json!({
                "status": "failed",
                "failure_class": "retryable",
                "retryable": true,
                "last_error": "injected semantic failure",
            })),
        );
        let iteration = run_daemon_scheduler_cycle_with_activity(
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
    assert_eq!(first.history_retry.consecutive_failures, 1);

    let mut restarted = DaemonRuntime::default();
    restarted.history_retry.record_failure();
    restore_daemon_consumer_retries(&mut restarted, temp.path());
    restarted.sidecar_drain.pro_attempted_generation = Some(generation.clone());
    assert!(!restarted.semantic_retry.ready());
    restarted.semantic_retry.retry_not_before = None;
    restarted.semantic_retry.retry_not_before_at_ms = None;
    calls.borrow_mut().clear();
    {
        let _hooks = install_jobs(
            calls.clone(),
            Some(json!({
                "status": "ready",
                "source_generation_ready": true,
                "source_work_remaining": false,
            })),
        );
        let iteration = run_daemon_scheduler_cycle_with_activity(
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
