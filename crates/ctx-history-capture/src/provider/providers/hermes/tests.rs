use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_store::{decode_native_path_committed_cursor, Store};
use rusqlite::Connection;
use serde_json::{json, Value};

use super::{
    layout::HermesSchema,
    sqlite::{
        HermesFrontier, HermesNativeRecord, HermesPhase, HermesRowReader, HERMES_FRONTIER_VERSION,
        HERMES_LOCATOR_KIND,
    },
    *,
};
use crate::{
    test_support_paths::tempdir, ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage,
    ProOutputPageResult, ProOutputProgress, ProOutputSink, ProOutputSinkError,
};

const MACHINE: &str = "hermes-nativepath-test-machine";

#[derive(Default)]
struct RecordingSink {
    fail_once: AtomicBool,
    fail_observe: AtomicBool,
    progress: Mutex<Option<ProOutputProgress>>,
    outputs: Mutex<Vec<(OutputOutcome, Vec<u8>)>>,
    saw_core_before_output: AtomicBool,
    store_path: Mutex<Option<PathBuf>>,
    behind: AtomicUsize,
}

impl RecordingSink {
    fn for_store(store: &Store) -> Self {
        Self {
            store_path: Mutex::new(Some(store.path().to_path_buf())),
            ..Self::default()
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        9
    }

    fn materializer_revision(&self) -> &str {
        "hermes-nativepath-test-materializer-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        if self.fail_observe.swap(false, Ordering::SeqCst) {
            return Err(ProOutputSinkError::new(
                "hermes_test_observe_failure",
                "transient progress failure",
            ));
        }
        Ok(self.progress.lock().unwrap().clone())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        if self.fail_once.swap(false, Ordering::SeqCst) {
            return Err(ProOutputSinkError::new(
                "hermes_test_failure",
                "retry the exact output page",
            ));
        }
        if let Some(path) = self.store_path.lock().unwrap().as_ref() {
            let core = Store::open_read_only(path)
                .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
            self.saw_core_before_output.store(
                !core
                    .list_sessions()
                    .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
                    .is_empty(),
                Ordering::SeqCst,
            );
        }
        self.outputs.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|output| (output.outcome.outcome, output.content.clone())),
        );
        let committed_cursor = page.next_safe_cursor.clone();
        *self.progress.lock().unwrap() = Some(ProOutputProgress {
            source_epoch: page.source_epoch,
            observed_revision: page.observed_revision.clone(),
            cursor: Some(committed_cursor.clone()),
            parser_revision: page.parser_revision.clone(),
            materializer_revision: page.materializer_revision.clone(),
            terminal: page.terminal,
        });
        Ok(ProOutputPageResult {
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

fn context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: MACHINE.to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: path.parent().map(Path::to_path_buf),
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
    }
}

fn options(import_profile: ImportProfile) -> ProviderImportOptions {
    ProviderImportOptions {
        import_profile,
        ..ProviderImportOptions::default()
    }
}

fn import(
    path: &Path,
    store: &mut Store,
    import_profile: ImportProfile,
) -> Result<ProviderImportSummary> {
    import_with_limit(path, store, import_profile, crate::CaptureWorkLimit::Drain)
}

fn import_with_limit(
    path: &Path,
    store: &mut Store,
    import_profile: ImportProfile,
    capture_work_limit: crate::CaptureWorkLimit,
) -> Result<ProviderImportSummary> {
    let mut options = options(import_profile);
    options.capture_work_limit = capture_work_limit;
    import_hermes_nativepath(path, store, context(path), options)
}

