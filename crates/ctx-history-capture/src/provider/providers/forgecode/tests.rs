use std::{
    collections::HashMap,
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderAdapterContext,
    ProviderImportOptions,
};

use super::{
    import_forgecode_nativepath,
    nativepath::source::{
        discover_forgecode_source, ForgeCodeDiscovery, ForgeCodeFrontier, ForgeCodeScanner,
    },
};

const SUCCESS_SENTINEL: &str = "forgecode-success-body-must-stay-out-of-core";

#[test]
fn scanner_pages_messages_and_separates_success_output() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let mut messages = (0..18)
        .map(|index| {
            json!({
                "message": {
                    "text": {
                        "role": "user",
                        "content": format!("message-{index}")
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    messages.push(success_output(SUCCESS_SENTINEL));
    write_source(&source_path, "conversation-pages", Value::Array(messages));

    let source = live_source(&source_path);
    let mut scanner = ForgeCodeScanner::new(
        source,
        ForgeCodeFrontier::initial(),
        context(&source_path),
        true,
    )
    .unwrap();
    let mut pages = Vec::new();
    while let Some(page) = scanner.next_page().unwrap() {
        pages.push(page);
    }

    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].events.len(), 16);
    assert_eq!(pages[1].events.len(), 2);
    assert_eq!(pages[1].outputs.len(), 1);
    assert_eq!(pages[1].outputs[0].content, SUCCESS_SENTINEL.as_bytes());
    assert!(pages
        .iter()
        .flat_map(|page| &page.events)
        .all(|event| !event.event.payload.to_string().contains(SUCCESS_SENTINEL)));
    assert!(pages
        .iter()
        .filter_map(|page| page.row.as_ref())
        .all(|row| {
            row.context_metadata
                .as_object()
                .is_none_or(|metadata| !metadata.contains_key("messages"))
        }));
    assert!(pages.last().unwrap().terminal);
}

#[test]
fn malformed_row_is_bounded_and_does_not_hide_healthy_sibling() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let conn = Connection::open(&source_path).unwrap();
    create_schema(&conn);
    insert_row(&conn, "broken", Some("{not-json"), Some("[not-json"));
    insert_row(
        &conn,
        "healthy",
        Some(
            &json!({
                "messages": [{
                    "message": {"text": {"role": "assistant", "content": "healthy"}}
                }]
            })
            .to_string(),
        ),
        None,
    );
    drop(conn);

    let mut scanner = ForgeCodeScanner::new(
        live_source(&source_path),
        ForgeCodeFrontier::initial(),
        context(&source_path),
        false,
    )
    .unwrap();
    let first = scanner.next_page().unwrap().unwrap();
    let second = scanner.next_page().unwrap().unwrap();

    assert_eq!(first.rejections.len(), 2);
    assert!(first
        .rejections
        .iter()
        .all(|failure| failure.error.len() <= 4 * 1024));
    assert!(first.events.is_empty());
    assert_eq!(second.events.len(), 1);
    assert!(second.terminal);
}

#[test]
fn core_replay_append_rewrite_and_disappearance_are_idempotent() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_root = directory.path().join("forge-root");
    fs::create_dir(&source_root).unwrap();
    let source_path = source_root.join(".forge.db");
    write_source(
        &source_path,
        "conversation-mutations",
        json!([text_message("one")]),
    );
    let store_path = directory.path().join("ctx.sqlite");
    let mut store = Store::open(&store_path).unwrap();

    let first = import_core(&source_root, &mut store).unwrap();
    assert_eq!(first.imported_events, 1);
    assert_eq!(event_count(&store), 1);

    let replay = import_core(&source_root, &mut store).unwrap();
    assert_eq!(replay.imported_events, 0);
    assert_eq!(event_count(&store), 1);

    replace_messages(
        &source_path,
        json!([text_message("one"), text_message("two")]),
    );
    import_core(&source_root, &mut store).unwrap();
    assert_eq!(event_count(&store), 2);

    replace_messages(
        &source_path,
        json!([text_message("one-rewritten"), text_message("two")]),
    );
    import_core(&source_root, &mut store).unwrap();
    assert_eq!(event_count(&store), 4);

    fs::remove_file(&source_path).unwrap();
    fs::remove_dir(&source_root).unwrap();
    let retirement = import_core(&source_root, &mut store).unwrap();
    assert_eq!(
        retirement.work_result(),
        crate::ProviderImportWorkResult::Changed
    );
    let replay = import_core(&source_root, &mut store).unwrap();
    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
}

#[test]
fn output_failure_does_not_stop_later_core_pages() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let mut messages = vec![success_output(SUCCESS_SENTINEL)];
    messages.extend((0..20).map(|index| text_message(&format!("core-{index}"))));
    write_source(
        &source_path,
        "conversation-pro-failure",
        Value::Array(messages),
    );
    let store_path = directory.path().join("ctx.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let mut options = ProviderImportOptions::default();
    options.import_profile = ImportProfile::CoreAndPro(Arc::new(FailingSink));

    let result =
        import_forgecode_nativepath(&source_path, &mut store, context(&source_path), options);

    assert!(result.is_err());
    assert_eq!(event_count(&store), 20);
    assert!(store
        .list_sessions()
        .unwrap()
        .into_iter()
        .flat_map(|session| store.events_for_session(session.id).unwrap())
        .all(|event| !event.payload.to_string().contains(SUCCESS_SENTINEL)));
}

#[test]
fn pro_can_activate_after_core_and_replay_success_body() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    write_source(
        &source_path,
        "conversation-later-pro",
        json!([text_message("core"), success_output(SUCCESS_SENTINEL)]),
    );
    let store_path = directory.path().join("ctx.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    import_core(&source_path, &mut store).unwrap();
    assert_eq!(event_count(&store), 1);

    let sink = Arc::new(RecordingSink::default());
    let mut options = ProviderImportOptions::default();
    options.import_profile = ImportProfile::ProReplayOnly(sink.clone());
    let replay =
        import_forgecode_nativepath(&source_path, &mut store, context(&source_path), options)
            .unwrap();

    assert_eq!(replay.work_result(), crate::ProviderImportWorkResult::NoOp);
    assert_eq!(
        sink.contents.lock().unwrap().as_slice(),
        [SUCCESS_SENTINEL.as_bytes()]
    );
    assert_eq!(event_count(&store), 1);
}

#[test]
fn truncated_message_keeps_the_existing_verified_source_locator() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join(".forge.db");
    let body = "x".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 1);
    write_source(
        &source_path,
        "conversation-complete-content",
        json!([text_message(&body)]),
    );
    let mut store = Store::open(directory.path().join("ctx.sqlite")).unwrap();
    import_core(&source_path, &mut store).unwrap();

    let session = store.list_sessions().unwrap().pop().unwrap();
    let event = store.events_for_session(session.id).unwrap().pop().unwrap();
    assert!(event
        .sync
        .metadata
        .get(crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_some());
}

