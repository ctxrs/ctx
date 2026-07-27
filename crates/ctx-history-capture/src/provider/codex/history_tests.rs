use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use chrono::DateTime;
use ctx_history_core::{CaptureProvider, SyncCursor};
use ctx_history_store::{decode_native_path_committed_cursor, Store};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    native_source::NativePosition,
    provider::importer::{
        provider_path_identity, provider_source_cursor_stream_for_path, timestamps,
        BoundedParserCheckpoint, CertifiedProviderCursor,
    },
    test_support_paths::tempdir,
    CaptureError, CaptureWorkLimit, ImportProfile, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProviderImportSummary, ProviderImportWorkResult,
};

use super::*;

const MACHINE: &str = "codex-prompt-history-nativepath-test-machine";
const SOURCE_FORMAT: &str = "codex_history_jsonl";
const SUCCESSFUL_OUTPUT_SECRET: &str = "successful-output-must-not-enter-core";

fn history_line(session_id: &str, timestamp: i64, text: &str) -> String {
    serde_json::to_string(&json!({
        "session_id": session_id,
        "ts": timestamp,
        "text": text,
    }))
    .expect("history line")
}

fn history_line_with_output_field(session_id: &str, timestamp: i64, text: &str) -> String {
    serde_json::to_string(&json!({
        "session_id": session_id,
        "ts": timestamp,
        "text": text,
        "successful_output": SUCCESSFUL_OUTPUT_SECRET,
        "exit_code": 0,
    }))
    .expect("history line")
}

fn write_lines(path: &Path, lines: &[String]) {
    let mut contents = lines.join("\n");
    contents.push('\n');
    fs::write(path, contents).expect("write prompt history");
}

fn import_options(path: &Path) -> CodexHistoryImportOptions {
    CodexHistoryImportOptions {
        machine_id: MACHINE.to_owned(),
        source_path: Some(path.to_path_buf()),
        imported_at: "2026-07-25T12:00:00Z".parse().expect("timestamp"),
        history_record_id: None,
        capture_work_limit: CaptureWorkLimit::Drain,
        inventory_observation_token: None,
        import_profile: ImportProfile::CoreOnly,
    }
}

fn import(path: &Path, store: &mut Store, work_limit: CaptureWorkLimit) -> ProviderImportSummary {
    import_codex_history_jsonl(
        path,
        store,
        CodexHistoryImportOptions {
            capture_work_limit: work_limit,
            ..import_options(path)
        },
    )
    .expect("import prompt history")
}

fn cursor_stream(path: &Path) -> String {
    provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        SOURCE_FORMAT,
        &provider_path_identity(path).expect("path identity"),
    )
}

fn session(store: &Store, external_id: &str) -> ctx_history_core::Session {
    store
        .session_by_external_session(CaptureProvider::Codex, external_id)
        .expect("session lookup")
        .expect("session")
}