fn core_cursor(path: &Path, store: &Store) -> ctx_history_core::SyncCursor {
    let canonical = std::fs::canonicalize(path).unwrap();
    let locator_identity = crate::provider::importer::provider_path_identity(&canonical).unwrap();
    let stream = crate::provider::importer::provider_source_cursor_stream_for_path(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .expect("Hermes Core cursor")
}

fn core_cursor_value(path: &Path, store: &Store) -> Value {
    let stored = core_cursor(path, store);
    let committed = decode_native_path_committed_cursor(&stored.cursor).unwrap();
    serde_json::from_str(committed.provider_cursor()).unwrap()
}

fn stored_events(store: &Store) -> Vec<ctx_history_core::Event> {
    let mut events = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .collect::<Vec<_>>();
    events.sort_by_key(|event| event.id);
    events
}

fn create_fixture(path: &Path, session: &str, extra_messages: usize) {
    let mut conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            parent_session_id TEXT,
            started_at REAL NOT NULL
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT,
            tool_call_id TEXT,
            tool_name TEXT,
            timestamp REAL NOT NULL,
            finish_reason TEXT,
            active INTEGER NOT NULL DEFAULT 1,
            compacted INTEGER NOT NULL DEFAULT 0
        );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, source, started_at) VALUES (?1, 'acp', 1782259200.0)",
        [session],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages
         (session_id, role, content, timestamp, finish_reason)
         VALUES (?1, 'assistant', 'ordinary core message', 1782259201.0, 'stop')",
        [session],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages
         (session_id, role, content, tool_call_id, tool_name, timestamp, finish_reason)
         VALUES (?1, 'tool', 'successful private bytes', 'call-success', 'shell',
                 1782259202.0, 'success')",
        [session],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages
         (session_id, role, content, tool_call_id, tool_name, timestamp, finish_reason)
         VALUES (?1, 'tool', 'failed diagnostic bytes', 'call-failure', 'shell',
                 1782259203.0, 'error')",
        [session],
    )
    .unwrap();
    let transaction = conn.transaction().unwrap();
    for index in 0..extra_messages {
        transaction
            .execute(
                "INSERT INTO messages
                 (session_id, role, content, timestamp, finish_reason)
                 VALUES (?1, 'assistant', ?2, ?3, 'stop')",
                rusqlite::params![
                    session,
                    format!("Hermes bounded message {index} hermesneedle{index}"),
                    1782259300.0 + index as f64,
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}

fn create_empty_fixture(path: &Path) {
    create_fixture(path, "removed-empty-session", 0);
    Connection::open(path)
        .unwrap()
        .execute_batch("DELETE FROM messages; DELETE FROM sessions;")
        .unwrap();
}

#[test]
fn provider_frontier_and_locator_are_exact_and_versioned() {
    let frontier = HermesFrontier {
        phase: HermesPhase::Messages,
        next_ordinal: 42,
        rowid: -7,
    };
    assert_eq!(
        HermesFrontier::decode(&frontier.encode()).unwrap(),
        frontier
    );
    assert_eq!(HERMES_FRONTIER_VERSION, 1);
    assert_eq!(HERMES_LOCATOR_KIND, "hermes-sqlite-row-v1");
    assert!(HermesFrontier::decode(&frontier.encode()[..16]).is_err());
}

#[test]
fn minimum_sqlite_rowid_is_distinct_from_the_initial_frontier() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "minimum-rowid-session", 0);
    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE sessions SET rowid = ?1 WHERE id = 'minimum-rowid-session'",
            [i64::MIN],
        )
        .unwrap();
    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO messages
             (id, session_id, role, content, timestamp)
             VALUES (?1, 'minimum-rowid-session', 'assistant', 'minimum rowid message', 1782259199.0)",
            [i64::MIN],
        )
        .unwrap();

    let conn = crate::provider::sqlite::open_provider_sqlite_readonly(&path).unwrap();
    let schema = HermesSchema::detect(&conn).unwrap();
    let mut reader = HermesRowReader::new(&conn, &schema).unwrap();
    let mut frontier = HermesFrontier::initial();
    let mut session_rowids = Vec::new();
    let mut message_rowids = Vec::new();
    let mut reached_eof = false;
    for _ in 0..16 {
        let Some(row) = reader.next(frontier).unwrap() else {
            reached_eof = true;
            break;
        };
        if row.locator.phase == HermesPhase::Sessions {
            session_rowids.push(row.locator.rowid);
        } else {
            message_rowids.push(row.locator.rowid);
        }
        frontier = row.next_frontier;
    }
    assert!(
        reached_eof,
        "minimum rowid must not restart the session scan"
    );
    assert_eq!(session_rowids, vec![i64::MIN]);
    assert_eq!(
        message_rowids
            .iter()
            .filter(|rowid| **rowid == i64::MIN)
            .count(),
        1
    );
    drop(reader);
    drop(conn);

    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let first = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    let replay = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(replay.imported, 0);
    assert_eq!(core_cursor_value(&path, &store)["terminal"], json!(true));
}