#[test]
fn missing_root_resolves_to_the_canonical_database_locator() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let missing_root = directory.path().join("missing-forge-root");
    match discover_forgecode_source(&missing_root).unwrap() {
        ForgeCodeDiscovery::Missing(path) => {
            assert_eq!(path, missing_root.join(".forge.db"));
        }
        ForgeCodeDiscovery::Live(_) => panic!("missing root was discovered as live"),
    }
}

struct FailingSink;

impl ProOutputSink for FailingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "forgecode-failing-test-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(None)
    }

    fn materialize_page(
        &self,
        _page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        Err(ProOutputSinkError::new("test_failure", "expected"))
    }
}

#[derive(Default)]
struct RecordingSink {
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "forgecode-recording-test-v1"
    }

    fn observe_source(
        &self,
        source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().unwrap().get(source).cloned())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        self.contents.lock().unwrap().extend(
            page.observations
                .iter()
                .map(|observation| observation.content.clone()),
        );
        let committed_cursor = page.next_safe_cursor.clone();
        self.progress.lock().unwrap().insert(
            page.source.clone(),
            ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(committed_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
            },
        );
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
            materialized_facts: 0,
            replayed: false,
        })
    }
}

fn import_core(path: &Path, store: &mut Store) -> crate::Result<crate::ProviderImportSummary> {
    import_forgecode_nativepath(path, store, context(path), ProviderImportOptions::default())
}

fn context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "forgecode-nativepath-test".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: path.parent().map(Path::to_path_buf),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    }
}

fn live_source(path: &Path) -> super::nativepath::source::ForgeCodeSourceObservation {
    match discover_forgecode_source(path).unwrap() {
        ForgeCodeDiscovery::Live(source) => source,
        ForgeCodeDiscovery::Missing(_) => panic!("fixture source is missing"),
    }
}

fn event_count(store: &Store) -> usize {
    store
        .list_sessions()
        .unwrap()
        .into_iter()
        .map(|session| store.events_for_session(session.id).unwrap().len())
        .sum()
}

fn write_source(path: &Path, conversation_id: &str, messages: Value) {
    let conn = Connection::open(path).unwrap();
    create_schema(&conn);
    insert_row(
        &conn,
        conversation_id,
        Some(&json!({"initiator": "forge", "messages": messages}).to_string()),
        Some(&json!({"files_accessed": ["Cargo.toml"]}).to_string()),
    );
}

fn create_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE conversations (
            conversation_id TEXT NOT NULL,
            title TEXT,
            workspace_id INTEGER NOT NULL,
            context TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT,
            metrics TEXT
        );",
    )
    .unwrap();
}

fn insert_row(
    conn: &Connection,
    conversation_id: &str,
    context: Option<&str>,
    metrics: Option<&str>,
) {
    conn.execute(
        "INSERT INTO conversations
         (conversation_id, title, workspace_id, context, created_at, updated_at, metrics)
         VALUES (?1, 'test', 7, ?2, '2026-01-01T00:00:00Z',
                 '2026-01-01T00:00:01Z', ?3)",
        rusqlite::params![conversation_id, context, metrics],
    )
    .unwrap();
}

fn replace_messages(path: &Path, messages: Value) {
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "UPDATE conversations SET context = ?1, updated_at = ?2",
        rusqlite::params![
            json!({"initiator": "forge", "messages": messages}).to_string(),
            "2026-01-01T00:00:02Z",
        ],
    )
    .unwrap();
}

fn text_message(text: &str) -> Value {
    json!({"message": {"text": {"role": "user", "content": text}}})
}

fn success_output(text: &str) -> Value {
    json!({
        "message": {
            "tool": {
                "name": "shell",
                "call_id": "call-success",
                "output": {
                    "is_error": false,
                    "values": [{"text": text}]
                }
            }
        }
    })
}
