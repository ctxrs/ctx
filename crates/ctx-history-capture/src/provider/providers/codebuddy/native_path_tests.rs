use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use ctx_history_core::{CaptureProvider, EntityTimestamps, SyncCursor};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    native_source::NativePosition,
    provider::importer::{
        provider_event_import_identity, provider_path_identity,
        provider_source_cursor_stream_for_path, BoundedParserCheckpoint, CertifiedProviderCursor,
    },
    test_support_paths::tempdir,
    ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderAdapterContext,
    ProviderImportOptions, ProviderImportWorkResult,
};

use super::*;

fn context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "codebuddy-nativepath-test-machine".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: None,
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
    }
}

fn options(profile: ImportProfile) -> ProviderImportOptions {
    ProviderImportOptions {
        import_profile: profile,
        ..ProviderImportOptions::default()
    }
}

fn cli_line(id: &str, role: &str, kind: &str, content: &str) -> String {
    serde_json::to_string(&json!({
        "id": id,
        "type": kind,
        "role": role,
        "content": content,
        "timestamp": "2026-07-25T10:00:00Z",
        "sessionId": "cli-session",
        "cwd": "/workspace/codebuddy",
    }))
    .unwrap()
}

fn write_cli_root(root: &Path, lines: &[String]) -> PathBuf {
    let path = root.join("projects/project-hash/cli-session.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut body = lines.join("\n");
    body.push('\n');
    fs::write(&path, body).unwrap();
    path
}

fn write_extension_root(root: &Path, records: &[(&str, &[u8])]) -> PathBuf {
    let project = root.join("history/project-hash");
    let session = project.join("extension-session");
    fs::create_dir_all(session.join("messages")).unwrap();
    let messages = records
        .iter()
        .map(|(id, _)| json!({ "id": id, "role": "user", "type": "message" }))
        .collect::<Vec<_>>();
    fs::write(
        session.join("index.json"),
        serde_json::to_vec(&json!({ "messages": messages })).unwrap(),
    )
    .unwrap();
    fs::write(
        project.join("index.json"),
        serde_json::to_vec(&json!({
            "conversations": [{
                "id": "extension-session",
                "name": "Extension session",
                "createdAt": "2026-07-25T10:00:00Z",
                "updatedAt": "2026-07-25T11:00:00Z",
                "projectPath": "/workspace/codebuddy-extension",
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    for (id, body) in records {
        fs::write(session.join(format!("messages/{id}.json")), body).unwrap();
    }
    session
}

fn codebuddy_events(store: &Store, external_session_id: &str) -> Vec<Event> {
    let session = store
        .session_by_external_session(CaptureProvider::CodeBuddy, external_session_id)
        .unwrap()
        .unwrap();
    store.events_for_session(session.id).unwrap()
}

fn native_message_id(event: &Event) -> &str {
    event
        .sync
        .metadata
        .pointer("/metadata/native_message_id")
        .and_then(Value::as_str)
        .unwrap()
}

#[test]
fn cli_nativepath_bounds_restarts_and_heals_an_incomplete_tail() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".codebuddy");
    let lines = (0..65)
        .map(|index| {
            cli_line(
                &format!("message-{index}"),
                if index % 2 == 0 { "user" } else { "assistant" },
                "message",
                &format!("bounded message {index}"),
            )
        })
        .collect::<Vec<_>>();
    let path = write_cli_root(&root, &lines);
    let mut store = Store::open(temp.path().join("bounded.sqlite")).unwrap();
    let first = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions {
            capture_work_limit: CaptureWorkLimit::OneSafeGroup,
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.imported_events, 64);
    assert!(first.work_remaining);

    let resumed = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(resumed.imported_events, 1);
    assert!(!resumed.work_remaining);

    let replay = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.skipped_events, 65);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        &provider_path_identity(&fs::canonicalize(&path).unwrap()).unwrap(),
    );
    let cursor = store
        .get_sync_cursor(None, "codebuddy-nativepath-test-machine", &stream)
        .unwrap()
        .unwrap();
    assert!(!cursor.cursor.contains("bounded message 0"));

    fs::write(
        &path,
        format!(
            "{}\n{{\"id\":\"healed\"",
            cli_line("stable", "user", "message", "stable before incomplete")
        ),
    )
    .unwrap();
    let incomplete = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(incomplete.imported_events, 1);
    assert_eq!(incomplete.failed, 1);
    assert!(incomplete.failures[0].error.contains("incomplete trailing"));

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        file,
        ",\"type\":\"message\",\"role\":\"assistant\",\"content\":\"healed tail\",\"timestamp\":\"2026-07-25T10:01:00Z\",\"sessionId\":\"cli-session\"}}"
    )
    .unwrap();
    drop(file);
    let healed = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(healed.failed, 0, "{:?}", healed.failures);
    assert_eq!(healed.imported_events, 1);

    let replacement = path.with_extension("replacement");
    fs::write(
        &replacement,
        format!(
            "{}\n",
            cli_line("replacement", "user", "message", "replacement generation")
        ),
    )
    .unwrap();
    fs::rename(&replacement, &path).unwrap();
    let replaced = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replaced.failed, 0, "{:?}", replaced.failures);
    assert_eq!(replaced.imported_events, 1);
}

#[test]
fn cli_rewrite_keeps_native_ids_stable_and_updates_mutated_payloads_without_duplicates() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".codebuddy");
    let path = write_cli_root(
        &root,
        &[
            cli_line("alpha", "user", "message", "alpha before rewrite"),
            cli_line("beta", "assistant", "message", "beta stable"),
        ],
    );
    let mut store = Store::open(temp.path().join("rewrite.sqlite")).unwrap();
    let first = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.imported_events, 2);
    let before = codebuddy_events(&store, "project-hash/cli-session")
        .into_iter()
        .map(|event| (native_message_id(&event).to_owned(), event.id))
        .collect::<HashMap<_, _>>();

    fs::write(
        &path,
        format!(
            "{}\n{}\n",
            cli_line("beta", "assistant", "message", "beta stable"),
            cli_line("alpha", "user", "message", "alpha after rewrite")
        ),
    )
    .unwrap();
    let rewritten = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(rewritten.imported_events, 0);
    assert_eq!(rewritten.skipped_events, 2);
    let after = codebuddy_events(&store, "project-hash/cli-session");
    assert_eq!(after.len(), 2);
    for event in &after {
        assert_eq!(Some(&event.id), before.get(native_message_id(event)));
    }
    let alpha = after
        .iter()
        .find(|event| native_message_id(event) == "alpha")
        .unwrap();
    assert!(alpha.payload.to_string().contains("alpha after rewrite"));
    assert!(!alpha.payload.to_string().contains("alpha before rewrite"));
    assert_eq!(
        alpha.sync.metadata["provider_event_hash_authority"],
        "normalized_payload_fallback"
    );
}

