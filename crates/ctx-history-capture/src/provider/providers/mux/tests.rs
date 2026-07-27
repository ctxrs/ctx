use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use serde_json::json;

use crate::provider::importer::{
    provider_path_identity, provider_source_cursor_stream_for_path, CertifiedProviderCursor,
};
use crate::test_support_paths::tempdir;
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext,
    MAX_PROVIDER_JSONL_LINE_BYTES, MUX_SOURCE_FORMAT,
};

use super::projector::MuxParserCheckpoint;
use super::source::{mux_session_source_from_dir, MuxSessionSource, MUX_MAX_DIRECTORY_DEPTH};
use super::{import_mux_session_batched, import_mux_sessions_batched};

fn test_context(root: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "mux-batch-test-machine".to_owned(),
        source_path: Some(root.to_path_buf()),
        source_root: None,
        imported_at: "2026-07-18T18:00:00Z".parse().unwrap(),
    }
}

fn test_options() -> NormalizedProviderImportOptions {
    NormalizedProviderImportOptions {
        fast_event_inserts: true,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
        ..NormalizedProviderImportOptions::default()
    }
}

fn write_session(root: &Path, message_count: usize, partial: bool) -> MuxSessionSource {
    let session_dir = root.join("mux-batched-session");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(
        session_dir.join("metadata.json"),
        serde_json::to_vec(&json!({
            "workspaceId": "mux-batched-session",
            "projectPath": "/workspace/mux-batched",
            "model": "mux-test-model",
            "createdAt": "2026-07-18T17:00:00Z",
        }))
        .unwrap(),
    )
    .unwrap();
    let mut chat = String::new();
    for index in 0..message_count {
        chat.push_str(
            &serde_json::to_string(&json!({
                "id": format!("mux-message-{index}"),
                "role": if index % 2 == 0 { "user" } else { "assistant" },
                "parts": [{
                    "type": "text",
                    "text": format!("mux bounded message {index}"),
                }],
                "createdAt": format!("2026-07-18T17:{:02}:00Z", index % 60),
                "metadata": { "historySequence": index },
                "workspaceId": "mux-batched-session",
            }))
            .unwrap(),
        );
        chat.push('\n');
    }
    fs::write(session_dir.join("chat.jsonl"), chat).unwrap();
    if partial {
        fs::write(
            session_dir.join("partial.json"),
            serde_json::to_vec(&json!({
                "id": "mux-partial-message",
                "role": "assistant",
                "parts": [{
                    "type": "text",
                    "text": "mux bounded partial oracle",
                }],
                "createdAt": "2026-07-18T18:05:00Z",
                "metadata": {
                    "historySequence": message_count,
                    "partial": true,
                },
                "workspaceId": "mux-batched-session",
            }))
            .unwrap(),
        )
        .unwrap();
    }
    mux_session_source_from_dir(&session_dir).unwrap().unwrap()
}

#[test]
fn bounded_mux_streams_chat_partial_and_verified_append() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let source = write_session(&root, 65, true);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = test_context(&root);
    let options = test_options();

    let first = import_mux_session_batched(source.clone(), &mut store, &context, &options).unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_events, 66);
    let session = store
        .session_by_external_session(CaptureProvider::Mux, "mux-batched-session")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 66);

    let archive = store.export_archive().unwrap();
    let root_display = root.display().to_string();
    let mux_sources = archive
        .capture_sources
        .iter()
        .filter(|capture_source| capture_source.descriptor.provider == CaptureProvider::Mux)
        .collect::<Vec<_>>();
    assert_eq!(mux_sources.len(), 2);
    assert!(mux_sources.iter().all(|capture_source| {
        capture_source.descriptor.source_root.as_deref() == Some(root_display.as_str())
    }));
    assert!(mux_sources.iter().any(|capture_source| {
        capture_source
            .descriptor
            .raw_source_path
            .as_deref()
            .is_some_and(|path| path.ends_with("chat.jsonl"))
    }));
    assert!(mux_sources.iter().any(|capture_source| {
        capture_source
            .descriptor
            .raw_source_path
            .as_deref()
            .is_some_and(|path| path.ends_with("partial.json"))
    }));

    let replay =
        import_mux_session_batched(source.clone(), &mut store, &context, &options).unwrap();
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_events, 66);

    let mut chat = OpenOptions::new()
        .append(true)
        .open(source.chat_path.as_ref().unwrap())
        .unwrap();
    writeln!(
        chat,
        "{}",
        serde_json::to_string(&json!({
            "id": "mux-message-65",
            "role": "assistant",
            "parts": [{ "type": "text", "text": "mux verified append oracle" }],
            "createdAt": "2026-07-18T18:06:00Z",
            "metadata": { "historySequence": 65 },
            "workspaceId": "mux-batched-session",
        }))
        .unwrap()
    )
    .unwrap();
    chat.sync_all().unwrap();

    let appended = import_mux_session_batched(source, &mut store, &context, &options).unwrap();
    assert_eq!(appended.failed, 0, "{:?}", appended.failures);
    assert_eq!(appended.imported_events, 1);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 67);
}

