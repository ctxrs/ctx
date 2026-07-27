use std::{ffi::OsStr, fs, fs::OpenOptions, io::Write};

use crate::captured_batch::{
    CapturedBatch, CapturedBatchBuilder, NativeLocator, NativePosition,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::test_support_paths::tempdir;

use super::*;

#[test]
fn antigravity_and_windsurf_formats_are_call_only_not_result_streams() {
    const FIXTURES: &[(CaptureProvider, &str)] = &[
        (
            CaptureProvider::Antigravity,
            include_str!(
                "../../../../../../tests/fixtures/provider-history/antigravity/v1/brain/agy-success/.system_generated/logs/transcript_full.jsonl"
            ),
        ),
        (
            CaptureProvider::Windsurf,
            include_str!(
                "../../../../../../tests/fixtures/provider-history/windsurf/transcripts/windsurf-hook-trajectory-1.jsonl"
            ),
        ),
    ];

    for (provider, fixture) in FIXTURES {
        let event_types = fixture
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .map(|value| native_jsonl_event_type(*provider, &value))
            .collect::<Vec<_>>();
        assert!(event_types.contains(&EventType::ToolCall));
        assert!(!event_types.iter().any(|event_type| matches!(
            event_type,
            EventType::ToolOutput | EventType::CommandOutput
        )));
    }
}

#[test]
fn cursor_tool_result_retains_only_artifact_identifiers() {
    let event = native_jsonl_event(
            CaptureProvider::Cursor,
            crate::CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
            &json!({
                "id": "cursor-result-1",
                "role": "user",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "tool-1",
                        "content": "created commit 0123456789abcdef0123456789abcdef01234567 https://github.com/ctxrs/ctx/pull/456?token=secret#fragment cursor-result-prose"
                    }]
                }
            }),
            1,
            "2026-07-21T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
    assert_eq!(event.event_type, EventType::ToolOutput);
    let rendered = event.payload.to_string();
    let evidence = event
        .payload
        .get("result_evidence")
        .and_then(Value::as_array)
        .unwrap();
    assert!(evidence.iter().any(|item| {
        item.get("kind").and_then(Value::as_str) == Some("call_id")
            && item.get("value").and_then(Value::as_str) == Some("tool-1")
    }));
    assert!(rendered.contains("0123456789abcdef0123456789abcdef01234567"));
    assert!(rendered.contains("https://github.com/ctxrs/ctx/pull/456"));
    assert!(rendered.contains("tool-1"));
    assert!(!rendered.contains("token=secret"));
    assert!(!rendered.contains("fragment"));
    assert!(!rendered.contains("cursor-result-prose"));
    assert_eq!(event.payload["result_outcome"], Value::Null);
}

fn test_context() -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "native-jsonl-batch-test-machine".to_owned(),
        source_path: Some("/tmp/native-jsonl-batch-test/events.jsonl".into()),
        source_root: Some("/tmp/native-jsonl-batch-test".into()),
        imported_at: "2026-07-17T12:00:00Z".parse().unwrap(),
    }
}

fn test_source(length: usize) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        "native-jsonl-file:/tmp/native-jsonl-batch-test/events.jsonl",
        format!("test-revision:{length}"),
        "provider:copilot_cli:copilot_cli_events_jsonl:source:test",
        NATIVE_JSONL_CAPTURE_REVISION,
        NATIVE_JSONL_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn test_position(offset: u64) -> NativePosition {
    NativePosition::new(
        "native-jsonl-test-position-v1",
        offset.to_be_bytes().to_vec(),
    )
    .unwrap()
}

fn test_record(ordinal: u64, bytes: impl AsRef<[u8]>) -> CapturedRecord {
    let payload = bytes.as_ref().to_vec();
    let source_item = b"native-jsonl-test-source";
    let start = ordinal.saturating_mul(1_024);
    let end = start
        .saturating_add(u64::try_from(payload.len()).unwrap())
        .saturating_add(1);
    let mut locator = Vec::new();
    locator.extend_from_slice(&u32::try_from(source_item.len()).unwrap().to_be_bytes());
    locator.extend_from_slice(source_item);
    locator.extend_from_slice(&start.to_be_bytes());
    locator.extend_from_slice(&end.to_be_bytes());
    CapturedRecord::content(
        ordinal,
        NativeLocator::new(NATIVE_JSONL_LOCATOR_KIND, locator).unwrap(),
        ProviderRecordKind::new(native_jsonl_record_kind(
            CaptureProvider::CopilotCli,
            crate::COPILOT_CLI_SOURCE_FORMAT,
        ))
        .unwrap(),
        payload,
    )
    .unwrap()
}

