use super::*;
use rusqlite::Connection;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use tempfile::TempDir;

const MACHINE: &str = "kiro-nativepath-test-machine";
const PRIVATE_OUTPUT: &str = "KIRO_PRIVATE_SUCCESS_OUTPUT_MUST_NOT_ENTER_CORE";

fn create_source() -> (TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "create table conversations_v2 (
                key text not null,
                conversation_id text not null,
                value text not null,
                created_at integer,
                updated_at integer
            );
            create table conversations (
                key text not null,
                value text not null
            );",
        )
        .unwrap();
    drop(connection);
    (temp, path)
}

fn import(
    path: &Path,
    store: &mut Store,
    work_limit: CaptureWorkLimit,
    profile: ImportProfile,
) -> ProviderImportSummary {
    try_import(path, store, work_limit, profile).unwrap()
}

fn try_import(
    path: &Path,
    store: &mut Store,
    work_limit: CaptureWorkLimit,
    profile: ImportProfile,
) -> Result<ProviderImportSummary> {
    import_kiro_native_path(
        path,
        store,
        ProviderAdapterContext {
            machine_id: MACHINE.to_owned(),
            source_path: Some(path.to_path_buf()),
            source_root: None,
            imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
        },
        ProviderImportOptions {
            capture_work_limit: work_limit,
            import_profile: profile,
            ..ProviderImportOptions::default()
        },
    )
}

fn history_entry(index: usize) -> Value {
    json!({
        "user": {
            "content": {"Prompt": {"prompt": format!("user-{index}")}},
            "timestamp": "2026-07-25T12:00:01Z"
        },
        "assistant": {
            "Response": {"content": format!("assistant-{index}")},
            "timestamp": "2026-07-25T12:00:02Z"
        }
    })
}

fn write_v2_conversation(path: &Path, history: Vec<Value>) {
    std::thread::sleep(std::time::Duration::from_millis(5));
    let connection = Connection::open(path).unwrap();
    let value = json!({"history": history}).to_string();
    let changed = connection
        .execute(
            "update conversations_v2
             set value = ?1, updated_at = coalesce(updated_at, 0) + 1
             where conversation_id = 'session'",
            [&value],
        )
        .unwrap();
    if changed == 0 {
        connection
            .execute(
                "insert into conversations_v2
                 (key, conversation_id, value, created_at, updated_at)
                 values ('/workspace', 'session', ?1, 1, 2)",
                [&value],
            )
            .unwrap();
    }
}

fn source_revision(path: &Path) -> String {
    KiroSource::acquire(path, path.to_path_buf(), None)
        .unwrap()
        .source_revision
}

#[test]
fn cursor_round_trips_exact_source_and_frontier_authority() {
    let cursor = KiroStoreCursor {
        version: KIRO_NATIVE_CURSOR_VERSION,
        provider: CaptureProvider::KiroCli.as_str().to_owned(),
        locator_identity: "locator".to_owned(),
        canonical_source_identity: "canonical".to_owned(),
        source_revision: "revision".to_owned(),
        frontier: KiroFrontier::initial(KiroTables {
            v2: true,
            legacy: true,
        }),
        retirement: None,
        terminal: false,
        generation: 3,
        rejected_records: 2,
    };
    assert_eq!(
        KiroStoreCursor::decode(&cursor.encode().unwrap()).unwrap(),
        cursor
    );
}

#[test]
fn scanner_pages_entries_and_keeps_output_bodies_out_of_core() {
    let (_temp, path) = create_source();
    let connection = Connection::open(&path).unwrap();
    let history = (0..35)
        .map(|index| {
            json!({
                "user": {"content": {"Prompt": {"prompt": format!("user-{index}")}}},
                "assistant": {
                    "ToolUse": {
                        "tool_uses": [{
                            "name": "shell",
                            "input": {"command": "pwd"},
                            "result": {"stdout": format!("PRIVATE-OUTPUT-{index}")}
                        }]
                    },
                    "tool_results": {"call": {"output": format!("PRIVATE-OUTPUT-{index}")}}
                }
            })
        })
        .collect::<Vec<_>>();
    connection
        .execute(
            "insert into conversations_v2
             (key, conversation_id, value, created_at, updated_at)
             values ('/workspace', 'session', ?1, 1, 2)",
            [json!({"history": history}).to_string()],
        )
        .unwrap();
    drop(connection);
    let source = KiroSource::acquire(&path, path.clone(), None).unwrap();
    let mut scanner = KiroScanner::new(
        &source,
        KiroFrontier::initial(source.tables),
        DateTime::<Utc>::UNIX_EPOCH,
    )
    .unwrap();
    let first = scanner.next_page().unwrap().unwrap();
    assert!(!first.terminal);
    assert!(first.events.len() <= KIRO_PAGE_HISTORY_ITEMS * 2);
    assert!(first
        .events
        .iter()
        .all(|event| { !event.event.payload.to_string().contains("PRIVATE-OUTPUT") }));
    let second = scanner.next_page().unwrap().unwrap();
    assert!(second.terminal);
    assert!(scanner.next_page().unwrap().is_none());
}