#[test]
fn cli_classifies_messages_metadata_and_rejections_without_searchable_metadata_leakage() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".codebuddy");
    let path = root.join("projects/project-hash/cli-session.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let metadata_text = "textual metadata must never become a message";
    fs::write(
        &path,
        format!(
            "{}\n{}\n{{malformed\n",
            cli_line("metadata", "user", "file-history-snapshot", metadata_text),
            serde_json::to_string(&json!({
                "id": "non-text",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "image", "media_type": "image/png", "data": "bounded"}],
                "timestamp": "2026-07-25T10:00:00Z",
                "sessionId": "cli-session",
                "cwd": "/workspace/codebuddy",
            }))
            .unwrap()
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("classification.sqlite")).unwrap();
    let imported = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(imported.imported_events, 1);
    assert_eq!(imported.failed, 1);
    assert!(imported.skipped >= 1);
    let events = codebuddy_events(&store, "project-hash/cli-session");
    assert_eq!(events.len(), 1);
    assert_eq!(native_message_id(&events[0]), "non-text");
    assert_eq!(events[0].event_type, EventType::Message);
    assert!(!events[0].payload.to_string().contains(metadata_text));
    assert!(events[0].payload.to_string().contains("image"));
    assert!(store
        .search_event_hits(metadata_text, 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        events[0].sync.metadata["source_record_ordinal"].as_u64(),
        Some(1)
    );
    assert_eq!(
        events[0].sync.metadata["source_record_subrecord_index"].as_u64(),
        Some(0)
    );

    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        &provider_path_identity(&fs::canonicalize(&path).unwrap()).unwrap(),
    );
    let stored = store
        .get_sync_cursor(None, "codebuddy-nativepath-test-machine", &stream)
        .unwrap()
        .unwrap();
    let committed = decode_native_path_committed_cursor(&stored.cursor).unwrap();
    let cursor = CodeBuddyNativeCursor::decode(committed.provider_cursor()).unwrap();
    assert_eq!(cursor.accepted_events, 1);
    assert_eq!(cursor.skipped_metadata, 1);
    assert_eq!(cursor.rejected_records, 1);
}

