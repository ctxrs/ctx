use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::ContentRef;
use serde_json::json;
use uuid::Uuid;

use crate::complete_content::structured::{
    StructuredCompleteContentResolver, STRUCTURED_RESULT_CONTENT_LOCATOR_KIND,
};
use crate::complete_content::{
    AuthorizedSourceRoute, CompleteContentBodyDigest, CompleteContentSourceFamily,
    ResultContentRequest, ResultContentResolverRegistry, SourceAccessBroker, SourceSnapshot,
    VerifiedContentLocatorsV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
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
fn continue_result_is_separate_source_backed_event_and_resolves_exactly() {
    let temp = tempdir().unwrap();
    let session_path = temp.path().join("session.json");
    let result_body = "continue exact result\nwith unicode 🦀";
    let session_bytes = serde_json::to_vec(&json!({
        "sessionId": "result-session",
        "history": [
            {
                "id": "user-1",
                "message": {"role": "user", "content": "read it"}
            },
            {
                "id": "assistant-1",
                "message": {"role": "assistant", "content": ""},
                "toolCallStates": [{
                    "toolCallId": "call-1",
                    "toolCall": {"function": {"name": "readFile"}},
                    "status": "done",
                    "output": result_body
                }]
            }
        ]
    }))
    .unwrap();
    fs::write(&session_path, &session_bytes).unwrap();

    let (output, checkpoint) = project_session_once(&session_path);
    assert!(output.rejections.is_empty());
    let events = output
        .normalizations
        .iter()
        .flat_map(|normalization| normalization.captures.iter())
        .filter_map(|(_, capture)| capture.event.as_ref())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 3);
    let call = events
        .iter()
        .find(|event| event.event_type == EventType::ToolCall)
        .unwrap();
    assert_eq!(call.provider_event_index, 2);
    assert_eq!(call.provider_event_hash.as_deref(), Some("assistant-1"));
    let result = events
        .iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .unwrap();
    assert_eq!(
        result.provider_event_index,
        continue_result_provider_event_index(1, 0).unwrap()
    );
    assert!(result.payload["result_content_ref"].is_object());
    assert_eq!(result.payload["tool"], "readFile");
    assert_eq!(result.payload["call_id"], "call-1");
    assert_eq!(checkpoint.accepted_events, 3);
    let serialized = serde_json::to_string(result).unwrap();
    assert!(!serialized.contains(result_body));

    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &result.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    let locator = locators.locator(VerifiedContentRole::ResultBody).unwrap();
    assert_eq!(locator.content_profile(), "continue.result-body.v1");
    assert_eq!(
        locator.source_locator().unwrap().kind(),
        STRUCTURED_RESULT_CONTENT_LOCATOR_KIND
    );
    assert!(locator.content_ref().verifies(result_body.as_bytes()));

    let event_id = Uuid::new_v4();
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: CaptureProvider::Continue,
                source_format: CONTINUE_CLI_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Structured,
                raw_source_path: session_path.clone(),
                source_root: session_path.parent().map(Path::to_path_buf),
                source_identity: Some("continue:test-result-session".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            event_id,
        )
        .unwrap();
    let mut request = ResultContentRequest {
        event_id,
        provider: CaptureProvider::Continue,
        source_format: CONTINUE_CLI_SOURCE_FORMAT.to_owned(),
        source_access,
        source_family: CompleteContentSourceFamily::Structured,
        content_profile: locator.content_profile().to_owned(),
        source_locator: locator.source_locator().unwrap(),
        source_record_ordinal: 0,
        source_record_subrecord_index: 2,
        expected_native_record_id: locator.native_record_id().to_owned(),
        expected_record_digest: locator.record_sha256().clone(),
        expected_content_ref: locator.content_ref().clone(),
    };
    let mut registry = ResultContentResolverRegistry::new();
    registry.register(StructuredCompleteContentResolver::new());
    let resolved = registry.resolve(std::slice::from_ref(&request));
    assert_eq!(resolved[0].as_ref().unwrap().content, result_body);
    assert_eq!(
        resolved[0].as_ref().unwrap().content_ref,
        ContentRef::from_bytes(result_body.as_bytes()).unwrap()
    );

    let mut changed_session: Value = serde_json::from_slice(&session_bytes).unwrap();
    changed_session["history"][1]["toolCallStates"][0]["output"] = json!("changed result");
    fs::write(&session_path, serde_json::to_vec(&changed_session).unwrap()).unwrap();
    request.source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: Uuid::new_v4(),
                provider: request.provider,
                source_format: request.source_format.clone(),
                family: CompleteContentSourceFamily::Structured,
                raw_source_path: session_path.clone(),
                source_root: session_path.parent().map(Path::to_path_buf),
                source_identity: Some("continue:test-result-session".to_owned()),
                source_snapshot: SourceSnapshot::default(),
            },
            request.event_id,
        )
        .unwrap();
    let changed = registry.resolve(&[request]);
    assert_eq!(
        changed[0].as_ref().unwrap_err().kind,
        crate::complete_content::CompleteContentErrorKind::SourceChanged
    );
}