#[test]
fn malformed_row_is_rejected_without_hiding_valid_sibling() {
    let (_temp, path) = create_source();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "insert into conversations_v2 values ('/bad', 'bad', '{', 1, 1);
             insert into conversations_v2 values (
                 '/good', 'good',
                 '{\"history\":[{\"user\":{\"content\":{\"Prompt\":{\"prompt\":\"healthy\"}}}}]}',
                 2, 2
             );",
        )
        .unwrap();
    drop(connection);
    let source = KiroSource::acquire(&path, path.clone(), None).unwrap();
    let mut scanner = KiroScanner::new(
        &source,
        KiroFrontier::initial(source.tables),
        DateTime::<Utc>::UNIX_EPOCH,
    )
    .unwrap();
    let rejected = scanner.next_page().unwrap().unwrap();
    assert_eq!(rejected.rejections.len(), 1);
    let healthy = scanner.next_page().unwrap().unwrap();
    assert_eq!(healthy.events.len(), 1);
    assert!(healthy.terminal);
}

#[test]
fn production_lifecycle_rewrites_stable_events_and_retires_truncation() {
    let (temp, path) = create_source();
    write_v2_conversation(&path, vec![history_entry(0), history_entry(1)]);
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let fresh = import(
        &path,
        &mut store,
        CaptureWorkLimit::Drain,
        ImportProfile::CoreOnly,
    );
    assert_eq!(fresh.imported_sessions, 1);
    assert_eq!(fresh.imported_events, 4);
    assert!(!fresh.work_remaining);
    let session = store
        .session_by_external_session(CaptureProvider::KiroCli, "session")
        .unwrap()
        .unwrap();
    let original = store.events_for_session(session.id).unwrap();
    let original_ids = original.iter().map(|event| event.id).collect::<Vec<_>>();
    let fresh_revision = source_revision(&path);

    assert_eq!(
        import(
            &path,
            &mut store,
            CaptureWorkLimit::Drain,
            ImportProfile::CoreOnly,
        )
        .work_result(),
        ProviderImportWorkResult::NoOp
    );
    drop(store);
    let mut store = Store::open(&store_path).unwrap();

    let mut rewritten = history_entry(0);
    rewritten["user"]["content"]["Prompt"]["prompt"] = json!("rewritten-user");
    write_v2_conversation(&path, vec![rewritten, history_entry(1)]);
    let rewritten_revision = source_revision(&path);
    assert_ne!(rewritten_revision, fresh_revision);
    assert_eq!(
        import(
            &path,
            &mut store,
            CaptureWorkLimit::Drain,
            ImportProfile::CoreOnly,
        )
        .work_result(),
        ProviderImportWorkResult::Changed
    );
    let after_rewrite = store.events_for_session(session.id).unwrap();
    assert_eq!(
        after_rewrite
            .iter()
            .map(|event| event.id)
            .collect::<Vec<_>>(),
        original_ids
    );
    assert!(serde_json::to_string(&after_rewrite)
        .unwrap()
        .contains("rewritten-user"));

    write_v2_conversation(&path, vec![history_entry(0)]);
    let truncated_revision = source_revision(&path);
    assert_ne!(truncated_revision, rewritten_revision);
    let truncated = import(
        &path,
        &mut store,
        CaptureWorkLimit::Drain,
        ImportProfile::CoreOnly,
    );
    assert_eq!(
        truncated.work_result(),
        ProviderImportWorkResult::Changed,
        "{truncated:?}"
    );
    assert!(store
        .get_event(original_ids[2])
        .unwrap()
        .sync
        .deleted_at
        .is_some());
    assert!(store
        .get_event(original_ids[3])
        .unwrap()
        .sync
        .deleted_at
        .is_some());

    write_v2_conversation(&path, vec![history_entry(0), history_entry(1)]);
    import(
        &path,
        &mut store,
        CaptureWorkLimit::Drain,
        ImportProfile::CoreOnly,
    );
    assert!(store
        .get_event(original_ids[2])
        .unwrap()
        .sync
        .deleted_at
        .is_none());
    assert!(store
        .get_event(original_ids[3])
        .unwrap()
        .sync
        .deleted_at
        .is_none());

    let (replacement_temp, replacement) = create_source();
    write_v2_conversation(&replacement, vec![history_entry(9)]);
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(
        import(
            &path,
            &mut store,
            CaptureWorkLimit::Drain,
            ImportProfile::CoreOnly,
        )
        .work_result(),
        ProviderImportWorkResult::Changed
    );
    drop(replacement_temp);

    let routed = original_ids[0];
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        import(
            &path,
            &mut store,
            CaptureWorkLimit::Drain,
            ImportProfile::CoreOnly,
        )
        .work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store.authorized_source_route_for_event(routed).is_err());
}