#[test]
fn row_reader_scans_sessions_then_messages_and_rejects_before_hydration() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "reader-session", 0);
    let oversized = i64::try_from(crate::MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .unwrap()
        .saturating_add(1);
    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO messages (session_id, role, content, timestamp)
             VALUES ('reader-session', 'assistant', zeroblob(?1), 1782259400.0)",
            [oversized],
        )
        .unwrap();

    let conn = crate::provider::sqlite::open_provider_sqlite_readonly(&path).unwrap();
    let schema = HermesSchema::detect(&conn).unwrap();
    let mut reader = HermesRowReader::new(&conn, &schema).unwrap();
    let mut frontier = HermesFrontier::initial();
    let mut phases = Vec::new();
    let mut rejected = 0;
    while let Some(row) = reader.next(frontier).unwrap() {
        phases.push(row.locator.phase);
        rejected += usize::from(matches!(row.record, HermesNativeRecord::Rejected(_)));
        frontier = row.next_frontier;
    }
    assert_eq!(phases.first(), Some(&HermesPhase::Sessions));
    assert!(phases[1..]
        .iter()
        .all(|phase| *phase == HermesPhase::Messages));
    assert_eq!(rejected, 1);
    assert_eq!(reader.session_hydration_queries, 1);
}

#[test]
fn malformed_session_and_dependent_message_are_local_rejections() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "valid-session", 0);
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "INSERT INTO sessions (id, source, started_at)
         VALUES ('malformed-session', 'acp', ?1)",
        [1.0e300_f64],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions (id, source, started_at)
         VALUES ('valid-sibling', 'acp', 1782259400.0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES ('malformed-session', 'assistant', 'dependent rejected body', 1782259401.0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES ('valid-sibling', 'assistant', 'valid sibling body', 1782259402.0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES ('orphan-session', 'assistant', 'orphan rejected body', 1782259403.0)",
        [],
    )
    .unwrap();
    drop(conn);
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();

    let summary = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(summary.failed, 3, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 3);
    assert!(summary
        .failures
        .iter()
        .any(|failure| failure.error.contains("started_at is outside")));
    assert!(summary.failures.iter().any(|failure| failure
        .error
        .contains("depends on malformed or missing session")));
    assert_eq!(
        summary
            .failures
            .iter()
            .filter(|failure| failure
                .error
                .contains("depends on malformed or missing session"))
            .count(),
        2
    );
    assert_eq!(
        store
            .search_event_hits("valid sibling body", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .list_sessions()
        .unwrap()
        .iter()
        .all(|session| session.external_session_id.as_deref() != Some("malformed-session")));
    assert_eq!(core_cursor_value(&path, &store)["terminal"], json!(true));
}

#[test]
fn structural_schema_errors_remain_fatal() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                started_at REAL NOT NULL
            );",
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();

    let error = import(&path, &mut store, ImportProfile::CoreOnly).unwrap_err();
    assert!(error
        .to_string()
        .contains("missing required messages table"));
}

