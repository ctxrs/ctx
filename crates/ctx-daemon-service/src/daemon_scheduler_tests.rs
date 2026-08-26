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
    derive_event_id, derive_session_id, AgentScope, CertifiedSource, CoreRecord,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceFrontier, SourceObservation, TypedKey,
};
use ctx_history_index::{
    CompiledSearchFilter, EventRecord, GenerationWriter, LexicalExecution, LexicalMode,
    SourceRouteIdentity, VerifiedIndex, WriterOptions, MAX_SEMANTIC_EVENT_PAGE_ITEMS,
};
use serde_json::{json, Value};

use crate::{
    config::{AppConfig, DaemonMode},
    daemon::{install_daemon_test_job_hooks, DaemonTestJobHooks},
    source_backed_refresh_coordinator::EventWatermark,
    source_backed_refresh_coordinator::{
        coordinate_source_backed_refresh, publish_authoritative_empty_generation_for_test,
        publish_authoritative_empty_generation_with_route_results_for_test,
        source_backed_index_root, CoreRefreshEngine, SourceBackedRefreshCurrent,
        SourceBackedRefreshExecution, SourceBackedRefreshMode, SourceBackedRefreshPublication,
        SourceBackedRefreshRouteResult, SourceBackedRefreshSourceFailure,
        SourceBackedRefreshTimings,
    },
    test_support::{
        current_semantic_vector_schema_version, seed_filter_unaware_semantic_state,
        semantic_contract_fingerprint, semantic_generation_is_ready_empty, semantic_vector_path,
    },
    CoreGenerationPublished, CoreGenerationPublishedPort, DaemonRunArgs,
};

use super::{
    daemon_core_refresh_job_path, daemon_job_should_backoff,
    daemon_mode_runs_core_semantic_projection, daemon_semantic_job_path, read_daemon_job_status,
    record_daemon_job_retry, record_source_refresh_retry, restore_daemon_consumer_retries,
    run_pending_core_refresh, write_daemon_job_status, DaemonRetryBackoff, DaemonRuntime,
    DaemonSchedulerCycleContext, DaemonSchedulerPorts, DaemonSemanticJobPorts,
};

const READINESS_QUERY: &str = "readiness-boundary-regression";

fn complete_lexical_candidates(
    index: &VerifiedIndex,
    query: &str,
    limit: usize,
) -> Vec<EventRecord> {
    let queries = [query];
    let filter = CompiledSearchFilter::compile(Default::default()).unwrap();
    let observed = index
        .execute_lexical(LexicalExecution::new(
            LexicalMode::Search(&queries),
            &filter,
            limit,
        ))
        .unwrap();
    assert!(observed.batch.complete, "lexical search must be complete");
    observed
        .batch
        .candidates
        .into_iter()
        .map(|candidate| {
            let event = index
                .event_by_id(candidate.event.event_id)
                .unwrap()
                .expect("selected lexical event must hydrate");
            assert_eq!(
                event.event_id.digest(),
                candidate.event.event_identity_digest,
                "selected lexical event must preserve its exact identity"
            );
            event
        })
        .collect()
}

fn daemon_args() -> DaemonRunArgs {
    DaemonRunArgs {
        loop_interval_seconds: None,
        max_chunks: None,
        handle_process_signals: false,
        force: false,
        profile: crate::DaemonRunProfile::Persistent,
        start_mode: None,
        trigger_command: None,
        supervisor: crate::DaemonSupervisor::User,
    }
}

