use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

use serde_json::{json, Value};

use crate::test_support_paths::tempdir;

use super::layout::{
    KimiWireLayout, KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES, KIMI_WIRE_LAYOUT_MAX_INDEX_ENTRIES,
};
use super::*;

fn kimi_wire_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempdir().unwrap();
    let root = temp.path().join(".kimi-code");
    let session_dir = root.join("sessions/work/session-1");
    let agent_dir = session_dir.join("agents/main");
    fs::create_dir_all(&agent_dir).unwrap();
    fs::write(
        root.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": "session-1",
                "sessionDir": session_dir,
                "workDir": "/workspace/kimi"
            })
        ),
    )
    .unwrap();
    fs::write(
        session_dir.join("state.json"),
        json!({
            "createdAt": "2026-07-17T12:00:00Z",
            "updatedAt": "2026-07-17T12:00:10Z",
            "title": "bounded Kimi import",
            "lastPrompt": "checkpoint must not retain this prompt",
            "agents": {"main": {"type": "main"}}
        })
        .to_string(),
    )
    .unwrap();
    let wire = agent_dir.join("wire.jsonl");
    let mut contents = String::from("{\"type\":\"metadata\",\"created_at\":1784289600000}\n");
    for index in 0..65 {
        let record = if index == 0 {
            json!({
                "type": "context.append_loop_event",
                "time": 1_784_289_600_001_i64 + index,
                "event": {
                    "type": "tool.call",
                    "toolName": "Write",
                    "input": {
                        "path": "src/kimi-batch.txt",
                        "content": "persisted touch scope proof"
                    }
                }
            })
        } else {
            json!({
                "type": "turn.prompt",
                "time": 1_784_289_600_001_i64 + index,
                "input": format!("bounded Kimi message {index}")
            })
        };
        contents.push_str(&record.to_string());
        contents.push('\n');
    }
    fs::write(&wire, contents).unwrap();
    (temp, root, wire)
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
fn kimi_wire_batches_resume_append_and_preserve_source_scope() {
    let (temp, root, wire) = kimi_wire_fixture();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "kimi-batch-machine".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root.clone()),
        imported_at: "2026-07-17T12:30:00Z".parse().unwrap(),
    };

    let first =
        import_kimi_wire_jsonl_file_batched(&wire, &mut store, context.clone(), import_options())
            .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 65);

    let source = store
        .capture_source_by_external_session(CaptureProvider::KimiCodeCli, "session-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        Some(wire.to_string_lossy().as_ref())
    );
    assert_eq!(
        source.descriptor.source_root.as_deref(),
        Some(root.to_string_lossy().as_ref())
    );
    let archive = store.export_archive().unwrap();
    let touch = archive
        .files_touched
        .iter()
        .find(|touch| touch.path == "src/kimi-batch.txt")
        .unwrap();
    assert_eq!(
        touch
            .sync
            .metadata
            .get("raw_source_path")
            .and_then(Value::as_str),
        Some(wire.to_string_lossy().as_ref())
    );
    assert_eq!(
        touch
            .sync
            .metadata
            .get("source_root")
            .and_then(Value::as_str),
        Some(root.to_string_lossy().as_ref())
    );

    let replay =
        import_kimi_wire_jsonl_file_batched(&wire, &mut store, context.clone(), import_options())
            .unwrap();
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_sessions, 1);
    assert_eq!(replay.skipped_events, 65);

    let mut file = OpenOptions::new().append(true).open(&wire).unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "type": "context.append_message",
            "time": 1_784_289_700_000_i64,
            "message": {"role": "assistant", "content": "appended Kimi answer"}
        })
    )
    .unwrap();
    drop(file);

    let append =
        import_kimi_wire_jsonl_file_batched(&wire, &mut store, context, import_options()).unwrap();
    assert_eq!(append.failed, 0, "{:?}", append.failures);
    assert_eq!(append.imported_sessions, 0);
    assert_eq!(append.imported_events, 1);

    let cursor_path = provider_path_identity(&wire).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        &cursor_path,
    );
    let cursor = store
        .get_sync_cursor(None, "kimi-batch-machine", &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&cursor.cursor).unwrap();
    let checkpoint: KimiParserCheckpoint = certified.parser_checkpoint().deserialize().unwrap();
    assert_eq!(checkpoint.next_ordinal, 67);
    assert_eq!(checkpoint.accepted_events, 66);
    assert_eq!(checkpoint.accepted_file_touches, 1);
    assert!(checkpoint.emitted_session);
    assert_eq!(KIMI_CAPTURE_REVISION, 4);
    assert_eq!(KIMI_POLICY_REVISION, 6);
    assert_eq!(certified.parser_revision(), KIMI_CAPTURE_REVISION);
    assert!(certified
        .source_revision()
        .starts_with("kimi-wire-jsonl-v3:"));
    assert_eq!(certified.rejected_records(), 0);
    let checkpoint_bytes = certified.parser_checkpoint().as_bytes();
    assert!(checkpoint_bytes.len() < 1024);
    let checkpoint_text = String::from_utf8_lossy(checkpoint_bytes);
    assert!(!checkpoint_text.contains("bounded Kimi import"));
    assert!(!checkpoint_text.contains("checkpoint must not retain this prompt"));
    assert!(!checkpoint_text.contains("persisted touch scope proof"));
    assert!(!checkpoint_text.contains("/workspace/kimi"));
    assert!(!checkpoint_text.contains(wire.to_string_lossy().as_ref()));
    assert!(!checkpoint_text.contains(root.to_string_lossy().as_ref()));
}