#[test]
fn mux_append_resume_partition_matches_one_shot_exactly() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let source = write_session(&root, 65, false);
    let context = test_context(&root);
    let options = test_options();
    let mut resumed_store = Store::open(temp.path().join("resumed.sqlite")).unwrap();

    let initial =
        import_mux_session_batched(source.clone(), &mut resumed_store, &context, &options).unwrap();
    assert_eq!(initial.failed, 0, "{:?}", initial.failures);
    assert_eq!(initial.imported_events, 65);

    let mut chat = OpenOptions::new()
        .append(true)
        .open(source.chat_path.as_ref().unwrap())
        .unwrap();
    writeln!(
        chat,
        "{}",
        serde_json::to_string(&json!({
            "id": "mux-message-appended",
            "role": "assistant",
            "parts": [{ "type": "text", "text": "mux partition parity append" }],
            "createdAt": "2026-07-18T18:08:00Z",
            "metadata": { "historySequence": 65 },
            "workspaceId": "mux-batched-session",
        }))
        .unwrap()
    )
    .unwrap();
    chat.sync_all().unwrap();
    drop(chat);

    let resumed =
        import_mux_session_batched(source.clone(), &mut resumed_store, &context, &options).unwrap();
    assert_eq!(resumed.failed, 0, "{:?}", resumed.failures);
    assert_eq!(resumed.imported_events, 1);
    let resumed_session = resumed_store
        .session_by_external_session(CaptureProvider::Mux, "mux-batched-session")
        .unwrap()
        .unwrap();
    let resumed_source = resumed_store
        .capture_source_by_external_session(CaptureProvider::Mux, "mux-batched-session")
        .unwrap()
        .unwrap();
    let resumed_events = resumed_store
        .events_for_session(resumed_session.id)
        .unwrap();
    let path_identity = provider_path_identity(source.chat_path.as_ref().unwrap()).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        &path_identity,
    );
    let resumed_cursor = resumed_store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();

    let mut one_shot_store = Store::open(temp.path().join("one-shot.sqlite")).unwrap();
    let one_shot =
        import_mux_session_batched(source, &mut one_shot_store, &context, &options).unwrap();
    assert_eq!(one_shot.failed, 0, "{:?}", one_shot.failures);
    assert_eq!(one_shot.imported_events, 66);
    let one_shot_session = one_shot_store
        .session_by_external_session(CaptureProvider::Mux, "mux-batched-session")
        .unwrap()
        .unwrap();
    let one_shot_source = one_shot_store
        .capture_source_by_external_session(CaptureProvider::Mux, "mux-batched-session")
        .unwrap()
        .unwrap();
    let one_shot_events = one_shot_store
        .events_for_session(one_shot_session.id)
        .unwrap();
    let one_shot_cursor = one_shot_store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();

    assert_eq!(resumed_session, one_shot_session);
    assert_eq!(resumed_source, one_shot_source);
    assert_eq!(resumed_events, one_shot_events);
    assert_eq!(resumed_cursor.cursor, one_shot_cursor.cursor);
}