#[test]
fn nativepath_fresh_bounded_restart_noop_append_and_core_output_privacy() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("history.jsonl");
    let store_path = temp.path().join("core.sqlite");
    let mut lines = vec![history_line_with_output_field(
        "bounded-session",
        1_784_371_200,
        "private prompt 0",
    )];
    lines.extend((1..70).map(|index| {
        history_line(
            "bounded-session",
            1_784_371_200 + index,
            &format!("private prompt {index}"),
        )
    }));
    write_lines(&path, &lines);
    let mut store = Store::open(&store_path).expect("store");

    let first = import(&path, &mut store, CaptureWorkLimit::OneSafeGroup);
    assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(first.imported_events, 60);
    assert!(first.work_remaining);

    drop(store);
    let mut store = Store::open(&store_path).expect("restart store");
    let mut attempts = 0;
    loop {
        attempts += 1;
        let next = import(&path, &mut store, CaptureWorkLimit::OneSafeGroup);
        if !next.work_remaining {
            break;
        }
        assert!(
            attempts < 5,
            "bounded import did not reach a terminal cursor"
        );
    }

    let imported_session = session(&store, "bounded-session");
    let events = store
        .events_for_session(imported_session.id)
        .expect("session events");
    assert_eq!(events.len(), 70);
    assert_eq!(
        imported_session.started_at,
        DateTime::from_timestamp(1_784_371_200, 0).expect("event timestamp")
    );
    for event in &events {
        assert_eq!(event.role, Some(ctx_history_core::EventRole::User));
        let retained = serde_json::to_string(&(event.payload.clone(), event.sync.metadata.clone()))
            .expect("retained event JSON");
        assert!(!retained.contains(SUCCESSFUL_OUTPUT_SECRET));
    }

    let stored_cursor = store
        .get_sync_cursor(None, MACHINE, &cursor_stream(&path))
        .expect("cursor lookup")
        .expect("cursor");
    decode_native_path_committed_cursor(&stored_cursor.cursor).expect("NativePath cursor");

    let replay = import(&path, &mut store, CaptureWorkLimit::Drain);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.skipped_events, 70);

    let mut file = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append history");
    writeln!(
        file,
        "{}",
        history_line("bounded-session", 1_784_371_270, "appended prompt")
    )
    .expect("append line");
    file.sync_all().expect("sync history");

    let appended = import(&path, &mut store, CaptureWorkLimit::Drain);
    assert_eq!(appended.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(appended.imported_events, 1);
    assert_eq!(
        store
            .events_for_session(imported_session.id)
            .expect("appended events")
            .iter()
            .filter(|event| event.sync.deleted_at.is_none())
            .count(),
        71
    );
}

#[test]
fn corrupt_lines_are_bounded_failures_and_do_not_block_valid_prompts() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("history.jsonl");
    fs::write(
        &path,
        format!(
            "{}\n{{\"session_id\"\n \t\n{}\n{}\n",
            history_line("corrupt-session", 1_784_371_200, "first"),
            history_line("corrupt-session", i64::MAX, "bad timestamp"),
            history_line("corrupt-session", 1_784_371_202, "second"),
        ),
    )
    .expect("write corrupt history");
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");

    let summary = import(&path, &mut store, CaptureWorkLimit::Drain);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(summary.failed, 2);
    assert_eq!(summary.failures.len(), 2);
    assert_eq!(
        store
            .events_for_session(session(&store, "corrupt-session").id)
            .expect("events")
            .len(),
        2
    );

    let replay = import(&path, &mut store, CaptureWorkLimit::Drain);
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(replay.failed, 2);
    assert_eq!(replay.skipped_events, 2);
}

#[test]
fn configured_logical_path_owns_identity_while_physical_path_owns_locator() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("physical-history.jsonl");
    let logical_path = temp.path().join("configured-history.jsonl");
    write_lines(
        &path,
        &[history_line("logical-session", 1_784_371_200, "prompt")],
    );
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    import_codex_history_jsonl(
        &path,
        &mut store,
        CodexHistoryImportOptions {
            source_path: Some(logical_path.clone()),
            ..import_options(&path)
        },
    )
    .expect("logical path import");

    let source = store
        .list_capture_sources()
        .expect("capture sources")
        .pop()
        .expect("capture source");
    let logical = logical_path.display().to_string();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        Some(logical.as_str())
    );
    assert_eq!(
        source.descriptor.source_root.as_deref(),
        Some(logical.as_str())
    );
    let event = store
        .events_for_session(session(&store, "logical-session").id)
        .expect("events")
        .pop()
        .expect("event");
    let route = store
        .authorized_source_route_for_event(event.id)
        .expect("authorized physical route");
    assert_eq!(route.path(), path);

    let logical_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        SOURCE_FORMAT,
        &provider_path_identity(&logical_path).expect("logical identity"),
    );
    let cursor = store
        .get_sync_cursor(None, MACHINE, &logical_stream)
        .expect("cursor lookup")
        .expect("logical route cursor");
    decode_native_path_committed_cursor(&cursor.cursor).expect("NativePath cursor");

    let tokenized = import_codex_history_jsonl(
        &path,
        &mut store,
        CodexHistoryImportOptions {
            source_path: Some(logical_path),
            inventory_observation_token: Some("inventory-generation-2".to_owned()),
            ..import_options(&path)
        },
    )
    .expect("inventory-authorized revision");
    assert_eq!(tokenized.work_result(), ProviderImportWorkResult::Changed);
}

