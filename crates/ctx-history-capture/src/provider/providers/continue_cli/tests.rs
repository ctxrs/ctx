use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use serde_json::json;

use crate::provider::importer::{ProviderProjectionOutput, ProviderProjectionResult};
use crate::test_support_paths::tempdir;

use super::*;

#[derive(Default)]
struct CollectingProjectionOutput {
    normalizations: Vec<ProviderNormalizationResult>,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for CollectingProjectionOutput {
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

fn project_session_once(
    session_path: &Path,
) -> (CollectingProjectionOutput, ContinueParserCheckpoint) {
    let mut cache = ContinueIndexCache::default();
    let observation = ContinueSessionObservation::read(session_path, &mut cache).unwrap();
    let source = SourceObservation::new(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        "continue-session-file:test",
        observation.source_revision(),
        "provider:continue:test",
        CONTINUE_CAPTURE_REVISION,
        CONTINUE_POLICY_REVISION,
        None,
    )
    .unwrap();
    let mut emitted = false;
    let item_path = session_path.to_path_buf();
    let item_length = observation.session_length();
    let mut producer = WholeJsonBatchProducer::new(
        source,
        ProviderRecordKind::new(CONTINUE_RECORD_KIND).unwrap(),
        move || {
            if emitted {
                return Ok(None);
            }
            emitted = true;
            WholeJsonItem::new(0, b"session.json".to_vec(), item_length, item_path.clone())
                .map(Some)
        },
    )
    .unwrap();
    let batch = producer.next_batch().unwrap().unwrap();
    let context = ProviderAdapterContext {
        machine_id: "continue-one-pass-test".to_owned(),
        source_path: Some(session_path.to_path_buf()),
        source_root: None,
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let mut projector = ContinueCapturedBatchProjector::fresh(
        context,
        session_path.display().to_string(),
        session_path,
        observation.sibling_index(),
        &cache,
    );
    let mut output = CollectingProjectionOutput::default();
    assert_eq!(batch.records().len(), 1);
    projector
        .project_record(&batch.records()[0], &mut output)
        .unwrap();
    assert!(producer.next_batch().unwrap().is_none());
    let CapturedBatchCursorFinish::Advance(cursor) = projector.finish_cursor(&batch).unwrap()
    else {
        panic!("Continue projector unexpectedly retained the prior cursor");
    };
    let checkpoint = cursor.parser_checkpoint().deserialize().unwrap();
    (output, checkpoint)
}

#[test]
fn unchanged_replay_preserves_certified_rejection_count() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::write(sessions.join("malformed.json"), b"{not-json").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "continue-rejection-replay".to_owned(),
        source_path: Some(sessions.clone()),
        source_root: Some(sessions.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };

    let first = import_continue_cli_sessions_batched(
        &sessions,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.failed, 1, "{:?}", first.failures);
    assert_eq!(first.failures.len(), 1);
    assert_eq!(first.imported_sessions, 0);
    assert_eq!(first.imported_events, 0);

    let replay = import_continue_cli_sessions_batched(
        &sessions,
        &mut store,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay.failed, 1, "{:?}", replay.failures);
    assert!(
        replay.failures.is_empty(),
        "unchanged replay retains the cumulative cursor count without duplicating details"
    );
    assert_eq!(replay.imported_sessions, 0);
    assert_eq!(replay.skipped_sessions, 0);
    assert_eq!(replay.imported_events, 0);
    assert_eq!(replay.skipped_events, 0);
}

#[test]
fn one_pass_projection_keeps_tool_only_history() {
    let temp = tempdir().unwrap();
    let session_path = temp.path().join("session.json");
    fs::write(
        &session_path,
        serde_json::to_vec(&json!({
            "sessionId": "tool-only",
            "history": [{
                "message": {"role": "assistant", "content": ""},
                "toolCallStates": [{
                    "toolCall": {"function": {"name": "read_file"}},
                    "status": "done"
                }]
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        temp.path().join("sessions.json"),
        br#"[{"sessionId":"tool-only","dateCreated":"2024-01-02T03:04:05Z"}]"#,
    )
    .unwrap();

    let (output, checkpoint) = project_session_once(&session_path);

    assert!(output.rejections.is_empty());
    assert_eq!(output.normalizations.len(), 1);
    let capture = &output.normalizations[0].captures[0].1;
    assert_eq!(capture.session.provider_session_id, "tool-only");
    assert_eq!(
        capture.event.as_ref().unwrap().event_type,
        EventType::ToolCall
    );
    assert_eq!(checkpoint.accepted_sessions, 1);
    assert_eq!(checkpoint.accepted_events, 1);
}

#[test]
fn one_pass_projection_keeps_metadata_only_history() {
    let temp = tempdir().unwrap();
    let session_path = temp.path().join("metadata.json");
    fs::write(
        &session_path,
        serde_json::to_vec(&json!({
            "sessionId": "metadata-only",
            "title": "Metadata only",
            "history": []
        }))
        .unwrap(),
    )
    .unwrap();

    let (output, checkpoint) = project_session_once(&session_path);

    assert!(output.rejections.is_empty());
    assert_eq!(output.normalizations.len(), 1);
    let capture = &output.normalizations[0].captures[0].1;
    assert_eq!(capture.session.provider_session_id, "metadata-only");
    assert!(capture.event.is_none());
    assert_eq!(checkpoint.accepted_sessions, 1);
    assert_eq!(checkpoint.accepted_events, 0);
}