#[test]
fn appended_history_preserves_result_uuid_provider_index_and_citation() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    let session_path = root.join("append-stable.json");
    let initial = json!({
        "sessionId": "append-stable",
        "history": [{
            "id": "assistant-1",
            "timestamp": "2026-07-22T12:00:00Z",
            "message": {"role": "assistant", "content": ""},
            "toolCallStates": [{
                "toolCallId": "call-1",
                "toolCall": {"function": {"name": "readFile"}},
                "status": "done",
                "output": "stable result"
            }]
        }]
    });
    fs::write(&session_path, serde_json::to_vec(&initial).unwrap()).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let context = ProviderAdapterContext {
        machine_id: "continue-append-stability".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root.clone()),
        imported_at: "2026-07-22T12:01:00Z".parse().unwrap(),
    };
    import_continue_cli_sessions_batched(
        &root,
        &mut store,
        context.clone(),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    let sessions = store
        .sessions_by_external_session_limited(CaptureProvider::Continue, "append-stable", 2)
        .unwrap();
    assert_eq!(sessions.len(), 1);
    let session_id = sessions[0].id;
    let first = store
        .events_for_session(session_id)
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .unwrap();
    let first_index = first.payload["provider_event_index"].as_u64().unwrap();
    let first_citation = first.sync.metadata["source_record_subrecord_index"]
        .as_u64()
        .unwrap();

    let mut appended = initial;
    appended["history"].as_array_mut().unwrap().push(json!({
        "id": "user-later",
        "timestamp": "2026-07-22T12:00:01Z",
        "message": {"role": "user", "content": "later appended history"}
    }));
    let appended_bytes = serde_json::to_vec(&appended).unwrap();
    fs::write(&session_path, &appended_bytes).unwrap();
    import_continue_cli_sessions_batched(
        &root,
        &mut store,
        context,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    let replayed = store
        .events_for_session(session_id)
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == EventType::ToolOutput)
        .unwrap();
    assert_eq!(replayed.id, first.id);
    assert_eq!(
        replayed.payload["provider_event_index"].as_u64(),
        Some(first_index)
    );
    assert_eq!(
        replayed.sync.metadata["source_record_subrecord_index"].as_u64(),
        Some(first_citation)
    );
    assert_eq!(
        first_index,
        continue_result_provider_event_index(0, 0).unwrap()
    );
    assert_eq!(first_citation, 1);
    let locators = VerifiedContentLocatorsV1::from_metadata_value(
        &replayed.sync.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
    )
    .unwrap();
    assert_eq!(
        locators
            .locator(VerifiedContentRole::ResultBody)
            .unwrap()
            .record_sha256(),
        &CompleteContentBodyDigest::from_text(std::str::from_utf8(&appended_bytes).unwrap())
    );
}

#[test]
fn continue_result_normalizer_preserves_explicit_non_string_json_without_labels() {
    assert_eq!(
        continue_tool_result_body(&json!({"output": {"z": 1, "a": [false, null]}})),
        Some(r#"{"a":[false,null],"z":1}"#.to_owned())
    );
    assert_eq!(
        continue_tool_result_body(&json!({"output": [{"type": "tool_result"}]})),
        Some(r#"[{"type":"tool_result"}]"#.to_owned())
    );
    assert_eq!(
        continue_tool_result_body(&json!({"output": false})),
        Some("false".to_owned())
    );
    assert_eq!(
        continue_tool_result_body(&json!({"output": " exact\ntext "})),
        Some(" exact\ntext ".to_owned())
    );
    assert_eq!(continue_tool_result_body(&json!({"output": null})), None);
}

#[test]
fn continue_result_classification_only_promotes_known_command_tools() {
    assert_eq!(
        continue_result_event_type("runTerminalCommand"),
        EventType::CommandOutput
    );
    assert_eq!(
        continue_result_event_type("execute_command"),
        EventType::CommandOutput
    );
    assert_eq!(
        continue_result_event_type("readFile"),
        EventType::ToolOutput
    );
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