fn run_daemon_scheduler_cycle_with_activity(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<std::time::Instant>,
    semantic_enabled: bool,
    query_activity: Option<&crate::query_service::DaemonQueryActivity>,
    source_refresh: Option<&CoreRefreshEngine>,
) -> anyhow::Result<crate::daemon::DaemonIteration> {
    run_daemon_scheduler_cycle_with_activity_and_notification(
        args,
        data_root,
        runtime,
        deadline,
        semantic_enabled,
        query_activity,
        source_refresh,
        &crate::test_support::GENERATION_PUBLISHED,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_daemon_scheduler_cycle_with_activity_and_notification(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<std::time::Instant>,
    semantic_enabled: bool,
    query_activity: Option<&crate::query_service::DaemonQueryActivity>,
    source_refresh: Option<&CoreRefreshEngine>,
    generation_published: &dyn CoreGenerationPublishedPort,
) -> anyhow::Result<crate::daemon::DaemonIteration> {
    super::run_daemon_scheduler_cycle_with_activity(
        args,
        data_root,
        runtime,
        DaemonSchedulerCycleContext {
            deadline,
            semantic_enabled,
            query_activity,
            source_refresh,
        },
        DaemonSchedulerPorts {
            generation_published,
            semantic: DaemonSemanticJobPorts {
                artifact_fetcher: &crate::test_support::ARTIFACT,
                config: &crate::test_support::CONFIG,
            },
            observation: &crate::test_support::OBSERVATION,
        },
    )
}

fn publish_empty_core_generation(data_root: &Path) -> String {
    publish_authoritative_empty_generation_for_test(
        &source_backed_index_root(data_root),
        "daemon-scheduler-empty-core-fixture",
        ctx_history_refresh::RefreshOperation::Refresh,
        SourceBackedRefreshScope::All,
        None,
    )
    .unwrap()
    .generation_id
}

#[path = "daemon_scheduler_tests/refresh_retry.rs"]
mod refresh_retry;

fn publish_empty_authoritative_generation(
    execution: &SourceBackedRefreshExecution<'_>,
) -> SourceBackedRefreshPublication {
    publish_empty_authoritative_generation_with_route_results(execution, None)
}

fn publish_empty_authoritative_generation_with_route_results(
    execution: &SourceBackedRefreshExecution<'_>,
    route_results: Option<Vec<SourceBackedRefreshRouteResult>>,
) -> SourceBackedRefreshPublication {
    let mut publication = publish_authoritative_empty_generation_with_route_results_for_test(
        execution.index_root,
        execution.request_id,
        execution.operation,
        execution.admitted_refresh().publication_scope().clone(),
        execution.explicit_source_catalog.cloned(),
        route_results,
    )
    .unwrap();
    publication.timings = SourceBackedRefreshTimings {
        discovery_us: 1,
        scan_stage_us: 1,
        commit_us: 1,
    };
    publication
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
        source.clone(),
        0,
        "message",
        "daemon-scheduler-test-v1",
        format!("exact lexical hit for {READINESS_QUERY}"),
    )
    .unwrap();
    record.provider_session_id = Some("readiness-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(0));
    record.occurred_at_unix_ms = Some(1_700_000_000_000);
    record.role = Some("assistant".to_owned());
    record.agent_scope = Some(AgentScope::Primary);
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
        source.clone(),
        sequence,
        "message",
        "daemon-scheduler-semantic-catch-up-test-v1",
        format!("eligible semantic catch-up event {sequence}"),
    )
    .unwrap();
    record.provider_session_id = Some("semantic-catch-up-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(sequence));
    record.occurred_at_unix_ms = Some(1_700_000_000_000 + sequence as i64);
    record.role = Some("user".to_owned());
    record.agent_scope = Some(AgentScope::Primary);
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
    coordinate_source_backed_refresh(
        &crate::test_support::AVAILABILITY,
        data_root,
        SourceBackedRefreshMode::Off,
    )
    .unwrap()
    .pin
    .generation_id()
    .to_owned()
}

#[derive(Default)]
struct FailingGenerationPublished {
    calls: AtomicUsize,
    observed: Mutex<Option<CoreGenerationPublished>>,
}

impl CoreGenerationPublishedPort for FailingGenerationPublished {
    fn notify(
        &self,
        _data_root: &Path,
        publication: &CoreGenerationPublished,
    ) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.observed.lock().unwrap() = Some(publication.clone());
        anyhow::bail!("injected publication notification failure")
    }
}

#[test]
fn notification_failure_cannot_revoke_a_ready_searchable_core_publication() {
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
    let notification = FailingGenerationPublished::default();
    let core = run_daemon_scheduler_cycle_with_activity_and_notification(
        &daemon_args(),
        &data_root,
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
        &notification,
    )
    .unwrap();

    let core_job = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root))
        .expect("Core terminal receipt");
    let refresh_off = coordinate_source_backed_refresh(
        &crate::test_support::AVAILABILITY,
        &data_root,
        SourceBackedRefreshMode::Off,
    )
    .unwrap();
    let pinned_generation = refresh_off.pin.generation_id().to_owned();
    let index = refresh_off.pin.into_index();
    let hits = complete_lexical_candidates(&index, READINESS_QUERY, 10);
    let published_generation = core_job["published_generation"]
        .as_str()
        .expect("published generation");
    assert_eq!(core_job["status"], "completed", "{core_job:#}");
    assert_eq!(core_job["request_state"], "published", "{core_job:#}");
    assert_eq!(core_job["progress"]["phase"], "published", "{core_job:#}");
    assert_eq!(core_job["progress"]["completed_sources"], 1);
    assert_eq!(core_job["progress"]["total_sources"], 1);
    assert!(core_job.get("semantic_projection").is_none());
    assert_eq!(pinned_generation, published_generation);
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].provider_session_id.as_deref(),
        Some("readiness-session")
    );

    assert!(core.did_work);
    assert!(!core.failed);
    assert!(core.continue_immediately);
    assert_eq!(
        runtime.sidecar_drain.generation.as_deref(),
        Some(published_generation)
    );
    assert_eq!(notification.calls.load(Ordering::SeqCst), 1);
    let observed = notification.observed.lock().unwrap();
    let observed = observed.as_ref().expect("bounded Core notification");
    assert_eq!(observed.generation_id(), published_generation);
    assert_eq!(observed.previous_generation_id(), None);
    assert!(observed.generation_changed());
    assert_eq!(observed.source_count(), 1);
    assert_eq!(observed.indexed_document_count(), 1);
    assert_eq!(observed.complete_record_count(), 1);
    assert_eq!(observed.retained_record_count(), 1);
    assert_eq!(observed.rejected_record_count(), 0);
    assert_eq!(observed.ignored_record_count(), 0);
    assert_eq!(observed.certified_source_bytes(), 128);
}