#[test]
fn empty_source_publishes_terminal_core_and_pro_transitions() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_empty_fixture(&path);
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let sink = Arc::new(RecordingSink::for_store(&store));

    let summary = import(&path, &mut store, ImportProfile::CoreAndPro(sink.clone())).unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported, 0);
    assert_eq!(core_cursor_value(&path, &store)["terminal"], json!(true));
    assert_eq!(
        sink.progress
            .lock()
            .unwrap()
            .as_ref()
            .map(|progress| progress.terminal),
        Some(true)
    );
    assert_eq!(sink.behind.load(Ordering::SeqCst), 0);

    let replay = import(
        &path,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(replay.imported, 0);
    assert_eq!(sink.behind.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_zero_observation_terminal_page_retries_once() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_empty_fixture(&path);
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let sink = Arc::new(RecordingSink::for_store(&store));
    sink.fail_once.store(true, Ordering::SeqCst);

    let partial = import(&path, &mut store, ImportProfile::CoreAndPro(sink.clone())).unwrap();
    assert_eq!(partial.failed, 1, "{:?}", partial.failures);
    assert_eq!(core_cursor_value(&path, &store)["terminal"], json!(true));
    assert!(sink.progress.lock().unwrap().is_none());

    let replay = import(
        &path,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(
        sink.progress
            .lock()
            .unwrap()
            .as_ref()
            .map(|progress| progress.terminal),
        Some(true)
    );
    assert!(sink.outputs.lock().unwrap().is_empty());
}

#[test]
fn exact_64_rows_publish_terminal_after_bounded_restart_and_pro_replay() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    // One session plus the three base messages plus sixty extra messages.
    create_fixture(&path, "exact-page-session", 60);
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let sink = Arc::new(RecordingSink::for_store(&store));

    let first = import_with_limit(
        &path,
        &mut store,
        ImportProfile::CoreAndPro(sink.clone()),
        crate::CaptureWorkLimit::OneSafeGroup,
    )
    .unwrap();
    assert!(first.work_remaining);
    assert_eq!(core_cursor_value(&path, &store)["terminal"], json!(false));
    assert_eq!(
        sink.progress
            .lock()
            .unwrap()
            .as_ref()
            .map(|progress| progress.terminal),
        Some(false)
    );

    let terminal = import_with_limit(
        &path,
        &mut store,
        ImportProfile::CoreAndPro(sink.clone()),
        crate::CaptureWorkLimit::OneSafeGroup,
    )
    .unwrap();
    assert!(!terminal.work_remaining);
    assert_eq!(core_cursor_value(&path, &store)["terminal"], json!(true));
    assert_eq!(
        sink.progress
            .lock()
            .unwrap()
            .as_ref()
            .map(|progress| progress.terminal),
        Some(true)
    );
    assert_eq!(sink.behind.load(Ordering::SeqCst), 0);

    let restart = import(
        &path,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(restart.imported, 0);
    assert_eq!(sink.behind.load(Ordering::SeqCst), 0);
}

#[test]
fn nativepath_core_is_bounded_idempotent_and_excludes_success_outputs() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "bounded-session", 70);
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();

    let first = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 72);
    let sessions = store.list_sessions().unwrap();
    let events = store.events_for_session(sessions[0].id).unwrap();
    let encoded = serde_json::to_string(&events).unwrap();
    assert!(!encoded.contains("successful private bytes"));
    assert!(!encoded.contains("failed diagnostic bytes"));
    assert!(!encoded.contains("output_preview"));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert!(events
        .iter()
        .filter(|event| event.event_type == EventType::ToolOutput)
        .all(|event| {
            event
                .sync
                .metadata
                .get(crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
                .is_none()
        }));
    assert_eq!(
        store.search_event_hits("hermesneedle69", 10).unwrap().len(),
        1
    );

    let replay = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(replay.imported, 0);
    assert_eq!(replay.skipped, 0);
}