#[test]
fn extension_nativepath_replays_failures_and_accepts_rewrite_and_truncation() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("extension-root");
    let valid = serde_json::to_vec(&json!({
        "id": "one",
        "content": "extension one",
        "createdAt": "2026-07-25T10:00:00Z",
    }))
    .unwrap();
    let session = write_extension_root(&root, &[("one", valid.as_slice()), ("two", b"{malformed")]);
    let project_index_path = session.parent().unwrap().join("index.json");
    let mut project_index: Value =
        serde_json::from_slice(&fs::read(&project_index_path).unwrap()).unwrap();
    project_index["conversations"][0]
        .as_object_mut()
        .unwrap()
        .remove("name");
    fs::write(
        &project_index_path,
        serde_json::to_vec(&project_index).unwrap(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("extension.sqlite")).unwrap();
    let first = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.imported_events, 1);
    assert_eq!(first.failed, 1);
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        &provider_path_identity(&fs::canonicalize(&session).unwrap()).unwrap(),
    );
    let cursor = store
        .get_sync_cursor(None, "codebuddy-nativepath-test-machine", &stream)
        .unwrap()
        .unwrap();
    assert!(!cursor.cursor.contains("extension one"));

    let replay = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.skipped_events, 1);
    assert_eq!(replay.failures, first.failures);

    fs::write(
        session.join("messages/two.json"),
        serde_json::to_vec(&json!({
            "id": "two",
            "content": "extension two after rewrite",
            "createdAt": "2026-07-25T10:01:00Z",
        }))
        .unwrap(),
    )
    .unwrap();
    let rewritten = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(rewritten.failed, 0, "{:?}", rewritten.failures);
    assert_eq!(rewritten.imported_events, 1);
    assert_eq!(rewritten.skipped_events, 1);
    let imported_session = store
        .session_by_external_session(CaptureProvider::CodeBuddy, "project-hash/extension-session")
        .unwrap()
        .unwrap();
    assert_eq!(
        store.events_for_session(imported_session.id).unwrap().len(),
        2
    );

    fs::write(
        session.join("index.json"),
        serde_json::to_vec(&json!({
            "messages": [{ "id": "one", "role": "user", "type": "message" }]
        }))
        .unwrap(),
    )
    .unwrap();
    let truncated = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(truncated.failed, 0, "{:?}", truncated.failures);
    assert_eq!(truncated.work_result(), ProviderImportWorkResult::Changed);
}

