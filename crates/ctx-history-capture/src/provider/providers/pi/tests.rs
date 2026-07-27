use std::{
    fs::OpenOptions,
    io::{Cursor, Write},
};

#[cfg(unix)]
use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

use crate::captured_batch::{CapturedBatchBuilder, NativeLocator, NativePosition};
use crate::test_support_paths::tempdir;

use super::*;

fn test_context() -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "pi-batch-test-machine".to_owned(),
        source_path: Some("/tmp/pi-batch-test.jsonl".into()),
        source_root: None,
        imported_at: "2026-07-17T12:00:00Z".parse().unwrap(),
    }
}

fn test_source(length: usize) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::Pi,
        PI_SOURCE_FORMAT,
        "pi-jsonl-file:/tmp/pi-batch-test.jsonl",
        format!("test-revision:{length}"),
        "provider:pi:pi_session_jsonl:source:test",
        PI_CAPTURE_REVISION,
        PI_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn test_position(offset: u64) -> NativePosition {
    NativePosition::new("pi-test-position-v1", offset.to_be_bytes().to_vec()).unwrap()
}

fn test_record(ordinal: u64, bytes: impl AsRef<[u8]>) -> CapturedRecord {
    CapturedRecord::content(
        ordinal,
        NativeLocator::new("pi-test-locator", ordinal.to_be_bytes().to_vec()).unwrap(),
        ProviderRecordKind::new(PI_RECORD_KIND).unwrap(),
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
    projector: &mut PiCapturedBatchProjector,
    record: &CapturedRecord,
) -> TestProjectionOutput {
    let mut output = TestProjectionOutput::default();
    projector.project_record(record, &mut output).unwrap();
    output
}

fn finish_cursor(
    projector: &PiCapturedBatchProjector,
    batch: &CapturedBatch,
) -> CertifiedProviderCursor {
    match projector.finish_cursor(batch).unwrap() {
        CapturedBatchCursorFinish::Advance(cursor) => cursor,
        CapturedBatchCursorFinish::RetainPrior => {
            panic!("Pi projector must certify every completed batch")
        }
    }
}

fn assert_rejected(output: TestProjectionOutput) -> String {
    assert!(output.normalizations.is_empty());
    assert_eq!(output.rejections.len(), 1);
    output.rejections.into_iter().next().unwrap().1
}

#[test]
fn real_tool_result_retains_only_bounded_result_evidence_and_outcome() {
    let header = PiSessionHeader {
        id: "pi-session".to_owned(),
        version: Some(3),
        timestamp: "2026-07-21T12:00:00Z".parse().unwrap(),
        cwd: Some("/workspace".to_owned()),
        parent_session: None,
        raw: json!({"type": "session", "id": "pi-session"}),
    };
    let entry = json!({
        "type": "message",
        "id": "result-1",
        "timestamp": "2026-07-21T12:00:01Z",
        "message": {
            "role": "toolResult",
            "toolCallId": "tool-1",
            "success": true,
            "content": [{"type": "text", "text": "[main 0123456789ab] private narrative"}],
        },
    });
    let event = pi_session_event(&header, &entry, 2).unwrap();
    assert_eq!(event.payload["result_outcome"], "success");
    assert_eq!(
        event.payload["result_evidence"],
        json!([
            {"kind": "call_id", "value": "tool-1"},
            {"kind": "git_commit_summary_id", "value": "0123456789ab"},
        ])
    );
    assert!(!event.payload.to_string().contains("private narrative"));
}

#[test]
fn projector_accepts_valid_records_and_rejects_invalid_complete_records() {
    let mut projector = PiCapturedBatchProjector::fresh(test_context());

    let before_header = project(
        &mut projector,
        &test_record(
            0,
            br#"{"type":"message","timestamp":"2026-07-17T12:00:01Z"}"#,
        ),
    );
    assert!(assert_rejected(before_header).contains("before session header"));

    let header = project(
            &mut projector,
            &test_record(
                1,
                br#"{"type":"session","id":"pi-batch-session","version":3,"timestamp":"2026-07-17T12:00:00Z","cwd":"/workspace"}"#,
            ),
        );
    assert!(header.normalizations.is_empty());
    assert!(header.rejections.is_empty());

    let invalid_replacement = project(
        &mut projector,
        &test_record(
            2,
            br#"{"type":"session","timestamp":"2026-07-17T12:00:00Z"}"#,
        ),
    );
    assert!(assert_rejected(invalid_replacement).contains("missing id"));

    let missing_timestamp = project(
        &mut projector,
        &test_record(3, br#"{"type":"message","id":"missing-time"}"#),
    );
    assert!(assert_rejected(missing_timestamp).contains("missing timestamp"));

    let mut accepted = project(
            &mut projector,
            &test_record(
                4,
                br#"{"type":"message","id":"event-1","timestamp":"2026-07-17T12:00:01Z","message":{"role":"user","content":"hello"}}"#,
            ),
        );
    assert!(accepted.rejections.is_empty());
    assert_eq!(accepted.normalizations.len(), 1);
    let normalization = accepted.normalizations.pop().unwrap();
    assert_eq!(normalization.captures.len(), 1);
    assert_eq!(
        normalization.captures[0].1.session.provider_session_id,
        "pi-batch-session"
    );

    let malformed = project(&mut projector, &test_record(5, br#"{"type":"message"#));
    assert!(!assert_rejected(malformed).is_empty());
    assert_eq!(projector.next_ordinal, 6);
}

#[test]
fn checkpoint_round_trips_header_state_without_transcript_text() {
    let transcript_text = "checkpoint-must-not-retain-this-transcript";
    let batch = test_batch(vec![
            test_record(
                0,
                br#"{"type":"session","id":"pi-checkpoint","version":3,"timestamp":"2026-07-17T12:00:00Z","cwd":"/workspace","parentSession":"parent-1"}"#,
            ),
            test_record(
                1,
                format!(
                    "{{\"type\":\"message\",\"id\":\"event-1\",\"timestamp\":\"2026-07-17T12:00:01Z\",\"message\":{{\"role\":\"user\",\"content\":\"{transcript_text}\"}}}}"
                )
                .into_bytes(),
            ),
        ]);
    let mut projector = PiCapturedBatchProjector::fresh(test_context());
    for (index, record) in batch.records().iter().enumerate() {
        let output = project(&mut projector, record);
        assert_eq!(output.normalizations.len(), usize::from(index == 1));
        assert!(output.rejections.is_empty());
    }

    let cursor = finish_cursor(&projector, &batch);
    let checkpoint_bytes = cursor.parser_checkpoint().as_bytes();
    assert!(!String::from_utf8_lossy(checkpoint_bytes).contains(transcript_text));
    let decoded = CertifiedProviderCursor::decode(&cursor.encode().unwrap()).unwrap();
    assert_eq!(decoded, cursor);
    let checkpoint: PiParserCheckpoint = decoded.parser_checkpoint().deserialize().unwrap();
    assert_eq!(checkpoint.next_ordinal, 2);
    assert_eq!(checkpoint.header.unwrap().id, "pi-checkpoint");

    let resumed = PiCapturedBatchProjector::resume(test_context(), &decoded).unwrap();
    assert_eq!(resumed.next_ordinal, 2);
    assert_eq!(resumed.header.unwrap().id, "pi-checkpoint");
}

#[test]
fn sixty_five_pi_records_partition_and_resume_at_the_exact_ordinal() {
    let mut lines = vec![
        "{\"type\":\"session\",\"id\":\"pi-sixty-five\",\"timestamp\":\"2026-07-17T12:00:00Z\"}"
            .to_owned(),
    ];
    for index in 0..64 {
        lines.push(format!(
                "{{\"type\":\"message\",\"id\":\"event-{index}\",\"timestamp\":\"2026-07-17T12:00:01Z\",\"message\":{{\"role\":\"user\",\"content\":\"message {index}\"}}}}"
            ));
    }
    let bytes = format!("{}\n", lines.join("\n")).into_bytes();
    let mut producer = JsonlBatchProducer::new(
        Cursor::new(bytes.clone()),
        test_source(bytes.len()),
        b"pi-sixty-five.jsonl".to_vec(),
        ProviderRecordKind::new(PI_RECORD_KIND).unwrap(),
        bytes.len() as u64,
        0,
        0,
        false,
    )
    .unwrap();

    let first = producer.next_batch().unwrap().unwrap();
    let second = producer.next_batch().unwrap().unwrap();
    assert_eq!(first.records().len(), 64);
    assert_eq!(second.records().len(), 1);
    assert_eq!(second.records()[0].ordinal(), 64);
    assert!(producer.next_batch().unwrap().is_none());

    let mut projector = PiCapturedBatchProjector::fresh(test_context());
    let mut output = TestProjectionOutput::default();
    for record in first.records() {
        projector.project_record(record, &mut output).unwrap();
    }
    let first_cursor = finish_cursor(&projector, &first);
    let first_checkpoint: PiParserCheckpoint =
        first_cursor.parser_checkpoint().deserialize().unwrap();
    assert_eq!(first_checkpoint.next_ordinal, 64);

    let mut resumed = PiCapturedBatchProjector::resume(test_context(), &first_cursor).unwrap();
    for record in second.records() {
        resumed.project_record(record, &mut output).unwrap();
    }
    let second_cursor = finish_cursor(&resumed, &second);
    let second_checkpoint: PiParserCheckpoint =
        second_cursor.parser_checkpoint().deserialize().unwrap();
    assert_eq!(second_checkpoint.next_ordinal, 65);
}

#[test]
fn file_import_reads_a_structurally_valid_source_once() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("one-pass.jsonl");
    fs::write(
            &path,
            concat!(
                r#"{"type":"session","id":"pi-one-pass","timestamp":"2026-07-17T12:00:00Z"}"#,
                "\n",
                r#"{"type":"message","id":"one-pass-1","timestamp":"2026-07-17T12:00:01Z","message":{"role":"user","content":"one pass"}}"#,
                "\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "pi-one-pass-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };

    let (summary, source_opens) = count_pi_source_file_opens(|| {
        import_pi_session_jsonl_file_batched(
            &path,
            &mut store,
            context,
            NormalizedProviderImportOptions::default(),
        )
    });
    let summary = summary.unwrap();

    assert_eq!(source_opens, 1);
    assert_eq!(summary.imported_events, 1);
    assert_eq!(summary.failed, 0);
}

#[test]
fn tool_only_file_import_converges_from_the_certified_cursor() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("tool-only.jsonl");
    fs::write(
            &path,
            concat!(
                r#"{"type":"session","id":"pi-tool-only","timestamp":"2026-07-17T12:00:00Z"}"#,
                "\n",
                r#"{"type":"message","id":"tool-only-1","timestamp":"2026-07-17T12:00:01Z","message":{"role":"toolResult","content":"private tool output"}}"#,
                "\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "pi-tool-only-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };

    let (first, first_source_opens) = count_pi_source_file_opens(|| {
        import_pi_session_jsonl_file_batched(
            &path,
            &mut store,
            context.clone(),
            NormalizedProviderImportOptions::default(),
        )
    });
    let first = first.unwrap();
    assert_eq!(first_source_opens, 1);
    assert_eq!(first.imported_events, 1);
    assert_eq!(first.failed, 0);

    let session = store
        .session_by_external_session(CaptureProvider::Pi, "pi-tool-only")
        .unwrap()
        .unwrap();
    let events = store.events_for_session(session.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, EventType::ToolOutput);

    let (second, second_source_opens) = count_pi_source_file_opens(|| {
        import_pi_session_jsonl_file_batched(
            &path,
            &mut store,
            ProviderAdapterContext {
                imported_at: "2026-07-17T12:02:00Z".parse().unwrap(),
                ..context
            },
            NormalizedProviderImportOptions::default(),
        )
    });
    let second = second.unwrap();
    assert_eq!(second_source_opens, 1);
    assert_eq!(second.skipped_events, 1);
    assert_eq!(second.failed, 0);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}

#[test]
fn file_import_resumes_a_verified_append_without_replaying_the_prefix() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("append.jsonl");
    fs::write(
            &path,
            concat!(
                r#"{"type":"session","id":"pi-append","timestamp":"2026-07-17T12:00:00Z","cwd":"/workspace"}"#,
                "\n",
                r#"{"type":"message","id":"append-1","timestamp":"2026-07-17T12:00:01Z","message":{"role":"user","content":"first append message"}}"#,
                "\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "pi-append-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };

    let first = import_pi_session_jsonl_file_batched(
        &path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.imported_events, 1);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(
            concat!(
                r#"{"type":"message","id":"append-2","timestamp":"2026-07-17T12:00:02Z","message":{"role":"assistant","content":"second append message"}}"#,
                "\n",
            )
            .as_bytes(),
        )
        .unwrap();
    file.sync_all().unwrap();
    let second = import_pi_session_jsonl_file_batched(
        &path,
        &mut store,
        ProviderAdapterContext {
            imported_at: "2026-07-17T12:02:00Z".parse().unwrap(),
            ..context
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(second.imported_events, 1);
    assert_eq!(second.skipped_events, 0);
    let session = store
        .session_by_external_session(CaptureProvider::Pi, "pi-append")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2);
}

#[test]
fn file_import_does_not_publish_an_incomplete_final_record() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("incomplete-tail.jsonl");
    fs::write(
            &path,
            concat!(
                r#"{"type":"session","id":"pi-incomplete-tail","timestamp":"2026-07-17T12:00:00Z"}"#,
                "\n",
                r#"{"type":"message","id":"tail-1","timestamp":"2026-07-17T12:00:01Z","message":{"role":"user","content":"complete"}}"#,
                "\n",
                r#"{"type":"message","id":"tail-2","timestamp":"2026-07-17T12:00:02Z","message":{"role":"assistant","content":"incom"#,
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "pi-incomplete-tail-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };

    let first = import_pi_session_jsonl_file_batched(
        &path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.imported_events, 1);
    assert_eq!(first.failed, 0);
    let session = store
        .session_by_external_session(CaptureProvider::Pi, "pi-incomplete-tail")
        .unwrap()
        .unwrap();
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"plete\"}}\n").unwrap();
    file.sync_all().unwrap();
    let second = import_pi_session_jsonl_file_batched(
        &path,
        &mut store,
        ProviderAdapterContext {
            imported_at: "2026-07-17T12:02:00Z".parse().unwrap(),
            ..context
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(second.imported_events, 1);
    assert_eq!(second.failed, 0);
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 2);
}

#[cfg(unix)]
#[test]
fn durable_path_identity_preserves_non_utf8_bytes_and_is_bounded() {
    let first = PathBuf::from(OsString::from_vec(b"/tmp/pi-\xff.jsonl".to_vec()));
    let second = PathBuf::from(OsString::from_vec(b"/tmp/pi-\xfe.jsonl".to_vec()));

    let first_identity = provider_path_identity(&first).unwrap();
    let second_identity = provider_path_identity(&second).unwrap();
    assert!(first_identity.starts_with("provider-path-v1:unix-bytes:"));
    assert_ne!(first_identity, second_identity);
    assert!(first_identity.ends_with("ff2e6a736f6e6c"));

    let oversized = PathBuf::from(OsString::from_vec(vec![
        b'x';
        crate::provider::importer::MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES
            + 1
    ]));
    assert!(matches!(
        provider_path_identity(&oversized).unwrap_err(),
        CaptureError::InvalidProviderTranscriptPath { .. }
    ));
}

#[test]
fn deterministic_rejection_advances_checkpoint_and_preserves_header() {
    let batch = test_batch(vec![
            test_record(
                0,
                br#"{"type":"session","id":"pi-rejection","timestamp":"2026-07-17T12:00:00Z"}"#,
            ),
            test_record(1, br#"{"type":"message"#),
            test_record(
                2,
                br#"{"type":"message","id":"event-after-rejection","timestamp":"2026-07-17T12:00:01Z","message":{"role":"assistant","content":"still valid"}}"#,
            ),
        ]);
    let mut projector = PiCapturedBatchProjector::fresh(test_context());
    let accepted_header = project(&mut projector, &batch.records()[0]);
    assert!(accepted_header.normalizations.is_empty());
    assert!(accepted_header.rejections.is_empty());
    let rejected = project(&mut projector, &batch.records()[1]);
    assert!(!assert_rejected(rejected).is_empty());
    let accepted_event = project(&mut projector, &batch.records()[2]);
    assert_eq!(accepted_event.normalizations.len(), 1);
    assert!(accepted_event.rejections.is_empty());

    let cursor = finish_cursor(&projector, &batch);
    let checkpoint: PiParserCheckpoint = cursor.parser_checkpoint().deserialize().unwrap();
    assert_eq!(checkpoint.next_ordinal, 3);
    assert_eq!(checkpoint.header.unwrap().id, "pi-rejection");
}

#[test]
fn malformed_file_replay_preserves_certified_rejection_without_scaffolding() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("malformed.jsonl");
    fs::write(&path, b"{\"type\":\"message\"\n").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "pi-malformed-replay-machine".to_owned(),
        source_path: Some(path.clone()),
        source_root: None,
        imported_at: "2026-07-17T12:01:00Z".parse().unwrap(),
    };

    let first = import_pi_session_jsonl_file_batched(
        &path,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.failures.len(), 1);
    assert_eq!(first.imported_sessions, 0);
    assert_eq!(first.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());

    let replay = import_pi_session_jsonl_file_batched(
        &path,
        &mut store,
        ProviderAdapterContext {
            imported_at: "2026-07-17T12:02:00Z".parse().unwrap(),
            ..context
        },
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();

    assert_eq!(replay.failed, first.failed);
    assert!(replay.failures.is_empty());
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert!(store.list_sessions().unwrap().is_empty());
}