fn test_batch(records: Vec<CapturedRecord>) -> CapturedBatch {
    let end = records
        .last()
        .map_or(0, |record| record.ordinal().saturating_add(1));
    let mut builder = CapturedBatchBuilder::new(test_source(end as usize), test_position(0));
    for record in records {
        builder.push(record).unwrap();
    }
    builder.finish(test_position(end)).unwrap()
}

#[derive(Default)]
struct TestProjectionOutput {
    normalizations: Vec<ProviderNormalizationResult>,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for TestProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        self.normalizations.push(normalization);
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.rejections.push((line_number, reason));
    }
}

fn project(
    projector: &mut NativeJsonlCapturedBatchProjector,
    record: &CapturedRecord,
) -> TestProjectionOutput {
    let mut output = TestProjectionOutput::default();
    projector.project_record(record, &mut output).unwrap();
    output
}

#[cfg(unix)]
#[test]
fn tree_rejects_selected_symlink_and_leaves_store_empty() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let root = temp.path().join("transcripts");
    fs::create_dir_all(&root).unwrap();
    let transcript = concat!(
        r#"{"id":"copilot-start","timestamp":"2026-07-17T12:00:00Z","type":"session.start","data":{"sessionId":"copilot-tree-symlink","startTime":"2026-07-17T12:00:00Z","context":{"cwd":"/workspace"}}}"#,
        "\n",
    );
    let symlink_target = temp.path().join("symlink-target.jsonl");
    fs::write(&symlink_target, transcript).unwrap();
    let symlink_path = root.join("events.jsonl");
    symlink(&symlink_target, &symlink_path).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let error = import_native_jsonl_tree_batched(
        &root,
        &mut store,
        ProviderAdapterContext {
            machine_id: "native-jsonl-tree-symlink-machine".to_owned(),
            source_path: Some(root.clone()),
            source_root: Some(root.clone()),
            imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
        },
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path == symlink_path
                && reason == "symlinked provider transcript files are rejected"
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn tree_visitation_is_sorted_by_durable_filename_bytes() {
    fn visited_agents(root: &Path, creation_order: &[&str]) -> Vec<String> {
        for agent in creation_order {
            let directory = root.join("sessions/work/session/agents").join(agent);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("wire.jsonl"), b"\n").unwrap();
        }

        let mut visited = Vec::new();
        visit_native_jsonl_files(root, CaptureProvider::KimiCodeCli, &mut |path| {
            visited.push(
                path.parent()
                    .and_then(Path::file_name)
                    .and_then(OsStr::to_str)
                    .unwrap()
                    .to_owned(),
            );
            Ok(())
        })
        .unwrap();
        visited
    }

    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    let expected = vec!["agent-1".to_owned(), "main".to_owned()];
    assert_eq!(visited_agents(first.path(), &["main", "agent-1"]), expected);
    assert_eq!(
        visited_agents(second.path(), &["agent-1", "main"]),
        expected
    );
}

#[test]
fn wide_tree_visitation_is_single_scan_bounded_and_globally_sorted() {
    const ENTRY_COUNT: usize = 1_025;

    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let mut expected = (0..ENTRY_COUNT)
        .map(|index| format!("session-{index:04}.jsonl"))
        .collect::<Vec<_>>();
    for name in expected.iter().rev() {
        fs::write(root.join(name), b"\n").unwrap();
    }
    expected.sort();

    let mut visited = Vec::new();
    let (result, stats) = traversal::count_native_jsonl_traversal_work(|| {
        visit_native_jsonl_files(&root, CaptureProvider::Codex, &mut |path| {
            visited.push(path.file_name().unwrap().to_str().unwrap().to_owned());
            Ok(())
        })
    });
    assert_eq!(result.unwrap(), ENTRY_COUNT);
    assert_eq!(visited, expected);
    assert_eq!(stats.directory_read_passes, 1);
    assert_eq!(stats.directory_entries_read, ENTRY_COUNT);
    assert_eq!(stats.max_retained_names, 64);
    assert_eq!(stats.initial_runs, 17);
    assert_eq!(stats.max_merge_readers, 16);
    assert_eq!(stats.merge_names_read, ENTRY_COUNT * 2);
    assert_eq!(stats.final_names_read, ENTRY_COUNT);
}