#[test]
fn one_safe_group_resumes_after_restart_through_generation_completion() {
    let (temp, path) = create_source();
    write_v2_conversation(&path, (0..35).map(history_entry).collect());
    let store_path = temp.path().join("core.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let first = import(
        &path,
        &mut store,
        CaptureWorkLimit::OneSafeGroup,
        ImportProfile::CoreOnly,
    );
    assert!(first.work_remaining);
    assert!(first.imported_events <= KIRO_PAGE_HISTORY_ITEMS * 2);
    drop(store);

    let mut store = Store::open(&store_path).unwrap();
    for attempt in 0..6 {
        let next = import(
            &path,
            &mut store,
            CaptureWorkLimit::OneSafeGroup,
            ImportProfile::CoreOnly,
        );
        if !next.work_remaining {
            let session = store
                .session_by_external_session(CaptureProvider::KiroCli, "session")
                .unwrap()
                .unwrap();
            assert_eq!(store.events_for_session(session.id).unwrap().len(), 70);
            assert!(attempt < 5);
            return;
        }
    }
    panic!("Kiro OneSafeGroup import did not reach a terminal generation");
}

#[test]
fn corrupt_replacement_does_not_rollback_or_retire_committed_core() {
    let (temp, path) = create_source();
    write_v2_conversation(&path, vec![history_entry(0)]);
    let mut store = Store::open(temp.path().join("core.sqlite")).unwrap();
    import(
        &path,
        &mut store,
        CaptureWorkLimit::Drain,
        ImportProfile::CoreOnly,
    );
    let session = store
        .session_by_external_session(CaptureProvider::KiroCli, "session")
        .unwrap()
        .unwrap();
    let event = store.events_for_session(session.id).unwrap()[0].id;

    std::fs::write(&path, b"not a sqlite database").unwrap();
    assert!(try_import(
        &path,
        &mut store,
        CaptureWorkLimit::Drain,
        ImportProfile::CoreOnly,
    )
    .is_err());
    assert!(store.get_event(event).unwrap().sync.deleted_at.is_none());
    assert!(store.authorized_source_route_for_event(event).is_ok());
}