#[test]
fn extension_message_io_failure_aborts_without_cursor_or_route_mutation() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("extension-root");
    let message = serde_json::to_vec(&json!({
        "id": "one",
        "content": "stable extension message",
        "createdAt": "2026-07-25T10:00:00Z",
    }))
    .unwrap();
    let session = write_extension_root(&root, &[("one", message.as_slice())]);
    let mut store = Store::open(temp.path().join("unreadable.sqlite")).unwrap();
    import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        &provider_path_identity(&fs::canonicalize(&session).unwrap()).unwrap(),
    );
    let before_cursor = store
        .get_sync_cursor(None, "codebuddy-nativepath-test-machine", &stream)
        .unwrap()
        .unwrap();
    let before_sources = store.list_capture_sources().unwrap();
    let before_events = codebuddy_events(&store, "project-hash/extension-session");

    let message_path = session.join("messages/one.json");
    let retained = session.join("messages/one.retained");
    fs::rename(&message_path, &retained).unwrap();
    fs::create_dir(&message_path).unwrap();
    let error = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("regular file"),
        "unexpected source error: {error}"
    );
    assert_eq!(
        store
            .get_sync_cursor(None, "codebuddy-nativepath-test-machine", &stream)
            .unwrap()
            .unwrap()
            .cursor,
        before_cursor.cursor
    );
    assert_eq!(store.list_capture_sources().unwrap(), before_sources);
    assert_eq!(
        codebuddy_events(&store, "project-hash/extension-session"),
        before_events
    );
}