#[test]
fn checkpoint_retains_compact_session_seed_and_resumes_with_exact_projection() {
    let provider_content = "checkpoint-must-not-retain-this-provider-content";
    let context = test_context();
    let path = context.source_path.clone().unwrap();
    let header = json!({
        "id": "copilot-start",
        "timestamp": "2026-07-17T12:00:00Z",
        "type": "session.start",
        "data": {
            "sessionId": "copilot-resume",
            "startTime": "2026-07-17T12:00:00Z",
            "context": { "cwd": "/workspace" },
        },
        "providerContent": provider_content,
    });
    let expected_session_metadata = native_jsonl_session_metadata_from_normalized_header(
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        &native_jsonl_normalized_header_metadata(&header),
        &path,
    );
    let first_batch = test_batch(vec![
            test_record(0, serde_json::to_vec(&header).unwrap()),
            test_record(
                1,
                br#"{"id":"copilot-first","timestamp":"2026-07-17T12:00:01Z","type":"user.message","data":{"content":"first"}}"#,
            ),
        ]);
    let appended = test_record(
            2,
            br#"{"id":"copilot-appended","timestamp":"2026-07-17T12:00:02Z","type":"assistant.message","data":{"content":"appended"}}"#,
        );
    let mut prefix_projector = NativeJsonlCapturedBatchProjector::fresh(
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        &path,
        context.clone(),
    );
    for record in first_batch.records() {
        let output = project(&mut prefix_projector, record);
        assert_eq!(output.normalizations.len(), 1);
        assert!(output.rejections.is_empty());
    }

    let CapturedBatchCursorFinish::Advance(cursor) =
        prefix_projector.finish_cursor(&first_batch).unwrap()
    else {
        panic!("native JSONL cursor must advance at a captured batch boundary");
    };
    let checkpoint_bytes = cursor.parser_checkpoint().as_bytes();
    assert!(!String::from_utf8_lossy(checkpoint_bytes).contains(provider_content));
    let checkpoint_value: Value = serde_json::from_slice(checkpoint_bytes).unwrap();
    assert!(checkpoint_value
        .pointer("/session/header_preview")
        .is_none());
    assert!(checkpoint_value
        .pointer("/session/normalized_header_metadata")
        .is_none());
    let checkpoint: NativeJsonlParserCheckpoint = cursor.parser_checkpoint().deserialize().unwrap();
    let session = checkpoint.session.unwrap();
    assert_eq!(session.native_session_id, "copilot-resume");
    assert_eq!(session.status, SessionStatus::Imported);
    assert_eq!(session.cwd.as_deref(), Some("/workspace"));

    let decoded = CertifiedProviderCursor::decode(&cursor.encode().unwrap()).unwrap();
    assert_eq!(decoded, cursor);
    let mut resumed = NativeJsonlCapturedBatchProjector::resume(
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        &path,
        context.clone(),
        &decoded,
        Some(native_jsonl_normalized_header_metadata(&header)),
    )
    .unwrap();
    let resumed_output = project(&mut resumed, &appended);
    assert!(resumed_output.rejections.is_empty());

    let mut one_shot = NativeJsonlCapturedBatchProjector::fresh(
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        &path,
        context,
    );
    for record in first_batch.records() {
        let output = project(&mut one_shot, record);
        assert!(output.rejections.is_empty());
    }
    let one_shot_output = project(&mut one_shot, &appended);
    assert!(one_shot_output.rejections.is_empty());
    assert_eq!(resumed_output.normalizations.len(), 2);
    assert_eq!(one_shot_output.normalizations.len(), 1);
    assert_eq!(resumed_output.normalizations[0].captures[0].0, 1);
    assert!(resumed_output.normalizations[0].captures[0]
        .1
        .event
        .is_none());
    assert_eq!(
        resumed_output.normalizations[1].captures,
        one_shot_output.normalizations[0].captures
    );
    assert_eq!(
        resumed_output.normalizations[1].files_touched,
        one_shot_output.normalizations[0].files_touched
    );
    let resumed_session_metadata = &resumed_output.normalizations[1].captures[0]
        .1
        .session
        .metadata;
    assert_eq!(resumed_session_metadata, &expected_session_metadata);
    assert_eq!(
        one_shot_output.normalizations[0].captures[0]
            .1
            .session
            .metadata,
        expected_session_metadata
    );
    assert!(resumed_session_metadata["header"]
        .to_string()
        .contains(provider_content));
}