#[test]
fn source_mutation_after_core_writes_rolls_back_rows_cursor_and_receipt() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "publication-fence-session", 0);
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    store.activate_projection_journal(&"a".repeat(64)).unwrap();
    let baseline = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(baseline.failed, 0, "{:?}", baseline.failures);

    let before_sources = store.list_capture_sources().unwrap();
    let before_sessions = store.list_sessions().unwrap();
    let before_events = stored_events(&store);
    let before_cursor = core_cursor(&path, &store);
    let before_receipt = store.projection_journal_snapshot(None).unwrap();

    let provider_writer = Connection::open(&path).unwrap();
    provider_writer
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    provider_writer
        .pragma_update(None, "wal_autocheckpoint", 0_i64)
        .unwrap();
    provider_writer
        .execute(
            "INSERT INTO messages
             (session_id, role, content, timestamp, finish_reason)
             VALUES ('publication-fence-session', 'assistant',
                     'candidate must roll back', 1782259500.0, 'stop')",
            [],
        )
        .unwrap();

    let hook_path = path.clone();
    let _hook =
        super::native_path::install_before_cursor_publication_revalidation_hook(move || {
            let hostile_writer = Connection::open(hook_path).unwrap();
            hostile_writer
                .pragma_update(None, "wal_autocheckpoint", 0_i64)
                .unwrap();
            hostile_writer
                .execute(
                    "INSERT INTO messages
                     (session_id, role, content, timestamp, finish_reason)
                     VALUES ('publication-fence-session', 'assistant',
                             'hostile post-snapshot mutation', 1782259501.0, 'stop')",
                    [],
                )
                .unwrap();
        });

    let error = import(&path, &mut store, ImportProfile::CoreOnly).unwrap_err();

    assert!(matches!(error, CaptureError::SourceChangedDuringCapture));
    assert_eq!(store.list_capture_sources().unwrap(), before_sources);
    assert_eq!(store.list_sessions().unwrap(), before_sessions);
    assert_eq!(stored_events(&store), before_events);
    assert!(store
        .search_event_hits("candidate must roll back", 10)
        .unwrap()
        .is_empty());
    assert_eq!(core_cursor(&path, &store), before_cursor);
    assert_eq!(
        store.projection_journal_snapshot(None).unwrap(),
        before_receipt
    );
    drop(provider_writer);
}

#[test]
fn released_message_hash_migrates_exactly_to_normalized_authority() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "released-hash-session", 0);
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    let session = store.list_sessions().unwrap().remove(0);
    let mut released = store
        .events_for_session(session.id)
        .unwrap()
        .into_iter()
        .find(|event| event.sync.metadata["provider_event_index"] == json!(1))
        .expect("ordinary Hermes message event");
    let released_id = released.id;
    let released_seq = released.seq;
    let exact_released_hash = "message:1";
    released.dedupe_key = Some(
        Store::provider_event_dedupe_key_with_payload_hash(
            released.dedupe_key.as_deref().unwrap(),
            exact_released_hash,
        )
        .unwrap(),
    );
    released.sync.metadata["provider_event_hash"] = json!(exact_released_hash);
    released.sync.metadata["provider_event_hash_authority"] = json!("provider_supplied");
    released.payload["provider_event_hash"] = json!(exact_released_hash);
    Connection::open(store.path())
        .unwrap()
        .execute(
            "UPDATE events
             SET payload_json = ?1, dedupe_key = ?2, metadata_json = ?3
             WHERE id = ?4",
            rusqlite::params![
                serde_json::to_string(&released.payload).unwrap(),
                released.dedupe_key.as_deref(),
                serde_json::to_string(&released.sync.metadata).unwrap(),
                released.id.to_string(),
            ],
        )
        .unwrap();

    let canonical = std::fs::canonicalize(&path).unwrap();
    let locator_identity = crate::provider::importer::provider_path_identity(&canonical).unwrap();
    let stream = crate::provider::importer::provider_source_cursor_stream_for_path(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let released_cursor = crate::provider::importer::CertifiedProviderCursor::new(
        "released-hermes-source-revision",
        1,
        5,
        crate::native_source::NativePosition::new("hermes-sqlite-keyset-v1", vec![0]).unwrap(),
        crate::provider::importer::BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    let mut stored = store
        .get_sync_cursor(None, MACHINE, &stream)
        .unwrap()
        .unwrap();
    stored.cursor = released_cursor;
    store.upsert_sync_cursor(&stored).unwrap();

    let migrated = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(migrated.failed, 0, "{:?}", migrated.failures);
    let event = store.get_event(released_id).unwrap();
    let normalized_hash = event.sync.metadata["provider_event_hash"].as_str().unwrap();
    assert_eq!(event.seq, released_seq);
    assert_ne!(normalized_hash, exact_released_hash);
    assert!(event
        .dedupe_key
        .as_deref()
        .unwrap()
        .ends_with(normalized_hash));
    assert_eq!(
        event.sync.metadata["provider_event_hash_authority"],
        json!("normalized_payload_fallback")
    );
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2);
    assert_eq!(core_cursor_value(&path, &store)["terminal"], json!(true));
}