#[test]
fn rewrite_truncation_replacement_and_restore_use_source_generations() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("history.jsonl");
    let replacement = temp.path().join("replacement.jsonl");
    write_lines(
        &path,
        &[
            history_line("session-a", 1_784_371_300, "alpha"),
            history_line("session-b", 1_784_371_250, "bravo"),
            history_line("session-a", 1_784_371_200, "later-first-seen"),
        ],
    );
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    assert_eq!(
        import(&path, &mut store, CaptureWorkLimit::Drain).imported_events,
        3
    );
    let session_a = session(&store, "session-a");
    let session_b = session(&store, "session-b");

    write_lines(
        &path,
        &[
            history_line("session-a", 1_784_371_300, "omega"),
            history_line("session-a", 1_784_371_200, "later-first-seen"),
        ],
    );
    assert_eq!(
        import(&path, &mut store, CaptureWorkLimit::Drain).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(
        store
            .get_session(session_b.id)
            .expect("retired session")
            .sync
            .deleted_at
            .is_some(),
        "rewrite omission must retire the missing session"
    );
    assert_eq!(
        store
            .events_for_session(session_a.id)
            .expect("rewritten events")
            .iter()
            .filter(|event| event.sync.deleted_at.is_none())
            .count(),
        2
    );

    write_lines(&path, &[history_line("session-a", 1_784_371_300, "omega")]);
    assert_eq!(
        import(&path, &mut store, CaptureWorkLimit::Drain).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert_eq!(
        store
            .events_for_session(session_a.id)
            .expect("truncated events")
            .iter()
            .filter(|event| event.sync.deleted_at.is_none())
            .count(),
        1
    );

    let current = fs::read(&path).expect("current history");
    fs::write(&replacement, &current).expect("replacement history");
    fs::rename(&replacement, &path).expect("replace history inode");
    assert_eq!(
        import(&path, &mut store, CaptureWorkLimit::Drain).work_result(),
        ProviderImportWorkResult::Changed,
        "same bytes at a replacement inode are a new source generation"
    );

    write_lines(
        &path,
        &[
            history_line("session-a", 1_784_371_300, "omega"),
            history_line("session-b", 1_784_371_250, "bravo"),
        ],
    );
    import(&path, &mut store, CaptureWorkLimit::Drain);
    assert!(
        store
            .get_session(session_b.id)
            .expect("restored session")
            .sync
            .deleted_at
            .is_none(),
        "a later generation must restore a retained session"
    );
}

#[test]
fn canonical_move_preserves_entities_and_rebinds_the_physical_route() {
    let temp = tempdir().expect("tempdir");
    let original = temp.path().join("history.jsonl");
    let moved = temp.path().join("renamed-prompts.jsonl");
    let line = history_line("moved-session", 1_784_371_200, "moved prompt");
    write_lines(&original, &[line]);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    import(&original, &mut store, CaptureWorkLimit::Drain);
    let imported_session = session(&store, "moved-session");
    let event_id = store
        .events_for_session(imported_session.id)
        .expect("events")[0]
        .id;

    fs::rename(&original, &moved).expect("move prompt history");
    let relocated = import(&moved, &mut store, CaptureWorkLimit::Drain);
    assert_eq!(relocated.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(session(&store, "moved-session").id, imported_session.id);
    let events = store
        .events_for_session(imported_session.id)
        .expect("relocated events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, event_id);
    assert!(events[0].sync.deleted_at.is_none());
    assert_eq!(
        store
            .authorized_source_route_for_event(event_id)
            .expect("relocated route")
            .path(),
        moved
    );

    fs::remove_file(&moved).expect("remove relocated prompt history");
    assert_eq!(
        import(&moved, &mut store, CaptureWorkLimit::Drain).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .get_session(imported_session.id)
        .expect("retained relocated session")
        .sync
        .deleted_at
        .is_none());
    assert!(store
        .events_for_session(imported_session.id)
        .expect("retained relocated events")[0]
        .sync
        .deleted_at
        .is_none());
    assert!(store.authorized_source_route_for_event(event_id).is_err());
}

#[test]
fn disappearance_retires_only_the_route_and_preserves_prior_entities() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("history.jsonl");
    let line = history_line("missing-session", 1_784_371_200, "prompt");
    write_lines(&path, std::slice::from_ref(&line));
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    import(&path, &mut store, CaptureWorkLimit::Drain);
    let imported_session = session(&store, "missing-session");
    let event_id = store
        .events_for_session(imported_session.id)
        .expect("events")[0]
        .id;

    fs::remove_file(&path).expect("remove history");
    let missing = import(&path, &mut store, CaptureWorkLimit::Drain);
    assert_eq!(missing.work_result(), ProviderImportWorkResult::Changed);
    assert!(store
        .get_session(imported_session.id)
        .expect("retained session")
        .sync
        .deleted_at
        .is_none());
    let retained_events = store
        .events_for_session(imported_session.id)
        .expect("retained events");
    assert_eq!(retained_events.len(), 1);
    assert_eq!(retained_events[0].id, event_id);
    assert!(retained_events[0].sync.deleted_at.is_none());
    assert!(store.authorized_source_route_for_event(event_id).is_err());
    let terminal_missing = store
        .get_sync_cursor(None, MACHINE, &cursor_stream(&path))
        .expect("cursor lookup")
        .expect("terminal missing cursor");
    let terminal_missing =
        decode_native_path_committed_cursor(&terminal_missing.cursor).expect("NativePath cursor");
    let terminal_missing: Value =
        serde_json::from_str(terminal_missing.provider_cursor()).expect("provider cursor JSON");
    assert_eq!(terminal_missing["phase"]["phase"], "complete");
    assert_eq!(terminal_missing["phase"]["missing"], true);
    assert_eq!(
        import(&path, &mut store, CaptureWorkLimit::Drain).work_result(),
        ProviderImportWorkResult::NoOp
    );

    write_lines(&path, &[line]);
    assert_eq!(
        import(&path, &mut store, CaptureWorkLimit::Drain).work_result(),
        ProviderImportWorkResult::Changed
    );
    assert!(store
        .get_session(imported_session.id)
        .expect("retained session after route restoration")
        .sync
        .deleted_at
        .is_none());
    assert_eq!(
        store
            .events_for_session(imported_session.id)
            .expect("events after route restoration")
            .len(),
        1
    );
    store
        .authorized_source_route_for_event(event_id)
        .expect("restored source route");
}

#[test]
fn disappearance_during_bounded_core_preserves_the_committed_partial_generation() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("history.jsonl");
    let lines = (0..70)
        .map(|index| {
            history_line(
                "partial-missing-session",
                1_784_371_200 + index,
                &format!("prompt {index}"),
            )
        })
        .collect::<Vec<_>>();
    write_lines(&path, &lines);
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    let partial = import(&path, &mut store, CaptureWorkLimit::OneSafeGroup);
    assert!(partial.work_remaining);
    assert_eq!(partial.imported_events, 60);
    let imported_session = session(&store, "partial-missing-session");

    fs::remove_file(&path).expect("remove partial history");
    let missing = import(&path, &mut store, CaptureWorkLimit::OneSafeGroup);
    assert!(!missing.work_remaining);
    assert!(store
        .get_session(imported_session.id)
        .expect("retained partial session")
        .sync
        .deleted_at
        .is_none());
    let partial_events = store
        .events_for_session(imported_session.id)
        .expect("retained partial events");
    assert_eq!(partial_events.len(), 60);
    assert!(partial_events
        .iter()
        .all(|event| event.sync.deleted_at.is_none()));

    write_lines(&path, &lines);
    let restored = import(&path, &mut store, CaptureWorkLimit::OneSafeGroup);
    assert!(restored.work_remaining);
    let drained = import(&path, &mut store, CaptureWorkLimit::Drain);
    assert_eq!(drained.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(
        store
            .events_for_session(imported_session.id)
            .expect("restored events")
            .iter()
            .filter(|event| event.sync.deleted_at.is_none())
            .count(),
        70
    );
}

#[cfg(unix)]
#[test]
fn linked_prompt_history_paths_are_typed_failures_not_missing_route_retirements() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("history.jsonl");
    let line = history_line("linked-session", 1_784_371_200, "prompt");
    write_lines(&path, std::slice::from_ref(&line));
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    import(&path, &mut store, CaptureWorkLimit::Drain);
    let event_id = store
        .events_for_session(session(&store, "linked-session").id)
        .expect("events")[0]
        .id;

    fs::remove_file(&path).expect("remove source");
    symlink(temp.path().join("missing-target.jsonl"), &path).expect("dangling link");
    let error = import_codex_history_jsonl(
        &path,
        &mut store,
        CodexHistoryImportOptions {
            capture_work_limit: CaptureWorkLimit::Drain,
            ..import_options(&path)
        },
    )
    .expect_err("linked path is not a missing source");
    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { .. }
    ));
    store
        .authorized_source_route_for_event(event_id)
        .expect("failed import leaves the prior route authorized");

    let parent_target = temp.path().join("real-parent");
    fs::create_dir_all(&parent_target).expect("real parent");
    let linked_parent = temp.path().join("linked-parent");
    symlink(&parent_target, &linked_parent).expect("linked parent");
    let parent_error = import_codex_history_jsonl(
        linked_parent.join("history.jsonl"),
        &mut store,
        import_options(&linked_parent.join("history.jsonl")),
    )
    .expect_err("linked parent is rejected");
    assert!(matches!(
        parent_error,
        CaptureError::InvalidProviderTranscriptPath { .. }
    ));
}