#[test]
fn append_resume_matches_one_shot_store_state_and_header_metadata() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    let header = json!({
        "id": "copilot-store-start",
        "timestamp": "2026-07-17T12:00:00Z",
        "type": "session.start",
        "data": {
            "sessionId": "copilot-store-resume",
            "startTime": "2026-07-17T12:00:00Z",
            "context": { "cwd": "/workspace" },
        },
        "providerContent": "normalized-store-retains-this-header-content",
    });
    let mut prefix = serde_json::to_vec(&header).unwrap();
    prefix.push(b'\n');
    prefix.extend_from_slice(
            concat!(
                r#"{"id":"copilot-store-first","timestamp":"2026-07-17T12:00:01Z","type":"user.message","data":{"content":"first"}}"#,
                "\n",
            )
            .as_bytes(),
        );
    fs::write(&path, prefix).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "native-jsonl-store-parity-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: Some(path.clone()),
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };
    let mut resumed_store = Store::open(temp.path().join("resumed.sqlite")).unwrap();
    let first = import_native_jsonl_file_batched(
        &path,
        &mut resumed_store,
        context.clone(),
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.imported_events, 2);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(
            concat!(
                r#"{"id":"copilot-store-appended","timestamp":"2026-07-17T12:00:02Z","type":"assistant.message","data":{"content":"appended"}}"#,
                "\n",
            )
            .as_bytes(),
        )
        .unwrap();
    file.sync_all().unwrap();
    let second = import_native_jsonl_file_batched(
        &path,
        &mut resumed_store,
        context.clone(),
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(second.imported_events, 1);

    let mut one_shot_store = Store::open(temp.path().join("one-shot.sqlite")).unwrap();
    let one_shot = import_native_jsonl_file_batched(
        &path,
        &mut one_shot_store,
        context,
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(one_shot.imported_events, 3);

    let resumed_session = resumed_store
        .session_by_external_session(CaptureProvider::CopilotCli, "copilot-store-resume")
        .unwrap()
        .unwrap();
    let one_shot_session = one_shot_store
        .session_by_external_session(CaptureProvider::CopilotCli, "copilot-store-resume")
        .unwrap()
        .unwrap();
    assert_eq!(resumed_session, one_shot_session);
    assert_eq!(
        resumed_store
            .events_for_session(resumed_session.id)
            .unwrap(),
        one_shot_store
            .events_for_session(one_shot_session.id)
            .unwrap()
    );
    assert_eq!(
        resumed_session.sync.metadata["metadata"],
        native_jsonl_session_metadata_from_normalized_header(
            CaptureProvider::CopilotCli,
            crate::COPILOT_CLI_SOURCE_FORMAT,
            &native_jsonl_normalized_header_metadata(&header),
            &path,
        )
    );
    assert!(resumed_session.sync.metadata["metadata"]["header"]
        .to_string()
        .contains("normalized-store-retains-this-header-content"));
}

#[cfg(unix)]
#[test]
fn observation_token_reconciles_same_stat_rewrite_and_preserves_append_resume() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    let header = |marker: &str| {
        format!(
            r#"{{"id":"observed-start","timestamp":"2026-07-17T12:00:00Z","type":"session.start","data":{{"sessionId":"observed-session","startTime":"2026-07-17T12:00:00Z"}},"providerContent":"{marker}"}}"#
        )
    };
    let original_header = header("same-size-a");
    let rewritten_header = header("same-size-b");
    assert_eq!(original_header.len(), rewritten_header.len());
    let filler = json!({
        "id": "observed-filler",
        "timestamp": "2026-07-17T12:00:01Z",
        "type": "user.message",
        "data": { "content": "x".repeat(70 * 1024) },
    });
    let mut source = original_header.into_bytes();
    source.push(b'\n');
    source.extend_from_slice(&serde_json::to_vec(&filler).unwrap());
    source.push(b'\n');
    fs::write(&path, source).unwrap();
    let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
    let context = ProviderAdapterContext {
        machine_id: "native-jsonl-observation-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: Some(path.clone()),
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first_observation = crate::observe_ordinary_file(&path).unwrap();
    let options = |token: String| NormalizedProviderImportOptions {
        inventory_observation_token: Some(token),
        ..NormalizedProviderImportOptions::default()
    };
    let first = import_native_jsonl_file_batched(
        &path,
        &mut store,
        context.clone(),
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        options(first_observation.token_hex()),
    )
    .unwrap();
    assert_eq!(first.imported_events, 2);

    let unchanged = import_native_jsonl_file_batched(
        &path,
        &mut store,
        context.clone(),
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        options(first_observation.token_hex()),
    )
    .unwrap();
    assert_eq!(unchanged.imported_events, 0);

    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(rewritten_header.as_bytes()).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(original_modified))
        .unwrap();
    file.sync_all().unwrap();
    drop(file);
    let rewritten_observation = crate::observe_ordinary_file(&path).unwrap();
    assert_eq!(rewritten_observation.len(), first_observation.len());
    assert_eq!(
        rewritten_observation.modified_at(),
        first_observation.modified_at()
    );
    assert_ne!(rewritten_observation.token(), first_observation.token());

    import_native_jsonl_file_batched(
        &path,
        &mut store,
        context.clone(),
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        options(rewritten_observation.token_hex()),
    )
    .unwrap();
    let session = store
        .session_by_external_session(CaptureProvider::CopilotCli, "observed-session")
        .unwrap()
        .unwrap();
    assert!(session.sync.metadata["metadata"]["header"]
        .to_string()
        .contains("same-size-b"));
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(
            concat!(
                r#"{"id":"observed-appended","timestamp":"2026-07-17T12:00:02Z","type":"assistant.message","data":{"content":"appended"}}"#,
                "\n",
            )
            .as_bytes(),
        )
        .unwrap();
    file.sync_all().unwrap();
    let appended_observation = crate::observe_ordinary_file(&path).unwrap();
    let appended = import_native_jsonl_file_batched(
        &path,
        &mut store,
        context,
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        options(appended_observation.token_hex()),
    )
    .unwrap();
    assert_eq!(appended.imported_events, 1);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 3);
}