#[test]
fn mux_certified_checkpoint_omits_raw_metadata_and_secrets() {
    const METADATA_SECRET: &str = "mux-metadata-secret-must-not-enter-checkpoint";
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let source = write_session(&root, 65, false);
    fs::write(
        source.metadata_path.as_ref().unwrap(),
        serde_json::to_vec(&json!({
            "workspaceId": "mux-batched-session",
            "projectPath": "/workspace/mux-batched",
            "model": "mux-test-model",
            "createdAt": "2026-07-18T17:00:00Z",
            "privateMetadata": METADATA_SECRET,
        }))
        .unwrap(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = test_context(&root);
    let imported =
        import_mux_session_batched(source.clone(), &mut store, &context, &test_options()).unwrap();
    assert_eq!(imported.failed, 0, "{:?}", imported.failures);

    let path_identity = provider_path_identity(source.chat_path.as_ref().unwrap()).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        &path_identity,
    );
    let cursor = store
        .get_sync_cursor(None, &context.machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&cursor.cursor).unwrap();
    let checkpoint: MuxParserCheckpoint = certified.parser_checkpoint().deserialize().unwrap();
    assert_eq!(checkpoint.provider_session_id, "mux-batched-session");
    assert_eq!(checkpoint.next_ordinal, 65);
    assert_eq!(checkpoint.accepted_events, 65);
    let checkpoint_bytes = certified.parser_checkpoint().as_bytes();
    assert!(checkpoint_bytes.len() < 2 * 1024);
    let checkpoint_text = String::from_utf8_lossy(checkpoint_bytes);
    assert!(!checkpoint_text.contains(METADATA_SECRET));
    assert!(!checkpoint_text.contains("privateMetadata"));
    assert!(!checkpoint_text.contains("preview"));
}

#[test]
fn bounded_mux_changed_partial_resets_certified_source() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let source = write_session(&root, 2, true);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = test_context(&root);
    let options = test_options();

    import_mux_session_batched(source.clone(), &mut store, &context, &options).unwrap();
    let before_replacement = store.export_archive().unwrap();
    let before_replacement_event_count = before_replacement.events.len();
    let old_partial_event_id = store
        .search_event_hits("mux bounded partial oracle", 10)
        .unwrap()
        .into_iter()
        .find(|hit| hit.provider == Some(CaptureProvider::Mux))
        .expect("initial partial event should remain identifiable")
        .event_id;
    assert!(before_replacement
        .events
        .iter()
        .any(|event| event.id == old_partial_event_id));
    fs::write(
        source.partial_path.as_ref().unwrap(),
        serde_json::to_vec(&json!({
            "id": "mux-partial-message",
            "role": "assistant",
            "parts": [{
                "type": "text",
                "text": "mux changed partial source oracle with a longer payload",
            }],
            "createdAt": "2026-07-18T18:07:00Z",
            "metadata": { "historySequence": 2, "partial": true },
            "workspaceId": "mux-batched-session",
        }))
        .unwrap(),
    )
    .unwrap();

    let changed =
        import_mux_session_batched(source.clone(), &mut store, &context, &options).unwrap();
    assert_eq!(changed.failed, 0, "{:?}", changed.failures);
    assert_eq!(changed.imported_events, 1);
    let new_partial_event_id = store
        .search_event_hits("changed partial source oracle", 10)
        .unwrap()
        .into_iter()
        .find(|hit| hit.provider == Some(CaptureProvider::Mux))
        .expect("changed partial event should remain identifiable")
        .event_id;
    assert_ne!(new_partial_event_id, old_partial_event_id);

    let after_replacement = store.export_archive().unwrap();
    assert_eq!(
        after_replacement.events.len(),
        before_replacement_event_count + 1
    );
    assert!(after_replacement
        .events
        .iter()
        .any(|event| event.id == old_partial_event_id));
    assert!(after_replacement
        .events
        .iter()
        .any(|event| event.id == new_partial_event_id));

    let event_count = after_replacement.events.len();
    let replay = import_mux_session_batched(source, &mut store, &context, &options).unwrap();
    assert_eq!(replay.imported_events, 0);
    assert_eq!(store.export_archive().unwrap().events.len(), event_count);
}