#[test]
fn only_certified_released_cursors_are_migrated_to_nativepath() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("history.jsonl");
    write_lines(
        &path,
        &[history_line("migration-session", 1_784_371_200, "prompt")],
    );
    let imported_at = import_options(&path).imported_at;
    let stream = cursor_stream(&path);
    let released = CertifiedProviderCursor::new(
        "released-codex-prompt-history-revision",
        1,
        1,
        NativePosition::new("released-codex-history-position-v1", vec![0])
            .expect("released position"),
        BoundedParserCheckpoint::from_serializable(&()).expect("released checkpoint"),
    )
    .expect("released cursor")
    .encode()
    .expect("encoded released cursor");
    let mut store = Store::open(temp.path().join("core.sqlite")).expect("store");
    store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: MACHINE.to_owned(),
            stream: stream.clone(),
            cursor: released,
            last_synced_at: Some(imported_at),
            timestamps: timestamps(imported_at),
        })
        .expect("seed released cursor");

    assert_eq!(
        import(&path, &mut store, CaptureWorkLimit::Drain).work_result(),
        ProviderImportWorkResult::Changed
    );
    let migrated = store
        .get_sync_cursor(None, MACHINE, &stream)
        .expect("cursor lookup")
        .expect("migrated cursor");
    decode_native_path_committed_cursor(&migrated.cursor).expect("NativePath migration");

    let invalid_temp = tempdir().expect("invalid tempdir");
    let invalid_path = invalid_temp.path().join("history.jsonl");
    write_lines(
        &invalid_path,
        &[history_line("invalid-session", 1_784_371_200, "prompt")],
    );
    let mut invalid_store =
        Store::open(invalid_temp.path().join("core.sqlite")).expect("invalid store");
    invalid_store
        .upsert_sync_cursor(&SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: MACHINE.to_owned(),
            stream: cursor_stream(&invalid_path),
            cursor: "uncertified-provider-cursor".to_owned(),
            last_synced_at: Some(imported_at),
            timestamps: timestamps(imported_at),
        })
        .expect("seed invalid cursor");
    let error = import_codex_history_jsonl(
        &invalid_path,
        &mut invalid_store,
        import_options(&invalid_path),
    )
    .expect_err("uncertified cursor must fail closed");
    assert!(error
        .to_string()
        .contains("neither NativePath nor a released"));
}

