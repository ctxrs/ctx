use std::{fs::FileTimes, fs::OpenOptions, io::Write};

use serde_json::json;

use crate::test_support_paths::tempdir;

use super::*;

const HEADER_CHECKPOINT_SECRET: &str = "openclaw-header-secret-must-not-enter-checkpoint";
const REWRITTEN_HEADER_SECRET: &str = "rewritten-openclaw-header-secret-same-length-000";
const INDEX_CHECKPOINT_SECRET: &str = "openclaw-index-secret-must-not-enter-checkpoint";

fn openclaw_fixture_with_messages(
    message_count: usize,
) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let temp = tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let sessions = root.join("agents/personal-agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let index = sessions.join("sessions.json");
    fs::write(
        &index,
        json!({
            "session-1": {
                "sessionId": "session-1",
                "label": "bounded OpenClaw import",
                "private_index_text": INDEX_CHECKPOINT_SECRET
            }
        })
        .to_string(),
    )
    .unwrap();
    let transcript = sessions.join("session-1.jsonl");
    let mut contents = format!(
        "{}\n",
        json!({
            "type": "session",
            "id": "session-1",
            "timestamp": "2026-07-17T12:00:00Z",
            "cwd": "/workspace/openclaw",
            "private_header_text": HEADER_CHECKPOINT_SECRET
        })
    );
    for index in 0..message_count {
        contents.push_str(
            &json!({
                "type": "message",
                "id": format!("message-{index}"),
                "timestamp": "2026-07-17T12:00:01Z",
                "message": {
                    "role": if index % 2 == 0 { "user" } else { "assistant" },
                    "content": format!("bounded OpenClaw message {index}")
                }
            })
            .to_string(),
        );
        contents.push('\n');
    }
    fs::write(&transcript, contents).unwrap();
    (temp, root, transcript, index)
}

fn openclaw_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    openclaw_fixture_with_messages(65)
}

#[test]
fn result_profile_returns_only_explicit_legacy_tool_message_content() {
    let row = json!({
        "type": "message",
        "message": {
            "role": "tool",
            "name": "shell",
            "content": [
                {"type": "text", "text": "first"},
                {"output": "second"}
            ]
        }
    });
    assert_eq!(
        openclaw_result_content(&row).as_deref(),
        Some("first\nsecond")
    );
    assert_eq!(
        openclaw_result_content(&json!({
            "type": "message",
            "message": {"role": "tool", "name": "shell"}
        })),
        None
    );
    assert_eq!(
        openclaw_result_content(&json!({
            "type": "message",
            "message": {"role": "assistant", "content": "not a result"}
        })),
        None
    );
}

fn import_options() -> NormalizedProviderImportOptions {
    NormalizedProviderImportOptions {
        history_record_id: None,
        persist_cursors: false,
        wrap_transaction: true,
        fast_event_inserts: true,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    }
}