#[test]
fn released_cursor_and_positional_hash_migrate_exactly_in_place() {
    let (temp, path) = create_source();
    write_v2_conversation(&path, vec![history_entry(0)]);
    let mut store = Store::open(temp.path().join("core.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: MACHINE.to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
    };
    let options = ProviderImportOptions::default();
    let source = KiroSource::acquire(&path, path.clone(), None).unwrap();
    let mut scanner = KiroScanner::new(
        &source,
        KiroFrontier::initial(source.tables),
        context.imported_at,
    )
    .unwrap();
    let page = scanner.next_page().unwrap().unwrap();
    let fact = page.fact.as_ref().unwrap();
    let prepared = &page.events[0];
    let raw_source_path = source.canonical_path.display().to_string();
    let resolution = store
        .reconcile_provider_source_locator(&kiro_locator_observation(&source, &context).unwrap())
        .unwrap();
    let source_id = provider_scoped_source_uuid(
        CaptureProvider::KiroCli,
        &fact.provider_session_id,
        KIRO_SQLITE_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    store
        .upsert_capture_source(&kiro_capture_source(
            &source,
            &context,
            fact,
            source_id,
            &raw_source_path,
            &source.configured_source_root.display().to_string(),
            &resolution.canonical_source_identity,
        ))
        .unwrap();
    store
        .bind_capture_source_provider_route(source_id, &resolution.route_binding())
        .unwrap();
    let session_id = provider_import_session_uuid(
        &store,
        CaptureProvider::KiroCli,
        &fact.provider_session_id,
        source_id,
        Some(&resolution.canonical_source_identity),
    )
    .unwrap();
    store
        .upsert_session(&kiro_session(
            &context, &options, fact, source_id, session_id,
        ))
        .unwrap();
    let legacy_hash = prepared
        .event
        .metadata
        .get("legacy_provider_event_hash")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    let legacy_identity = provider_event_import_identity_with_exact_legacy_source(
        &store,
        CaptureProvider::KiroCli,
        &fact.provider_session_id,
        source_id,
        prepared.event.provider_event_index,
        prepared.event.provider_event_index,
        &legacy_hash,
        None,
        Some(prepared.event.provider_event_index),
        true,
    )
    .unwrap();
    let released = kiro_core_event(
        &context,
        &options,
        &fact.provider_session_id,
        source_id,
        session_id,
        1,
        &prepared.event,
        &legacy_hash,
        ProviderEventHashAuthority::ProviderSupplied,
        &legacy_identity,
    )
    .unwrap();
    store.upsert_event(&released).unwrap();
    let released_id = released.id;
    let released_cursor = CertifiedProviderCursor::new(
        "released-kiro-source-revision",
        2,
        4,
        crate::native_source::NativePosition::new(KIRO_LEGACY_POSITION_KIND, vec![0]).unwrap(),
        crate::provider::importer::BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: MACHINE.to_owned(),
            stream: source.cursor_stream.clone(),
            cursor: released_cursor,
            last_synced_at: None,
            timestamps: timestamps(DateTime::<Utc>::UNIX_EPOCH),
        })
        .unwrap();
    drop(source);

    import(
        &path,
        &mut store,
        CaptureWorkLimit::Drain,
        ImportProfile::CoreOnly,
    );
    let migrated = store.get_event(released_id).unwrap();
    let normalized_hash = compute_payload_hash(&migrated.payload["body"]).unwrap();
    assert_eq!(migrated.id, released_id);
    assert!(migrated
        .dedupe_key
        .as_deref()
        .unwrap()
        .ends_with(&normalized_hash));
    assert_eq!(
        migrated.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert_eq!(store.events_for_session(session_id).unwrap().len(), 2);
}

#[derive(Default)]
struct RecordingOutputSink {
    fail_next: AtomicBool,
    behind: AtomicUsize,
    pages: AtomicUsize,
    progress: Mutex<Option<ProOutputProgress>>,
}

impl ProOutputSink for RecordingOutputSink {
    fn inventory_generation(&self) -> u64 {
        11
    }

    fn materializer_revision(&self) -> &str {
        "kiro-test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().clone())
    }

    fn materialize_page(
        &self,
        page: crate::ProOutputMaterializationPage,
    ) -> std::result::Result<crate::ProOutputPageResult, ProOutputSinkError> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(ProOutputSinkError::new("injected", "injected failure"));
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        let committed_cursor = page.next_safe_cursor.clone();
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(committed_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        Ok(crate::ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
            materialized_facts: 0,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn pro_failure_never_blocks_core_and_later_replay_is_independent() {
    let (temp, path) = create_source();
    write_v2_conversation(
        &path,
        vec![json!({
            "user": {
                "content": {"Prompt": {"prompt": "run the tool"}},
                "timestamp": "2026-07-25T12:00:01Z"
            },
            "assistant": {
                "ToolUse": {
                    "tool_uses": [{
                        "name": "shell",
                        "input": {"command": "pwd"},
                        "result": {"stdout": PRIVATE_OUTPUT}
                    }]
                },
                "tool_results": {"call": {"output": PRIVATE_OUTPUT}},
                "timestamp": "2026-07-25T12:00:02Z"
            }
        })],
    );
    let mut store = Store::open(temp.path().join("core.sqlite")).unwrap();
    let sink = Arc::new(RecordingOutputSink::default());
    sink.fail_next.store(true, Ordering::SeqCst);

    let core = import(
        &path,
        &mut store,
        CaptureWorkLimit::Drain,
        ImportProfile::CoreAndPro(sink.clone()),
    );
    assert_eq!(core.imported_events, 2);
    assert!(sink.behind.load(Ordering::SeqCst) > 0);
    let session = store
        .session_by_external_session(CaptureProvider::KiroCli, "session")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    let rendered = serde_json::to_string(&(session, events)).unwrap();
    assert!(!rendered.contains(PRIVATE_OUTPUT));

    import(
        &path,
        &mut store,
        CaptureWorkLimit::Drain,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(sink.pages.load(Ordering::SeqCst), 1);
    assert!(sink.progress.lock().unwrap().as_ref().unwrap().terminal);

    import(
        &path,
        &mut store,
        CaptureWorkLimit::Drain,
        ImportProfile::ProReplayOnly(sink.clone()),
    );
    assert_eq!(sink.pages.load(Ordering::SeqCst), 1);
}