#[test]
fn kimi_cursor_resets_when_admission_source_root_changes() {
    let (temp, root, wire) = kimi_wire_fixture();
    let alternate_root = temp.path().join("alternate-kimi-root");
    fs::create_dir_all(&alternate_root).unwrap();
    let mut store = Store::open(temp.path().join("scope.sqlite")).unwrap();
    let context_for = |source_root: Option<PathBuf>| ProviderAdapterContext {
        machine_id: "kimi-scope-machine".to_owned(),
        source_path: Some(wire.clone()),
        source_root,
        imported_at: "2026-07-17T12:30:00Z".parse().unwrap(),
    };
    let cursor_path = provider_path_identity(&wire).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        &cursor_path,
    );

    let first = import_kimi_wire_jsonl_file_batched(
        &wire,
        &mut store,
        context_for(Some(root.clone())),
        import_options(),
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_events, 65);
    let first_cursor = store
        .get_sync_cursor(None, "kimi-scope-machine", &stream)
        .unwrap()
        .unwrap();
    let first_certified = CertifiedProviderCursor::decode(&first_cursor.cursor).unwrap();

    let changed_root = import_kimi_wire_jsonl_file_batched(
        &wire,
        &mut store,
        context_for(Some(alternate_root.clone())),
        import_options(),
    )
    .unwrap();
    assert_eq!(changed_root.failed, 0, "{:?}", changed_root.failures);
    assert_eq!(changed_root.imported_sessions, 0);
    assert!(changed_root.skipped_sessions > 0);
    assert_eq!(changed_root.imported_events, 0);
    assert_eq!(changed_root.skipped_events, 65);
    let changed_cursor = store
        .get_sync_cursor(None, "kimi-scope-machine", &stream)
        .unwrap()
        .unwrap();
    let changed_certified = CertifiedProviderCursor::decode(&changed_cursor.cursor).unwrap();
    assert_eq!(changed_cursor.id, first_cursor.id);
    assert_eq!(changed_cursor.stream, first_cursor.stream);
    assert_ne!(
        changed_certified.source_revision(),
        first_certified.source_revision()
    );
    let changed_archive = store.export_archive().unwrap();
    assert!(changed_archive.capture_sources.iter().any(|source| source
        .descriptor
        .source_root
        .as_deref()
        == Some(alternate_root.to_string_lossy().as_ref())
        && source.descriptor.raw_source_path.as_deref() == Some(wire.to_string_lossy().as_ref())));
    let changed_touch = changed_archive
        .files_touched
        .iter()
        .find(|touch| touch.path == "src/kimi-batch.txt")
        .unwrap();
    assert_eq!(
        changed_touch
            .sync
            .metadata
            .get("source_root")
            .and_then(Value::as_str),
        Some(alternate_root.to_string_lossy().as_ref())
    );

    let mut direct_store = Store::open(temp.path().join("direct-to-tree.sqlite")).unwrap();
    let direct = import_kimi_wire_jsonl_file_batched(
        &wire,
        &mut direct_store,
        context_for(None),
        import_options(),
    )
    .unwrap();
    assert_eq!(direct.failed, 0, "{:?}", direct.failures);
    assert_eq!(direct.imported_events, 65);
    let tree = import_kimi_wire_jsonl_tree_batched(
        &root,
        &mut direct_store,
        context_for(Some(root.clone())),
        import_options(),
    )
    .unwrap();
    assert_eq!(tree.failed, 0, "{:?}", tree.failures);
    assert_eq!(tree.imported_sessions, 0);
    assert!(tree.skipped_sessions > 0);
    assert_eq!(tree.imported_events, 0);
    assert_eq!(tree.skipped_events, 65);
}