#[test]
fn openclaw_session_batches_resume_append_and_preserve_scope() {
    // Header plus 127 events fills exactly two 64-record batches. The appended record must
    // therefore begin the same third batch in both resumed and one-shot imports.
    let (temp, root, transcript, _index) = openclaw_fixture_with_messages(127);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "openclaw-batch-machine".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root.clone()),
        imported_at: "2026-07-17T12:30:00Z".parse().unwrap(),
    };

    let first = import_openclaw_session_jsonl_file_batched(
        &transcript,
        &mut store,
        context.clone(),
        import_options(),
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 127);

    let source = store
        .capture_source_by_external_session(CaptureProvider::OpenClaw, "personal-agent/session-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        Some(transcript.to_string_lossy().as_ref())
    );
    assert_eq!(
        source.descriptor.source_root.as_deref(),
        Some(root.to_string_lossy().as_ref())
    );
    let header_preview: Value = serde_json::from_str(
        source.sync.metadata["source_metadata"]["header"]["json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(header_preview["truncated"], false);
    let header: Value = serde_json::from_str(header_preview["json"].as_str().unwrap()).unwrap();
    assert_eq!(
        header,
        json!({
            "type": "session",
            "id": "session-1",
            "timestamp": "2026-07-17T12:00:00Z",
            "cwd": "/workspace/openclaw",
            "private_header_text": HEADER_CHECKPOINT_SECRET,
        })
    );
    let session = store
        .session_by_external_session(CaptureProvider::OpenClaw, "personal-agent/session-1")
        .unwrap()
        .unwrap();
    let session_index_preview: Value = serde_json::from_str(
        session.sync.metadata["metadata"]["session_index"]["json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(session_index_preview["truncated"], false);
    let session_index: Value =
        serde_json::from_str(session_index_preview["json"].as_str().unwrap()).unwrap();
    assert_eq!(session_index["label"], "bounded OpenClaw import");

    let replay = import_openclaw_session_jsonl_file_batched(
        &transcript,
        &mut store,
        context.clone(),
        import_options(),
    )
    .unwrap();
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_sessions, 1);
    assert_eq!(replay.skipped_events, 127);

    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "type": "message",
            "id": "message-appended",
            "timestamp": "2026-07-17T12:01:30Z",
            "message": {
                "role": "assistant",
                "content": "appended OpenClaw answer"
            }
        })
    )
    .unwrap();
    drop(file);

    let one_shot_context = context.clone();
    let append = import_openclaw_session_jsonl_file_batched(
        &transcript,
        &mut store,
        context,
        import_options(),
    )
    .unwrap();
    assert_eq!(append.failed, 0, "{:?}", append.failures);
    assert_eq!(append.imported_sessions, 0);
    assert_eq!(append.imported_events, 1);

    let cursor_path = provider_path_identity(&transcript).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::OpenClaw,
        OPENCLAW_SOURCE_FORMAT,
        &cursor_path,
    );
    let cursor = store
        .get_sync_cursor(None, "openclaw-batch-machine", &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&cursor.cursor).unwrap();
    let checkpoint: OpenClawParserCheckpoint = certified.parser_checkpoint().deserialize().unwrap();
    assert_eq!(checkpoint.next_ordinal, 129);
    assert!(checkpoint
        .header_anchor
        .is_some_and(|anchor| anchor.start == 0 && anchor.end > anchor.start));
    assert_eq!(checkpoint.accepted_events, 128);
    assert!(checkpoint.emitted_session);
    let checkpoint_bytes = certified.parser_checkpoint().as_bytes();
    assert!(checkpoint_bytes.len() < 2 * 1024);
    let checkpoint_text = String::from_utf8_lossy(checkpoint_bytes);
    assert!(!checkpoint_text.contains(HEADER_CHECKPOINT_SECRET));
    assert!(!checkpoint_text.contains(INDEX_CHECKPOINT_SECRET));
    assert!(!checkpoint_text.contains("header_raw"));

    let resumed_session = store
        .session_by_external_session(CaptureProvider::OpenClaw, "personal-agent/session-1")
        .unwrap()
        .unwrap();
    let resumed_source = store
        .capture_source_by_external_session(CaptureProvider::OpenClaw, "personal-agent/session-1")
        .unwrap()
        .unwrap();
    let resumed_events = store.events_for_session(resumed_session.id).unwrap();

    let mut one_shot_store = Store::open(temp.path().join("one-shot.sqlite")).unwrap();
    let one_shot = import_openclaw_session_jsonl_file_batched(
        &transcript,
        &mut one_shot_store,
        one_shot_context,
        import_options(),
    )
    .unwrap();
    assert_eq!(one_shot.failed, 0, "{:?}", one_shot.failures);
    assert_eq!(one_shot.imported_sessions, 1);
    assert_eq!(one_shot.imported_events, 128);
    let one_shot_session = one_shot_store
        .session_by_external_session(CaptureProvider::OpenClaw, "personal-agent/session-1")
        .unwrap()
        .unwrap();
    let one_shot_source = one_shot_store
        .capture_source_by_external_session(CaptureProvider::OpenClaw, "personal-agent/session-1")
        .unwrap()
        .unwrap();
    let one_shot_events = one_shot_store
        .events_for_session(one_shot_session.id)
        .unwrap();

    assert_eq!(resumed_session, one_shot_session);
    assert_eq!(resumed_source, one_shot_source);
    assert_eq!(resumed_events, one_shot_events);
}