#[test]
fn extension_keeps_non_text_messages_and_skips_textual_metadata() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("extension-root");
    let metadata_text = "extension metadata must not become searchable";
    let metadata = serde_json::to_vec(&json!({
        "id": "metadata",
        "role": "user",
        "content": metadata_text,
    }))
    .unwrap();
    let non_text = serde_json::to_vec(&json!({
        "id": "non-text",
        "role": "assistant",
        "content": [{"type": "image", "media_type": "image/png", "data": "bounded"}],
    }))
    .unwrap();
    let session = write_extension_root(
        &root,
        &[
            ("metadata", metadata.as_slice()),
            ("non-text", non_text.as_slice()),
        ],
    );
    fs::write(
        session.join("index.json"),
        serde_json::to_vec(&json!({
            "messages": [
                {"id": "metadata", "role": "user", "type": "metadata"},
                {"id": "non-text", "role": "assistant", "type": "message"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("extension-classification.sqlite")).unwrap();
    let imported = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(imported.imported_events, 1);
    assert!(imported.skipped >= 1);
    let events = codebuddy_events(&store, "project-hash/extension-session");
    assert_eq!(events.len(), 1);
    assert_eq!(native_message_id(&events[0]), "non-text");
    assert!(events[0].payload.to_string().contains("image"));
    assert!(!events[0].payload.to_string().contains(metadata_text));
    assert!(store
        .search_event_hits(metadata_text, 10)
        .unwrap()
        .is_empty());
}

#[test]
fn source_and_root_disappearance_retire_routes_idempotently() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".codebuddy");
    let path = write_cli_root(
        &root,
        &[cli_line("one", "user", "message", "source to retire")],
    );
    let second_path = root.join("projects/project-hash/second-session.jsonl");
    fs::write(
        &second_path,
        format!(
            "{}\n",
            cli_line("two", "user", "message", "root to retire")
                .replace("\"cli-session\"", "\"second-session\"")
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("retirement.sqlite")).unwrap();
    import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();

    fs::remove_file(&path).unwrap();
    let removed = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(removed.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(removed.failed, 0, "{:?}", removed.failures);

    fs::remove_dir_all(&root).unwrap();
    let root_missing = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(
        root_missing.work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_eq!(root_missing.failed, 0, "{:?}", root_missing.failures);

    let replay = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
}

#[test]
fn retired_cli_locator_reappears_with_changed_revision_through_normal_reconciliation() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".codebuddy");
    let path = write_cli_root(
        &root,
        &[cli_line(
            "original",
            "user",
            "message",
            "original source revision",
        )],
    );
    let original_revision = discover_sources(&root, &root, &ProviderImportOptions::default())
        .unwrap()
        .sources
        .into_iter()
        .next()
        .unwrap()
        .source_revision;
    let mut store = Store::open(temp.path().join("reappearance.sqlite")).unwrap();
    let first = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.imported_events, 1);

    fs::remove_file(&path).unwrap();
    let retired = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(retired.failed, 0, "{:?}", retired.failures);

    let restored_path = write_cli_root(
        &root,
        &[cli_line(
            "restored",
            "assistant",
            "message",
            "restored source with a deliberately changed revision",
        )],
    );
    assert_eq!(restored_path, path);
    let restored_revision = discover_sources(&root, &root, &ProviderImportOptions::default())
        .unwrap()
        .sources
        .into_iter()
        .next()
        .unwrap()
        .source_revision;
    assert_ne!(restored_revision, original_revision);

    let restored = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(restored.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(restored.imported_events, 1);
    assert_eq!(restored.failed, 0, "{:?}", restored.failures);

    let replay = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.skipped_events, 1);
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
}

struct RecordingSink {
    fail: AtomicBool,
    behind: AtomicUsize,
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    contents: Mutex<Vec<Vec<u8>>>,
}

impl RecordingSink {
    fn failing() -> Self {
        Self {
            fail: AtomicBool::new(true),
            behind: AtomicUsize::new(0),
            progress: Mutex::new(HashMap::new()),
            contents: Mutex::new(Vec::new()),
        }
    }

    fn recording() -> Self {
        Self {
            fail: AtomicBool::new(false),
            behind: AtomicUsize::new(0),
            progress: Mutex::new(HashMap::new()),
            contents: Mutex::new(Vec::new()),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "codebuddy-nativepath-test-materializer-v1"
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
        if self.fail.load(Ordering::SeqCst) {
            return Err(ProOutputSinkError::new("test_failure", "intentional"));
        }
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
        let accepted_outputs = u32::try_from(page.observations.len()).unwrap();
        Ok(ProOutputPageResult {
            source_epoch: page.source_epoch,
            committed_cursor,
            accepted_outputs,
            materialized_facts: accepted_outputs,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn successful_output_is_absent_from_core_and_pro_can_activate_later() {
    const SECRET: &str = "successful-output-secret-only-for-pro";

    let temp = tempdir().unwrap();
    let root = temp.path().join(".codebuddy");
    write_cli_root(
        &root,
        &[
            cli_line("user", "user", "message", "run the tool"),
            cli_line("result", "tool", "tool_result", SECRET),
        ],
    );
    let store_path = temp.path().join("output.sqlite");
    let mut store = Store::open(&store_path).unwrap();
    let failing = Arc::new(RecordingSink::failing());
    let core = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        options(ImportProfile::CoreAndPro(failing.clone())),
    )
    .unwrap();
    assert_eq!(core.imported_events, 1);
    assert!(failing.behind.load(Ordering::SeqCst) > 0);

    let session = store
        .session_by_external_session(CaptureProvider::CodeBuddy, "project-hash/cli-session")
        .unwrap()
        .unwrap();
    let core_json = serde_json::to_string(&store.events_for_session(session.id).unwrap()).unwrap();
    assert!(!core_json.contains(SECRET));

    let recording = Arc::new(RecordingSink::recording());
    let replay = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        options(ImportProfile::ProReplayOnly(recording.clone())),
    )
    .unwrap();
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(
        recording.contents.lock().unwrap().as_slice(),
        [SECRET.as_bytes()]
    );
}

#[test]
fn inventory_revision_is_revalidated_with_the_same_authority() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("extension-root");
    let message = serde_json::to_vec(&json!({
        "id": "one",
        "content": "inventory-authorized extension",
        "createdAt": "2026-07-25T10:00:00Z",
    }))
    .unwrap();
    write_extension_root(&root, &[("one", message.as_slice())]);
    let mut store = Store::open(temp.path().join("inventory.sqlite")).unwrap();
    let imported = import_codebuddy_nativepath(
        &root,
        &mut store,
        context(&root),
        ProviderImportOptions {
            inventory_observation_token: Some("inventory-generation-7".to_owned()),
            ..ProviderImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(imported.imported_events, 1);
}

#[test]
fn released_cursor_is_consumed_only_as_a_migration_input() {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".codebuddy");
    let path = write_cli_root(
        &root,
        &[cli_line("migrated", "user", "message", "legacy migration")],
    );
    let canonical_path = fs::canonicalize(&path).unwrap();
    let locator_identity = provider_path_identity(&canonical_path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        &locator_identity,
    );
    let legacy = CertifiedProviderCursor::new(
        "released-codebuddy-revision",
        3,
        5,
        NativePosition::new("legacy-codebuddy-position", Vec::new()).unwrap(),
        BoundedParserCheckpoint::from_serializable(&()).unwrap(),
    )
    .unwrap()
    .encode()
    .unwrap();
    let adapter_context = context(&root);
    let import_options = ProviderImportOptions::default();
    let source = discover_sources(&root, &root, &import_options)
        .unwrap()
        .sources
        .into_iter()
        .next()
        .unwrap();
    let initial = initial_cursor(&source, &adapter_context).unwrap();
    let page = next_cli_page(&source, &initial, &adapter_context).unwrap();
    let core = page.records[0].classification.core().unwrap();
    let provider_session_id = core.session.provider_session_id.clone();
    let raw_source_path = source.canonical_path.display().to_string();
    let source_id = provider_scoped_source_uuid(
        CaptureProvider::CodeBuddy,
        &provider_session_id,
        CODEBUDDY_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    let mut store = Store::open(temp.path().join("migration.sqlite")).unwrap();
    let source_record = capture_source(
        source_id,
        &source,
        &adapter_context,
        &core.session,
        &source.proposed_source_identity,
        &root.display().to_string(),
    );
    store.upsert_capture_source(&source_record).unwrap();
    let session_id = provider_session_uuid(CaptureProvider::CodeBuddy, &provider_session_id);
    let session = normalized_session(
        session_id,
        source_id,
        &adapter_context,
        &import_options,
        &core.session,
    );
    store.upsert_session(&session).unwrap();
    let released_hash = core.event.legacy_provider_event_hash.clone();
    let released_identity = provider_event_import_identity(
        &store,
        CaptureProvider::CodeBuddy,
        &provider_session_id,
        source_id,
        0,
        0,
        &released_hash,
        Some(0),
        true,
    )
    .unwrap();
    let released_event = Event {
        id: released_identity.id,
        seq: released_identity.seq,
        history_record_id: None,
        session_id: Some(session_id),
        run_id: None,
        event_type: core.event.event_type,
        role: Some(core.event.role),
        occurred_at: core.event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::CodeBuddy.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": 0,
            "provider_event_hash": released_hash,
            "body": core.event.payload,
        }),
        payload_blob_id: None,
        dedupe_key: Some(released_identity.dedupe_key),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": 0,
                "provider_event_hash": released_hash,
                "provider_event_hash_authority": "provider_supplied",
                "source_record_ordinal": null,
                "source_record_subrecord_index": null,
                "metadata": core.event.metadata,
            }),
        ),
    };
    assert!(store.insert_event_if_absent(&released_event).unwrap());

    let imported_at = adapter_context.imported_at;
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: "codebuddy-nativepath-test-machine".to_owned(),
            stream: stream.clone(),
            cursor: legacy,
            last_synced_at: Some(imported_at),
            timestamps: EntityTimestamps {
                created_at: imported_at,
                updated_at: imported_at,
            },
        })
        .unwrap();

    let migrated = import_codebuddy_nativepath(
        &root,
        &mut store,
        adapter_context,
        ProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(migrated.imported_events, 0);
    assert_eq!(migrated.skipped_events, 1);
    let events = codebuddy_events(&store, "project-hash/cli-session");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, released_event.id);
    assert_eq!(
        events[0].sync.metadata["provider_event_hash_authority"],
        "normalized_payload_fallback"
    );
    assert_eq!(
        events[0].sync.metadata["source_record_ordinal"].as_u64(),
        Some(0)
    );
    let stored = store
        .get_sync_cursor(None, "codebuddy-nativepath-test-machine", &stream)
        .unwrap()
        .unwrap();
    let committed = decode_native_path_committed_cursor(&stored.cursor).unwrap();
    let cursor = CodeBuddyNativeCursor::decode(committed.provider_cursor()).unwrap();
    assert_eq!(cursor.generation, 1);
    assert!(cursor.terminal);
}
