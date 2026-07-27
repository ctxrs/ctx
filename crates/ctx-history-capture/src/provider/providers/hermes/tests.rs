use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_store::Store;
use rusqlite::Connection;

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
    progress: Mutex<Option<ProOutputProgress>>,
    outputs: Mutex<Vec<(OutputOutcome, Vec<u8>)>>,
    saw_core_before_output: AtomicBool,
    store_path: Mutex<Option<PathBuf>>,
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
    import_hermes_nativepath(path, store, context(path), options(import_profile))
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
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolOutput));
    assert_eq!(
        store.search_event_hits("hermesneedle69", 10).unwrap().len(),
        1
    );

    let replay = import(&path, &mut store, ImportProfile::CoreOnly).unwrap();
    assert_eq!(replay.imported, 0);
    assert_eq!(replay.skipped, 0);
}

#[test]
fn output_replay_is_independent_and_core_commits_before_sink_failure() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("state.db");
    create_fixture(&path, "output-session", 0);
    let mut store = Store::open(temp.path().join("ctx.sqlite")).unwrap();
    let sink = Arc::new(RecordingSink::for_store(&store));
    sink.fail_once.store(true, Ordering::SeqCst);

    let failed = import(&path, &mut store, ImportProfile::CoreAndPro(sink.clone()));
    assert!(failed.is_err());
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(
        store
            .events_for_session(store.list_sessions().unwrap()[0].id)
            .unwrap()
            .len(),
        2
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