struct RecordingSink {
    store_path: std::path::PathBuf,
    progress: Mutex<HashMap<OutputSourceIdentity, ProOutputProgress>>,
    pages: AtomicUsize,
    behind: AtomicUsize,
    fail_next: AtomicBool,
    saw_committed_core: AtomicBool,
}

impl RecordingSink {
    fn new(store_path: std::path::PathBuf, fail_next: bool) -> Self {
        Self {
            store_path,
            progress: Mutex::new(HashMap::new()),
            pages: AtomicUsize::new(0),
            behind: AtomicUsize::new(0),
            fail_next: AtomicBool::new(fail_next),
            saw_committed_core: AtomicBool::new(false),
        }
    }
}

impl ProOutputSink for RecordingSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "codex-prompt-history-test-materializer-v1"
    }

    fn observe_source(
        &self,
        source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(self.progress.lock().expect("progress").get(source).cloned())
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        let core = Store::open_read_only(&self.store_path)
            .map_err(|error| ProOutputSinkError::new("test_core", error.to_string()))?;
        if !core
            .list_sessions()
            .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
            .is_empty()
        {
            self.saw_committed_core.store(true, Ordering::SeqCst);
        }
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(ProOutputSinkError::new(
                "injected",
                "injected output failure",
            ));
        }
        if !page.observations.is_empty() {
            return Err(ProOutputSinkError::new(
                "unexpected_output",
                "prompt history must publish an empty output page",
            ));
        }
        self.pages.fetch_add(1, Ordering::SeqCst);
        let committed_cursor = page.next_safe_cursor.clone();
        self.progress.lock().expect("progress").insert(
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
            accepted_outputs: 0,
            materialized_facts: 0,
            replayed: false,
        })
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn pro_replay_is_independent_empty_and_observes_committed_core_first() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("history.jsonl");
    let store_path = temp.path().join("core.sqlite");
    write_lines(
        &path,
        &[history_line("pro-session", 1_784_371_200, "prompt")],
    );
    let mut store = Store::open(&store_path).expect("store");
    let sink = Arc::new(RecordingSink::new(store_path.clone(), true));
    let core = import_codex_history_jsonl(
        &path,
        &mut store,
        CodexHistoryImportOptions {
            import_profile: ImportProfile::CoreAndPro(sink.clone()),
            ..import_options(&path)
        },
    )
    .expect("Core survives Pro failure");
    assert_eq!(core.work_result(), ProviderImportWorkResult::Changed);
    assert_eq!(core.imported_events, 1);
    assert_eq!(sink.behind.load(Ordering::SeqCst), 1);
    assert!(sink.saw_committed_core.load(Ordering::SeqCst));

    let replay = import_codex_history_jsonl(
        &path,
        &mut store,
        CodexHistoryImportOptions {
            import_profile: ImportProfile::ProReplayOnly(sink.clone()),
            ..import_options(&path)
        },
    )
    .expect("independent Pro replay");
    assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
    assert_eq!(sink.pages.load(Ordering::SeqCst), 1);

    import_codex_history_jsonl(
        &path,
        &mut store,
        CodexHistoryImportOptions {
            import_profile: ImportProfile::ProReplayOnly(sink.clone()),
            ..import_options(&path)
        },
    )
    .expect("terminal Pro no-op");
    assert_eq!(sink.pages.load(Ordering::SeqCst), 1);
}