#[test]
fn resume_header_anchor_is_scoped_to_the_exact_source_with_duplicate_session_ids() {
    let temp = tempdir().unwrap();
    let root_a = temp.path().join("root-a");
    let root_b = temp.path().join("root-b");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();
    let path_a = root_a.join("events.jsonl");
    let path_b = root_b.join("events.jsonl");
    let transcript = |marker: &str, event_id: &str| {
        format!(
            concat!(
                r#"{{"id":"start-{event_id}","timestamp":"2026-07-17T12:00:00Z","type":"session.start","data":{{"sessionId":"duplicate-session","startTime":"2026-07-17T12:00:00Z"}},"providerContent":"{marker}"}}"#,
                "\n",
                r#"{{"id":"{event_id}","timestamp":"2026-07-17T12:00:01Z","type":"user.message","data":{{"content":"{marker}"}}}}"#,
                "\n",
            ),
            event_id = event_id,
            marker = marker,
        )
    };
    fs::write(&path_a, transcript("root-a-header", "event-a")).unwrap();
    fs::write(&path_b, transcript("root-b-header", "event-b")).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = |path: &Path, root: &Path| ProviderAdapterContext {
        machine_id: "native-jsonl-duplicate-session-machine".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: Some(root.to_path_buf()),
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };
    import_native_jsonl_file_batched(
        &path_a,
        &mut store,
        context(&path_a, &root_a),
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    import_native_jsonl_file_batched(
        &path_b,
        &mut store,
        context(&path_b, &root_b),
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    let mut file = OpenOptions::new().append(true).open(&path_a).unwrap();
    file.write_all(
            concat!(
                r#"{"id":"event-a-appended","timestamp":"2026-07-17T12:00:02Z","type":"assistant.message","data":{"content":"appended"}}"#,
                "\n",
            )
            .as_bytes(),
        )
        .unwrap();
    file.sync_all().unwrap();
    import_native_jsonl_file_batched(
        &path_a,
        &mut store,
        context(&path_a, &root_a),
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    let session = store
        .session_by_external_session(CaptureProvider::CopilotCli, "duplicate-session")
        .unwrap()
        .unwrap();
    let metadata = session.sync.metadata["metadata"]["header"].to_string();
    assert!(metadata.contains("root-a-header"));
    assert!(!metadata.contains("root-b-header"));
}

#[test]
fn append_resume_resets_and_replays_a_rewritten_header_anchor() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    let header = |marker: &str| {
        format!(
            r#"{{"id":"rewrite-start","timestamp":"2026-07-17T12:00:00Z","type":"session.start","data":{{"sessionId":"rewrite-session","startTime":"2026-07-17T12:00:00Z"}},"providerContent":"{marker}"}}"#
        )
    };
    let original_header = header("old-anchor-a");
    let rewritten_header = header("old-anchor-b");
    assert_eq!(original_header.len(), rewritten_header.len());
    let filler = json!({
        "id": "rewrite-filler",
        "timestamp": "2026-07-17T12:00:01Z",
        "type": "user.message",
        "data": { "content": "x".repeat(70 * 1024) },
    });
    let mut source = original_header.into_bytes();
    source.push(b'\n');
    source.extend_from_slice(&serde_json::to_vec(&filler).unwrap());
    source.push(b'\n');
    fs::write(&path, source).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "native-jsonl-rewrite-anchor-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: Some(path.clone()),
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    import_native_jsonl_file_batched(
        &path,
        &mut store,
        context.clone(),
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(rewritten_header.as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(
            concat!(
                r#"{"id":"rewrite-appended","timestamp":"2026-07-17T12:00:02Z","type":"assistant.message","data":{"content":"appended"}}"#,
                "\n",
            )
            .as_bytes(),
        )
        .unwrap();
    file.sync_all().unwrap();

    import_native_jsonl_file_batched(
        &path,
        &mut store,
        context,
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    let session = store
        .session_by_external_session(CaptureProvider::CopilotCli, "rewrite-session")
        .unwrap()
        .unwrap();
    assert!(session.sync.metadata["metadata"]["header"]
        .to_string()
        .contains("old-anchor-b"));
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 3);
}

#[test]
fn structural_rejection_remains_failed_on_unchanged_replay() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    let header = concat!(
        r#"{"id":"copilot-start","timestamp":"2026-07-17T12:00:00Z","type":"session.start","data":{"sessionId":"copilot-structural-rejection","startTime":"2026-07-17T12:00:00Z","context":{"cwd":"/workspace"}}}"#,
        "\n",
    );
    let mut source = Vec::with_capacity(
        header
            .len()
            .saturating_add(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
            .saturating_add(2),
    );
    source.extend_from_slice(header.as_bytes());
    source.resize(
        source
            .len()
            .saturating_add(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
            .saturating_add(1),
        b'x',
    );
    source.push(b'\n');
    fs::write(&path, source).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "native-jsonl-structural-rejection-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: Some(path.clone()),
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };

    let first = import_native_jsonl_file_batched(
        &path,
        &mut store,
        context.clone(),
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.imported_events, 1);
    assert_eq!(first.failed, 1);

    let second = import_native_jsonl_file_batched(
        &path,
        &mut store,
        ProviderAdapterContext {
            imported_at: "2026-07-17T12:02:00Z".parse().unwrap(),
            ..context
        },
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(second.skipped_events, 1);
    assert_eq!(second.failed, 1);
}

#[test]
fn tool_only_file_is_read_once_and_converges_from_the_certified_cursor() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("events.jsonl");
    fs::write(
            &path,
            concat!(
                r#"{"id":"copilot-start","timestamp":"2026-07-17T12:00:00Z","type":"session.start","data":{"sessionId":"copilot-tool-only","startTime":"2026-07-17T12:00:00Z","context":{"cwd":"/workspace"}}}"#,
                "\n",
                r#"{"id":"copilot-tool","timestamp":"2026-07-17T12:00:01Z","type":"tool.execution_start","data":{"toolCallId":"tool-1","toolName":"bash"}}"#,
                "\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "native-jsonl-tool-only-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: Some(path.clone()),
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };

    let (first, first_source_opens) = count_native_jsonl_source_file_opens(|| {
        import_native_jsonl_file_batched(
            &path,
            &mut store,
            context.clone(),
            CaptureProvider::CopilotCli,
            crate::COPILOT_CLI_SOURCE_FORMAT,
            NormalizedProviderImportOptions::default(),
        )
    });
    let first = first.unwrap();
    assert_eq!(first_source_opens, 1);
    assert_eq!(first.imported_events, 2);
    assert_eq!(first.failed, 0);

    let session = store
        .session_by_external_session(CaptureProvider::CopilotCli, "copilot-tool-only")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall));

    let (second, second_source_opens) = count_native_jsonl_source_file_opens(|| {
        import_native_jsonl_file_batched(
            &path,
            &mut store,
            ProviderAdapterContext {
                imported_at: "2026-07-17T12:02:00Z".parse().unwrap(),
                ..context
            },
            CaptureProvider::CopilotCli,
            crate::COPILOT_CLI_SOURCE_FORMAT,
            NormalizedProviderImportOptions::default(),
        )
    });
    let second = second.unwrap();
    assert_eq!(second_source_opens, 2);
    assert_eq!(second.skipped_events, 2);
    assert_eq!(second.failed, 0);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2);
}