#[test]
fn finite_core_worker_does_not_notify_adjacent_maintenance() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            Ok(publish_empty_authoritative_generation(&execution))
        },
    ));
    coordinator.enqueue_for_test(None);
    let mut args = daemon_args();
    args.profile = crate::DaemonRunProfile::FiniteCoreWorker;
    let mut runtime = DaemonRuntime::default();
    let notification = FailingGenerationPublished::default();

    let core = run_daemon_scheduler_cycle_with_activity_and_notification(
        &args,
        temp.path(),
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
        &notification,
    )
    .unwrap();

    assert!(core.did_work);
    assert!(!core.failed);
    assert_eq!(notification.calls.load(Ordering::SeqCst), 0);
    let job = read_daemon_job_status(&daemon_core_refresh_job_path(temp.path())).unwrap();
    assert_eq!(job["status"], "completed", "{job:#}");
    assert_eq!(job["request_state"], "published", "{job:#}");
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
fn finite_core_worker_never_turns_dirty_routes_into_background_work() {
    let temp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let executor_calls = Arc::clone(&calls);
    let coordinator =
        CoreRefreshEngine::with_executor(Arc::new(move |_: SourceBackedRefreshExecution<'_>| {
            executor_calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!(
                "finite worker must not run dirty-route work"
            ))
        }));
    let route = SourceRouteIdentity::from_sha256("91".repeat(32)).unwrap();
    coordinator.initialize_watch_route_authority(BTreeSet::from([route.clone()]));
    coordinator.schedule_startup_route_reconciliation(
        BTreeSet::from([route]),
        EventWatermark::new(1, 0),
        super::source_route_ledger_now_ms().saturating_sub(1_000),
    );
    let mut args = daemon_args();
    args.profile = crate::DaemonRunProfile::FiniteCoreWorker;
    let mut runtime = DaemonRuntime::default();

    let idle = run_daemon_scheduler_cycle_with_activity(
        &args,
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
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!coordinator.has_pending_request());
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
fn one_core_cycle_then_scheduler_drains_semantic_consumer() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::with_executor(std::sync::Arc::new(
        |execution: SourceBackedRefreshExecution<'_>| {
            Ok(publish_empty_authoritative_generation(&execution))
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
    assert!(runtime
        .sidecar_drain
        .semantic_attempted_generation
        .is_none());
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
    assert_eq!(runtime.semantic_retry.consecutive_failures, 0);
    assert_eq!(pinned_generation(temp.path()), generation);

    let drained = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(!drained.continue_immediately);
}

#[test]
fn automatic_scheduler_restart_migrates_legacy_semantic_state_despite_ready_job() {
    let temp = tempfile::tempdir().unwrap();
    let generation = publish_empty_core_generation(temp.path());
    let vector_path = semantic_vector_path(temp.path());
    let contract_fingerprint = semantic_contract_fingerprint().unwrap();

    let mut initial_runtime = DaemonRuntime::default();
    let initial = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut initial_runtime,
        None,
        true,
        None,
        None,
    )
    .unwrap();
    assert!(initial.continue_immediately);
    let initial_job = read_daemon_job_status(&daemon_semantic_job_path(temp.path())).unwrap();
    assert_eq!(initial_job["status"], "ready");
    assert_eq!(initial_job["core_generation_id"], generation);
    assert_eq!(
        initial_job["source_contract_fingerprint"],
        contract_fingerprint
    );

    seed_filter_unaware_semantic_state(&vector_path).unwrap();
    let control = rusqlite::Connection::open(vector_path.join("state.sqlite")).unwrap();
    let seeded_version = control
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    assert_eq!(seeded_version, 5);
    drop(control);
    write_daemon_job_status(
        &daemon_semantic_job_path(temp.path()),
        &json!({
            "schema_version": 1,
            "status": "ready",
            "core_generation_id": generation,
            "source_generation_ready": true,
            "source_work_remaining": false,
        }),
    )
    .unwrap();

    let mut restarted_runtime = DaemonRuntime::default();
    assert!(super::semantic_generation_needs_catch_up(
        temp.path(),
        &generation
    ));
    for _ in 0..8 {
        run_daemon_scheduler_cycle_with_activity(
            &daemon_args(),
            temp.path(),
            &mut restarted_runtime,
            None,
            true,
            None,
            None,
        )
        .unwrap();
        if !super::semantic_generation_needs_catch_up(temp.path(), &generation) {
            break;
        }
    }

    assert!(!super::semantic_generation_needs_catch_up(
        temp.path(),
        &generation
    ));
    let migrated_job = read_daemon_job_status(&daemon_semantic_job_path(temp.path())).unwrap();
    assert_eq!(migrated_job["status"], "ready");
    assert_eq!(migrated_job["core_generation_id"], generation);
    assert_eq!(
        migrated_job["source_contract_fingerprint"],
        contract_fingerprint
    );
    let control = rusqlite::Connection::open(vector_path.join("state.sqlite")).unwrap();
    let migrated_version = control
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    assert_eq!(migrated_version, current_semantic_vector_schema_version());
    drop(control);
    assert!(semantic_generation_is_ready_empty(&vector_path, &generation).unwrap());
}

#[test]
fn idle_semantic_catch_up_continues_past_one_page_and_drains_to_terminal() {
    const ELIGIBLE_EVENTS: u64 = MAX_SEMANTIC_EVENT_PAGE_ITEMS as u64 + 1;

    let temp = tempfile::tempdir().unwrap();
    let generation = publish_semantic_catch_up_generation(temp.path(), ELIGIBLE_EVENTS);
    let index = coordinate_source_backed_refresh(
        &crate::test_support::AVAILABILITY,
        temp.path(),
        SourceBackedRefreshMode::Off,
    )
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
        loop_interval_seconds: None,
        max_chunks: None,
        handle_process_signals: false,
        force: false,
        profile: crate::DaemonRunProfile::Persistent,
        start_mode: None,
        trigger_command: None,
        supervisor: crate::DaemonSupervisor::User,
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
    assert!(!super::daemon_semantic_job_path(temp.path()).exists());
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