#[test]
fn bounded_mux_replay_preserves_deterministic_rejection() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let source = write_session(&root, 0, false);
    fs::write(
        source.chat_path.as_ref().unwrap(),
        b"not-json\n{\"id\":\"valid\",\"role\":\"user\",\"parts\":[{\"type\":\"text\",\"text\":\"mux after malformed oracle\"}],\"workspaceId\":\"mux-batched-session\"}\n",
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = test_context(&root);
    let options = test_options();

    let first = import_mux_session_batched(source.clone(), &mut store, &context, &options).unwrap();
    let replay = import_mux_session_batched(source, &mut store, &context, &options).unwrap();

    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(replay.failed, first.failed);
    assert_eq!(replay.failures, first.failures);
    assert_eq!(first.imported_events, 1);
    assert_eq!(replay.skipped_events, 1);
}

#[test]
fn bounded_mux_chat_replays_cumulative_structural_rejection() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let source = write_session(&root, 0, false);
    let mut oversize = vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(1)];
    oversize.push(b'\n');
    fs::write(source.chat_path.as_ref().unwrap(), oversize).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = test_context(&root);
    let options = test_options();

    let first = import_mux_session_batched(source.clone(), &mut store, &context, &options).unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.failures.len(), 1);
    assert_eq!(first.failures[0].line, 1);
    assert_eq!(first.imported, 0);

    let replay = import_mux_session_batched(source, &mut store, &context, &options).unwrap();
    assert_eq!(replay.failed, 1, "{:?}", replay.failures);
    assert_eq!(replay.failures, first.failures);
    assert_eq!(replay.imported, 0);
}

#[test]
fn bounded_mux_chat_structural_rejection_advances_to_later_valid_record() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let source = write_session(&root, 0, false);
    let mut chat = vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(1)];
    chat.extend_from_slice(
        b"\n{\"id\":\"valid\",\"role\":\"user\",\"parts\":[{\"type\":\"text\",\"text\":\"mux after structural rejection oracle\"}],\"workspaceId\":\"mux-batched-session\"}\n",
    );
    fs::write(source.chat_path.as_ref().unwrap(), chat).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = test_context(&root);
    let options = test_options();

    let first = import_mux_session_batched(source.clone(), &mut store, &context, &options).unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.imported_events, 1);
    assert!(store
        .search_event_hits("mux after structural rejection oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Mux)));

    let replay = import_mux_session_batched(source, &mut store, &context, &options).unwrap();
    assert_eq!(replay.failed, 1, "{:?}", replay.failures);
    assert_eq!(replay.skipped_events, 1);
}

#[test]
fn bounded_mux_partial_replays_cumulative_structural_rejection() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let source = write_session(&root, 0, true);
    fs::remove_file(source.chat_path.as_ref().unwrap()).unwrap();
    let source = mux_session_source_from_dir(&source.session_dir)
        .unwrap()
        .unwrap();
    fs::write(
        source.partial_path.as_ref().unwrap(),
        vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(1)],
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = test_context(&root);
    let options = test_options();

    let first = import_mux_session_batched(source.clone(), &mut store, &context, &options).unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.failures.len(), 1);
    assert_eq!(first.failures[0].line, 1);
    assert_eq!(first.imported, 0);

    let replay = import_mux_session_batched(source, &mut store, &context, &options).unwrap();
    assert_eq!(replay.failed, 1, "{:?}", replay.failures);
    assert_eq!(replay.failures, first.failures);
    assert_eq!(replay.imported, 0);
}

#[test]
fn bounded_mux_rejects_excessive_directory_depth() {
    let temp = tempdir().unwrap();
    let mut root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    // Stay below platform PATH_MAX while testing the logical depth guard.
    for _ in 0..=MUX_MAX_DIRECTORY_DEPTH {
        root = root.join("d");
        fs::create_dir(&root).unwrap();
    }
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = test_context(temp.path());
    let error = import_mux_sessions_batched(
        &temp.path().join("sessions"),
        &mut store,
        context,
        test_options(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath {
            reason: "Mux session directory nesting exceeds the supported limit",
            ..
        }
    ));
}