#[test]
fn indivisible_over_8_mib_tool_row_is_rejected_without_pro_retry() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "oversized-output-session", 0);
    let oversized =
        "x".repeat(crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES + 1);
    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO messages
             (session_id, role, content, tool_call_id, tool_name, timestamp, finish_reason)
             VALUES ('oversized-output-session', 'tool', ?1, 'oversized-call', 'shell',
                     1782259500.0, 'success')",
            [oversized],
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let sink = Arc::new(RecordingSink::for_store(&store));

    let first = import(&path, &mut store, ImportProfile::CoreAndPro(sink.clone())).unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert!(first.failures[0].error.contains("indivisible"));
    assert_eq!(sink.behind.load(Ordering::SeqCst), 0);
    assert_eq!(sink.outputs.lock().unwrap().len(), 2);
    assert_eq!(core_cursor_value(&path, &store)["terminal"], json!(true));

    let replay = import(
        &path,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(sink.behind.load(Ordering::SeqCst), 0);
    assert_eq!(sink.outputs.lock().unwrap().len(), 2);
}

#[test]
fn large_ordinary_message_compacts_into_core_with_message_locator() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "large-message-session", 0);
    let large = format!(
        "largeordinarymarker {}",
        "x".repeat(crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES + 1)
    );
    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO messages
             (session_id, role, content, timestamp, finish_reason)
             VALUES ('large-message-session', 'assistant', ?1, 1782259500.0, 'stop')",
            [large],
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();

    let summary = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 3);
    let session = store.list_sessions().unwrap().remove(0);
    let event = store
        .events_for_session(session.id)
        .unwrap()
        .into_iter()
        .find(|event| event.sync.metadata["provider_event_index"] == json!(4))
        .expect("large ordinary message");
    let locators = crate::complete_content::VerifiedContentLocatorsV1::from_metadata_value(
        &event.sync.metadata[crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .expect("large ordinary message locator");
    assert!(locators
        .locator(crate::complete_content::VerifiedContentRole::MessageBody)
        .is_some());
    assert_eq!(
        store
            .search_event_hits("largeordinarymarker", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn pro_progress_failure_marks_only_output_behind_and_core_continues() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "observe-failure-session", 0);
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let sink = Arc::new(RecordingSink::for_store(&store));
    sink.fail_observe.store(true, Ordering::SeqCst);

    let partial = import(&path, &mut store, ImportProfile::CoreAndPro(sink.clone())).unwrap();
    assert_eq!(partial.imported_events, 2);
    assert_eq!(partial.failed, 1);
    assert_eq!(
        partial.failures[0].error,
        "Hermes Pro output is behind committed Core"
    );
    assert_eq!(sink.behind.load(Ordering::SeqCst), 1);
    assert_eq!(core_cursor_value(&path, &store)["terminal"], json!(true));

    let replay = import(
        &path,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(sink.outputs.lock().unwrap().len(), 2);

    sink.fail_observe.store(true, Ordering::SeqCst);
    let unchanged = import(&path, &mut store, ImportProfile::CoreAndPro(sink.clone())).unwrap();
    assert_eq!(unchanged.imported, 0);
    assert_eq!(unchanged.failed, 1);
    assert_eq!(sink.behind.load(Ordering::SeqCst), 2);
}

#[test]
fn pro_output_failure_does_not_fail_unchanged_core_import() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "output-session", 0);
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let initial = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(initial.imported_events, 2);
    let core_events_before = store
        .events_for_session(store.list_sessions().unwrap()[0].id)
        .unwrap()
        .len();
    let sink = Arc::new(RecordingSink::for_store(&store));
    sink.fail_once.store(true, Ordering::SeqCst);

    let partial = import(&path, &mut store, ImportProfile::CoreAndPro(sink.clone())).unwrap();
    assert_eq!(partial.imported, 0);
    assert_eq!(partial.failed, 1);
    assert_eq!(
        partial.failures[0].error,
        "Hermes Pro output is behind committed Core"
    );
    assert_eq!(sink.behind.load(Ordering::SeqCst), 1);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(
        store
            .events_for_session(store.list_sessions().unwrap()[0].id)
            .unwrap()
            .len(),
        core_events_before
    );

    let replay = import(
        &path,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(replay.imported, 0);
    assert!(sink.saw_core_before_output.load(Ordering::SeqCst));
    assert_eq!(
        *sink.outputs.lock().unwrap(),
        vec![
            (OutputOutcome::Success, b"successful private bytes".to_vec()),
            (OutputOutcome::Failure, b"failed diagnostic bytes".to_vec()),
        ]
    );

    let no_op = import(
        &path,
        &mut store,
        ImportProfile::ProReplayOnly(sink.clone()),
    )
    .unwrap();
    assert_eq!(no_op.imported, 0);
    assert_eq!(sink.outputs.lock().unwrap().len(), 2);
}

#[test]
fn append_rewrite_replacement_and_disappearance_reset_authority_safely() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "lifecycle-a", 0);
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    assert_eq!(
        import(&path, &mut store, ImportProfile::CoreOnly)
            .unwrap()
            .imported_events,
        2
    );

    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO messages
             (session_id, role, content, timestamp, finish_reason)
             VALUES ('lifecycle-a', 'assistant', 'appended body', 1782259500.0, 'stop')",
            [],
        )
        .unwrap();
    let append = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(append.imported_events, 1);

    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE messages SET content = 'rewritten body' WHERE id = 1",
            [],
        )
        .unwrap();
    let rewrite = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(rewrite.failed, 0, "{:?}", rewrite.failures);
    let rewritten_events = store
        .events_for_session(store.list_sessions().unwrap()[0].id)
        .unwrap();
    assert!(serde_json::to_string(&rewritten_events)
        .unwrap()
        .contains("rewritten body"));

    let replacement = temp.path().join("replacement.db");
    create_fixture(&replacement, "lifecycle-b", 0);
    std::fs::rename(&replacement, &path).unwrap();
    let replaced = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(replaced.failed, 0, "{:?}", replaced.failures);
    assert!(store
        .list_sessions()
        .unwrap()
        .iter()
        .any(|session| { session.external_session_id.as_deref() == Some("lifecycle-b") }));

    let event_id = store
        .list_sessions()
        .unwrap()
        .iter()
        .find(|session| session.external_session_id.as_deref() == Some("lifecycle-b"))
        .and_then(|session| store.events_for_session(session.id).ok())
        .and_then(|events| events.first().map(|event| event.id))
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(import(&path, &mut store, ImportProfile::CoreOnly).is_err());
    assert!(store.authorized_source_route_for_event(event_id).is_err());
    assert_eq!(store.list_sessions().unwrap().len(), 2);
}

#[test]
fn result_content_uses_only_the_tool_content_column_without_a_size_cap() {
    let long = "x".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 19);
    assert_eq!(
        hermes_normalized_result_content("tool", &Value::String(long.clone())),
        Some(long)
    );
    assert_eq!(
        hermes_normalized_result_content("assistant", &Value::String("not a result".into())),
        None
    );
    assert_eq!(hermes_normalized_result_content("tool", &Value::Null), None);
}