#[test]
fn kimi_all_malformed_replay_retains_rejections_without_scaffolding() {
    let (temp, root, wire) = kimi_wire_fixture();
    fs::write(&wire, b"{not-json\n[still-not-json\n").unwrap();
    let mut store = Store::open(temp.path().join("malformed.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "kimi-malformed-machine".to_owned(),
        source_path: Some(wire.clone()),
        source_root: Some(root),
        imported_at: "2026-07-17T12:30:00Z".parse().unwrap(),
    };

    let first =
        import_kimi_wire_jsonl_file_batched(&wire, &mut store, context.clone(), import_options())
            .unwrap();
    assert_eq!(first.failed, 2);
    assert_eq!(first.failures.len(), 2);
    assert_eq!(first.imported_sessions, 0);
    assert_eq!(first.imported_events, 0);
    let archive = store.export_archive().unwrap();
    assert!(archive.capture_sources.is_empty());
    assert!(archive.sessions.is_empty());
    assert!(archive.events.is_empty());
    assert!(archive.files_touched.is_empty());

    let replay =
        import_kimi_wire_jsonl_file_batched(&wire, &mut store, context, import_options()).unwrap();
    assert_eq!(replay.failed, 2);
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.skipped_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_events, 0);
    assert!(store.export_archive().unwrap().sessions.is_empty());

    let cursor_path = provider_path_identity(&wire).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        &cursor_path,
    );
    let cursor = store
        .get_sync_cursor(None, "kimi-malformed-machine", &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&cursor.cursor).unwrap();
    let checkpoint: KimiParserCheckpoint = certified.parser_checkpoint().deserialize().unwrap();
    assert_eq!(certified.rejected_records(), 2);
    assert!(!checkpoint.emitted_session);
    assert_eq!(checkpoint.accepted_events, 0);
    assert_eq!(checkpoint.accepted_file_touches, 0);
}

#[test]
fn kimi_wire_observation_detects_auxiliary_state_changes() {
    let (_temp, _root, wire) = kimi_wire_fixture();
    let observation = KimiWireObservation::read(&wire).unwrap();
    assert!(observation.revalidate(&wire).unwrap());
    let session_dir = wire.parent().unwrap().parent().unwrap().parent().unwrap();
    fs::write(
        session_dir.join("state.json"),
        json!({
            "createdAt": "2026-07-17T12:00:00Z",
            "title": "changed state",
            "agents": {"main": {"type": "main"}}
        })
        .to_string(),
    )
    .unwrap();
    assert!(!observation.revalidate(&wire).unwrap());
}