#[test]
fn openclaw_header_anchor_detects_old_rewrite_beyond_append_proof() {
    assert_eq!(
        HEADER_CHECKPOINT_SECRET.len(),
        REWRITTEN_HEADER_SECRET.len()
    );
    let (temp, root, transcript, _index) = openclaw_fixture_with_messages(600);
    let mut store = Store::open(temp.path().join("header-anchor.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "openclaw-header-anchor-machine".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root),
        imported_at: "2026-07-17T12:30:00Z".parse().unwrap(),
    };

    let first = import_openclaw_session_jsonl_file_batched(
        &transcript,
        &mut store,
        context.clone(),
        import_options(),
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_events, 600);

    let cursor_path = provider_path_identity(&transcript).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::OpenClaw,
        OPENCLAW_SOURCE_FORMAT,
        &cursor_path,
    );
    let cursor = store
        .get_sync_cursor(None, "openclaw-header-anchor-machine", &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&cursor.cursor).unwrap();
    let checkpoint: OpenClawParserCheckpoint = certified.parser_checkpoint().deserialize().unwrap();
    let anchor = checkpoint.header_anchor.unwrap();
    let source_length = fs::metadata(&transcript).unwrap().len();
    assert!(source_length - anchor.end > 64 * 1024);

    let original_metadata = fs::metadata(&transcript).unwrap();
    let original = fs::read_to_string(&transcript).unwrap();
    let rewritten = original.replacen(HEADER_CHECKPOINT_SECRET, REWRITTEN_HEADER_SECRET, 1);
    assert_ne!(rewritten, original);
    assert_eq!(rewritten.len(), original.len());
    fs::write(&transcript, rewritten).unwrap();
    let rewritten_file = OpenOptions::new().write(true).open(&transcript).unwrap();
    rewritten_file
        .set_times(
            FileTimes::new()
                .set_accessed(original_metadata.accessed().unwrap())
                .set_modified(original_metadata.modified().unwrap()),
        )
        .unwrap();
    drop(rewritten_file);

    let same_revision = import_openclaw_session_jsonl_file_batched(
        &transcript,
        &mut store,
        context.clone(),
        import_options(),
    )
    .unwrap_err();
    assert!(matches!(
        same_revision,
        CaptureError::SourceChangedDuringCapture
    ));

    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "type": "message",
            "id": "message-after-old-header-rewrite",
            "timestamp": "2026-07-17T12:02:00Z",
            "message": {
                "role": "assistant",
                "content": "replacement after old header rewrite"
            }
        })
    )
    .unwrap();
    drop(file);

    let replacement = import_openclaw_session_jsonl_file_batched(
        &transcript,
        &mut store,
        context,
        import_options(),
    )
    .unwrap();
    assert_eq!(replacement.failed, 0, "{:?}", replacement.failures);
    assert_eq!(replacement.imported_events, 1);
    assert_eq!(replacement.skipped_events, 600);
    let session = store
        .session_by_external_session(CaptureProvider::OpenClaw, "personal-agent/session-1")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 601);
    let source = store
        .capture_source_by_external_session(CaptureProvider::OpenClaw, "personal-agent/session-1")
        .unwrap()
        .unwrap();
    let header_preview: Value = serde_json::from_str(
        source.sync.metadata["source_metadata"]["header"]["json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let header: Value = serde_json::from_str(header_preview["json"].as_str().unwrap()).unwrap();
    assert_eq!(header["private_header_text"], REWRITTEN_HEADER_SECRET);
}

#[test]
fn openclaw_rejection_count_survives_unchanged_replay() {
    let (temp, root, transcript, _index) = openclaw_fixture();
    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    writeln!(file, "{{malformed-openclaw-record").unwrap();
    drop(file);

    let mut store = Store::open(temp.path().join("rejection-replay.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "openclaw-rejection-replay-machine".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root),
        imported_at: "2026-07-17T12:30:00Z".parse().unwrap(),
    };

    let first = import_openclaw_session_jsonl_file_batched(
        &transcript,
        &mut store,
        context.clone(),
        import_options(),
    )
    .unwrap();
    assert_eq!(first.failed, 1);
    assert_eq!(first.imported_events, 65);

    let cursor_path = provider_path_identity(&transcript).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::OpenClaw,
        OPENCLAW_SOURCE_FORMAT,
        &cursor_path,
    );
    let cursor = store
        .get_sync_cursor(None, "openclaw-rejection-replay-machine", &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&cursor.cursor).unwrap();
    assert_eq!(certified.rejected_records(), 1);
    let checkpoint_text = String::from_utf8_lossy(certified.parser_checkpoint().as_bytes());
    assert!(!checkpoint_text.contains("rejected_records"));

    let replay = import_openclaw_session_jsonl_file_batched(
        &transcript,
        &mut store,
        context,
        import_options(),
    )
    .unwrap();
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.failed, 1);
    assert_eq!(replay.skipped_sessions, 1);
    assert_eq!(replay.skipped_events, 65);
}

#[test]
fn openclaw_observation_detects_index_and_transcript_changes() {
    let (_temp, _root, transcript, index) = openclaw_fixture();
    let observation = OpenClawSessionObservation::read(&transcript).unwrap();
    assert!(observation.revalidate(&transcript).unwrap());

    fs::write(
        &index,
        json!({
            "session-1": {
                "sessionId": "session-1",
                "label": "changed OpenClaw metadata"
            }
        })
        .to_string(),
    )
    .unwrap();
    assert!(!observation.revalidate(&transcript).unwrap());

    let changed_index_observation = OpenClawSessionObservation::read(&transcript).unwrap();
    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    writeln!(file, "{}", json!({"type": "custom", "id": "changed"})).unwrap();
    drop(file);
    assert!(!changed_index_observation.revalidate(&transcript).unwrap());
}

#[test]
fn openclaw_tree_ignores_jsonl_outside_session_directories() {
    let (temp, root, _transcript, _index) = openclaw_fixture();
    let unrelated = root.join("diagnostics.jsonl");
    fs::write(
        &unrelated,
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "id": "unrelated-session",
                "timestamp": "2026-07-17T12:00:00Z"
            }),
            json!({
                "type": "message",
                "id": "unrelated-message",
                "timestamp": "2026-07-17T12:00:01Z",
                "message": {
                    "role": "user",
                    "content": "must not be imported"
                }
            })
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "openclaw-tree-filter-machine".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root.clone()),
        imported_at: "2026-07-17T12:30:00Z".parse().unwrap(),
    };

    let summary =
        import_openclaw_session_jsonl_tree_batched(&root, &mut store, context, import_options())
            .unwrap();

    assert_eq!(summary.imported_sessions, 1);
    assert!(store
        .session_by_external_session(CaptureProvider::OpenClaw, "unrelated-session")
        .unwrap()
        .is_none());
}
