use std::{fs::OpenOptions, io::Write, path::PathBuf};

use crate::captured_batch::{CapturedBatchBuilder, NativeLocator, NativePosition};
use crate::test_support_paths::tempdir;

use super::*;

fn test_context(source_path: impl Into<PathBuf>) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "codex-history-batch-test-machine".to_owned(),
        source_path: Some(source_path.into()),
        source_root: None,
        imported_at: "2026-07-18T12:00:00Z".parse().unwrap(),
    }
}

fn test_source(length: usize) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::Codex,
        CODEX_HISTORY_SOURCE_FORMAT,
        "codex-history-file:test",
        format!("test-revision:{length}"),
        "provider:codex:codex_history_jsonl:source:test",
        CODEX_HISTORY_CAPTURE_REVISION,
        CODEX_HISTORY_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn test_position(offset: u64) -> NativePosition {
    NativePosition::new(
        "codex-history-test-position-v1",
        offset.to_be_bytes().to_vec(),
    )
    .unwrap()
}

fn test_record(ordinal: u64, bytes: impl AsRef<[u8]>) -> CapturedRecord {
    CapturedRecord::content(
        ordinal,
        NativeLocator::new(
            "codex-history-test-locator-v1",
            ordinal.to_be_bytes().to_vec(),
        )
        .unwrap(),
        ProviderRecordKind::new(CODEX_HISTORY_RECORD_KIND).unwrap(),
        bytes.as_ref().to_vec(),
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
    projector: &mut CodexHistoryCapturedBatchProjector,
    record: &CapturedRecord,
) -> TestProjectionOutput {
    let mut output = TestProjectionOutput::default();
    projector.project_record(record, &mut output).unwrap();
    output
}

fn history_line(session_id: &str, timestamp: i64, text: &str) -> String {
    serde_json::to_string(&json!({
        "session_id": session_id,
        "ts": timestamp,
        "text": text,
    }))
    .unwrap()
}

fn history_file(records: usize, session_id: &str) -> String {
    let mut contents = String::new();
    for index in 0..records {
        contents.push_str(&history_line(
            session_id,
            1_784_371_200 + index as i64,
            &format!("prompt {index}"),
        ));
        contents.push('\n');
    }
    contents
}

fn import_options(path: &Path) -> CodexHistoryImportOptions {
    CodexHistoryImportOptions {
        machine_id: "codex-history-batch-test-machine".to_owned(),
        source_path: Some(path.to_path_buf()),
        imported_at: "2026-07-18T12:00:00Z".parse().unwrap(),
        history_record_id: None,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    }
}

#[test]
fn projector_preserves_prompt_log_projection_and_rejects_bad_rows() {
    let mut projector =
        CodexHistoryCapturedBatchProjector::fresh(test_context("/logical/codex/history.jsonl"));
    let accepted = project(
        &mut projector,
        &test_record(
            0,
            history_line("codex-history-session", 1_784_371_200, "private prompt"),
        ),
    );
    assert!(accepted.rejections.is_empty());
    assert_eq!(accepted.normalizations.len(), 1);
    let capture = &accepted.normalizations[0].captures[0].1;
    assert_eq!(
        capture.source.raw_source_path.as_deref(),
        Some("/logical/codex/history.jsonl")
    );
    assert_eq!(
        capture.source.source_root.as_deref(),
        Some("/logical/codex/history.jsonl")
    );
    assert_eq!(capture.source.fidelity, Fidelity::SummaryOnly);
    assert_eq!(
        capture.session.started_at,
        capture.event.as_ref().unwrap().occurred_at
    );
    assert_eq!(capture.event.as_ref().unwrap().role, Some(EventRole::User));
    assert_eq!(capture.event.as_ref().unwrap().provider_event_index, 0);
    assert_eq!(
        capture.event.as_ref().unwrap().payload["text"],
        "private prompt"
    );

    let malformed = project(&mut projector, &test_record(1, br#"{"session_id""#));
    assert!(malformed.normalizations.is_empty());
    assert_eq!(malformed.rejections.len(), 1);

    let empty_session = project(
        &mut projector,
        &test_record(2, history_line(" ", 1_784_371_202, "empty session")),
    );
    assert_eq!(empty_session.rejections[0].0, 3);

    let blank = project(&mut projector, &test_record(3, b" \t"));
    assert!(blank.normalizations.is_empty());
    assert!(blank.rejections.is_empty());
}

#[test]
fn checkpoint_is_bounded_and_replay_counts_valid_failed_and_blank_records() {
    let records = vec![
        test_record(0, history_line("session-a", 1_784_371_200, "a")),
        test_record(1, br#"{"session_id""#),
        test_record(2, b""),
        test_record(3, history_line("session-b", 1_784_371_203, "b")),
    ];
    let batch = test_batch(records);
    let mut projector =
        CodexHistoryCapturedBatchProjector::fresh(test_context("/logical/codex/history.jsonl"));
    for record in batch.records() {
        let _ = project(&mut projector, record);
    }
    let cursor = match projector.finish_cursor(&batch).unwrap() {
        CapturedBatchCursorFinish::Advance(cursor) => cursor,
        CapturedBatchCursorFinish::RetainPrior => {
            panic!("Codex history checkpoint should always be safe")
        }
    };
    assert!(cursor.parser_checkpoint().as_bytes().len() < 1024);
    let resumed = CodexHistoryCapturedBatchProjector::resume(
        test_context("/logical/codex/history.jsonl"),
        &cursor,
    )
    .unwrap();
    let replay = resumed.replay_summary().unwrap();
    assert_eq!(replay.skipped_sessions, 2);
    assert_eq!(replay.skipped_events, 2);
    assert_eq!(replay.failed, 1);
}

#[test]
fn import_crosses_record_batches_in_one_pass_and_replays_from_certified_cursor() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    fs::write(&path, history_file(70, "codex-history-batched")).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let (first, source_opens) = count_codex_history_source_file_opens(|| {
        import_codex_history_jsonl(&path, &mut store, import_options(&path))
    });
    let first = first.unwrap();
    assert_eq!(source_opens, 1);
    assert_eq!(first.imported_events, 70);
    assert_eq!(first.failed, 0);
    let session = store
        .session_by_external_session(CaptureProvider::Codex, "codex-history-batched")
        .unwrap()
        .unwrap();
    let first_prompt_at = DateTime::from_timestamp(1_784_371_200, 0).unwrap();
    assert_eq!(session.started_at, first_prompt_at);
    let source = store.list_capture_sources().unwrap().pop().unwrap();
    assert_eq!(source.started_at, first_prompt_at);

    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        CODEX_HISTORY_SOURCE_FORMAT,
        &provider_path_identity(&path).unwrap(),
    );
    let stored_cursor = store
        .get_sync_cursor(None, "codex-history-batch-test-machine", &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&stored_cursor.cursor).unwrap();
    assert_eq!(
        jsonl_position_offset(certified.native_position()).unwrap(),
        fs::metadata(&path).unwrap().len()
    );
    let checkpoint: CodexHistoryParserCheckpoint =
        certified.parser_checkpoint().deserialize().unwrap();
    assert_eq!(checkpoint.next_ordinal, 70);

    let replay = import_codex_history_jsonl(&path, &mut store, import_options(&path)).unwrap();
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_events, 70);
    assert_eq!(replay.failed, 0);
}

#[test]
fn import_resumes_verified_append_and_keeps_logical_source_identity() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    let logical_path = temp.path().join("configured-history.jsonl");
    fs::write(&path, history_file(2, "codex-history-append")).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let options = CodexHistoryImportOptions {
        source_path: Some(logical_path.clone()),
        ..import_options(&path)
    };

    let first = import_codex_history_jsonl(&path, &mut store, options.clone()).unwrap();
    assert_eq!(first.imported_events, 2);
    let source = store.list_capture_sources().unwrap().pop().unwrap();
    let logical_display = logical_path.display().to_string();
    assert_eq!(
        source.descriptor.raw_source_path.as_deref(),
        Some(logical_display.as_str())
    );
    assert_eq!(
        source.descriptor.source_root.as_deref(),
        Some(logical_display.as_str())
    );

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        file,
        "{}",
        history_line("codex-history-append", 1_784_371_202, "appended prompt")
    )
    .unwrap();
    file.sync_all().unwrap();

    let appended = import_codex_history_jsonl(&path, &mut store, options).unwrap();
    assert_eq!(appended.imported_events, 1);
    assert_eq!(appended.skipped_events, 0);
    let session = store
        .session_by_external_session(CaptureProvider::Codex, "codex-history-append")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 3);
}

#[test]
fn production_import_retains_the_global_first_seen_session_timestamp() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    fs::write(
        &path,
        format!(
            "{}\n{}\n{}\n",
            history_line("session-a", 1_784_371_300, "later a"),
            history_line("session-b", 1_784_371_250, "b"),
            history_line("session-a", 1_784_371_200, "earlier a"),
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_history_jsonl(&path, &mut store, import_options(&path)).unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 3);
    let session_a = store
        .session_by_external_session(CaptureProvider::Codex, "session-a")
        .unwrap()
        .unwrap();
    assert_eq!(
        session_a.started_at,
        DateTime::from_timestamp(1_784_371_200, 0).unwrap()
    );
    assert_eq!(store.events_for_session(session_a.id).unwrap().len(), 2);
}