#[test]
fn kimi_layout_derives_one_exact_root_despite_malicious_nesting() {
    let (temp, root, wire) = kimi_wire_fixture();
    let session_dir = wire.parent().unwrap().parent().unwrap().parent().unwrap();
    fs::write(
        session_dir.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({"sessionId": "session-1", "workDir": "/malicious/nearby"})
        ),
    )
    .unwrap();

    let mut layout = KimiWireLayout::read(&wire).unwrap();
    assert_eq!(
        layout.take_index_entry().unwrap().work_dir.as_deref(),
        Some("/workspace/kimi")
    );

    let invalid_wire = temp
        .path()
        .join("not-sessions/work/session-1/agents/main/wire.jsonl");
    fs::create_dir_all(invalid_wire.parent().unwrap()).unwrap();
    fs::write(&invalid_wire, "{}\n").unwrap();
    assert!(matches!(
        KimiWireLayout::read(&invalid_wire),
        Err(CaptureError::InvalidProviderTranscriptPath { .. })
    ));
    assert!(root.join("session_index.jsonl").is_file());
}

#[test]
fn kimi_layout_preserves_first_matching_index_entry_order() {
    let (_temp, root, wire) = kimi_wire_fixture();
    fs::write(
        root.join("session_index.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"sessionId": "session-1", "workDir": "/first"}),
            json!({"sessionId": "session-1", "workDir": "/second"}),
        ),
    )
    .unwrap();

    let mut layout = KimiWireLayout::read(&wire).unwrap();
    assert_eq!(
        layout.take_index_entry().unwrap().work_dir.as_deref(),
        Some("/first")
    );
}

#[test]
fn kimi_layout_accepts_exact_aggregate_byte_limit() {
    let (_temp, root, wire) = kimi_wire_fixture();
    let index_path = root.join("session_index.jsonl");
    OpenOptions::new()
        .write(true)
        .open(&index_path)
        .unwrap()
        .set_len(KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES as u64)
        .unwrap();

    let mut layout = KimiWireLayout::read(&wire).unwrap();
    assert_eq!(
        layout.take_index_entry().unwrap().work_dir.as_deref(),
        Some("/workspace/kimi")
    );
}

#[test]
fn kimi_layout_rejects_oversized_index_before_import_writes() {
    let (temp, root, wire) = kimi_wire_fixture();
    let index_path = root.join("session_index.jsonl");
    OpenOptions::new()
        .write(true)
        .open(&index_path)
        .unwrap()
        .set_len(KIMI_WIRE_LAYOUT_MAX_AGGREGATE_BYTES as u64 + 1)
        .unwrap();
    let error = KimiWireLayout::read(&wire).unwrap_err();
    assert!(error.to_string().contains("16777216-byte layout limit"));

    let mut store = Store::open(temp.path().join("oversized.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "kimi-oversized-machine".to_owned(),
        source_path: Some(wire.clone()),
        source_root: Some(root),
        imported_at: "2026-07-17T12:30:00Z".parse().unwrap(),
    };
    let error = import_kimi_wire_jsonl_file_batched(&wire, &mut store, context, import_options())
        .unwrap_err();
    assert!(error.to_string().contains("16777216-byte layout limit"));
    let archive = store.export_archive().unwrap();
    assert!(archive.capture_sources.is_empty());
    assert!(archive.sessions.is_empty());
    assert!(archive.events.is_empty());
    assert!(archive.files_touched.is_empty());
}

#[test]
fn kimi_layout_enforces_entry_boundary_without_partial_positive() {
    let (_temp, root, wire) = kimi_wire_fixture();
    let index_path = root.join("session_index.jsonl");
    let mut index = fs::read(&index_path).unwrap();
    index.extend(std::iter::repeat_n(
        b'\n',
        KIMI_WIRE_LAYOUT_MAX_INDEX_ENTRIES - 1,
    ));
    fs::write(&index_path, &index).unwrap();

    let mut layout = KimiWireLayout::read(&wire).unwrap();
    assert_eq!(
        layout.take_index_entry().unwrap().work_dir.as_deref(),
        Some("/workspace/kimi")
    );

    index.push(b'\n');
    fs::write(&index_path, index).unwrap();
    let error = KimiWireLayout::read(&wire).unwrap_err();
    assert!(error.to_string().contains("65536-entry layout limit"));
}
