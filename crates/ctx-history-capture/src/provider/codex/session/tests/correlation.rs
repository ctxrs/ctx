use super::*;

use crate::complete_content::{
    PersistedCompleteContentLocatorV1, RESULT_CONTENT_LOCATOR_METADATA_KEY,
};
use crate::provider::codex::events::codex_tool_output_event;

#[test]
fn codex_tool_checkpoint_is_bounded_after_json_escaping() {
    let mut projector = CodexCapturedBatchProjector::fresh(ProviderAdapterContext::default());
    projector.header = Some(
        codex_session_header(
            serde_json::from_str(&session_meta("checkpoint-session", None)).unwrap(),
        )
        .unwrap(),
    );
    for index in 0..64 {
        projector.correlation.insert_for_test(
            format!("call-{index:02}-{}", "\\n".repeat(128)),
            CodexToolCallContext {
                tool_name: "shell".to_owned(),
                command_preview: Some("\u{0000}".repeat(4 * 1024)),
                arguments_preview: Some("\"".repeat(4 * 1024)),
            },
        );
    }

    projector.bound_call_contexts();

    assert!(projector.correlation.len() <= CODEX_MAX_TOOL_CONTEXTS);
    BoundedParserCheckpoint::from_serializable(&projector.checkpoint(0)).unwrap();
}

#[test]
fn codex_certified_cursor_omits_tool_preview_content() {
    const SECRET_COMMAND: &str = "curl -H 'Authorization: Bearer codex-secret-preview'";
    const SECRET_ARGUMENTS: &str =
        r#"{"token":"codex-secret-arguments","path":"/private/transcript"}"#;
    let mut projector = CodexCapturedBatchProjector::fresh(ProviderAdapterContext::default());
    projector.correlation.insert_for_test(
        "content-free-call".to_owned(),
        CodexToolCallContext {
            tool_name: "shell".to_owned(),
            command_preview: Some(SECRET_COMMAND.to_owned()),
            arguments_preview: Some(SECRET_ARGUMENTS.to_owned()),
        },
    );
    let cursor = CertifiedProviderCursor::new(
        "codex-content-free-cursor",
        CODEX_CAPTURE_REVISION,
        CODEX_POLICY_REVISION,
        initial_jsonl_position().unwrap(),
        BoundedParserCheckpoint::from_serializable(&projector.checkpoint(0)).unwrap(),
    )
    .unwrap();

    let encoded = cursor.encode().unwrap();
    let decoded = CertifiedProviderCursor::decode(&encoded).unwrap();
    let checkpoint_wire = String::from_utf8_lossy(decoded.parser_checkpoint().as_bytes());
    assert!(!checkpoint_wire.contains(SECRET_COMMAND));
    assert!(!checkpoint_wire.contains(SECRET_ARGUMENTS));
    assert!(!checkpoint_wire.contains("codex-secret-preview"));
    assert!(!checkpoint_wire.contains("codex-secret-arguments"));
    assert!(!checkpoint_wire.contains("command_preview"));
    assert!(!checkpoint_wire.contains("arguments_preview"));
    assert!(checkpoint_wire.contains("content-free-call"));
    assert!(checkpoint_wire.contains("shell"));
}

#[test]
fn codex_tool_correlation_is_partition_independent_and_content_free() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("append.jsonl");
    let initial = [
        jsonl_line(json!({
            "timestamp": "2026-07-18T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "append-session",
                "timestamp": "2026-07-18T12:00:00Z",
                "cwd": "/workspace",
                "originator": "codex-cli",
                "source": {
                    "kind": "cli",
                    "secret": "codex-secret-session-source"
                }
            }
        })),
        jsonl_line(json!({
            "timestamp": "2026-07-18T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell",
                "call_id": "append-call",
                "arguments": "{\"cmd\":\"printf codex-secret-boundary\"}"
            }
        })),
    ]
    .concat();
    let output_line = jsonl_line(json!({
        "timestamp": "2026-07-18T12:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": "append-call",
            "output": "Process exited with code 7\nOutput:\nappend failure"
        }
    }));
    let complete = [initial.as_str(), output_line.as_str()].concat();
    let options = CodexSessionImportOptions {
        imported_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        ..CodexSessionImportOptions::default()
    };

    fs::write(&path, &complete).unwrap();
    let mut full_store = Store::open(temp.path().join("full.sqlite")).unwrap();
    let full = import_codex_session_jsonl(&path, &mut full_store, options.clone()).unwrap();
    assert_eq!(full.failed, 0, "{:?}", full.failures);
    assert_eq!(full.imported_events, 2);
    let full_session = full_store
        .session_by_external_session(CaptureProvider::Codex, "append-session")
        .unwrap()
        .unwrap();
    let full_events = full_store.events_for_session(full_session.id).unwrap();

    fs::write(&path, &initial).unwrap();
    let mut split_store = Store::open(temp.path().join("split.sqlite")).unwrap();
    let first = import_codex_session_jsonl(&path, &mut split_store, options.clone()).unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 1);
    let path_identity = provider_path_identity(&path).unwrap();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        &path_identity,
    );
    let stored = split_store
        .get_sync_cursor(None, &options.machine_id, &stream)
        .unwrap()
        .unwrap();
    let certified = CertifiedProviderCursor::decode(&stored.cursor).unwrap();
    let checkpoint_wire = String::from_utf8_lossy(certified.parser_checkpoint().as_bytes());
    assert!(!checkpoint_wire.contains("codex-secret-boundary"));
    assert!(!checkpoint_wire.contains("codex-secret-session-source"));

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(output_line.as_bytes()).unwrap();
    file.sync_all().unwrap();

    let appended =
        import_codex_session_jsonl_tail(&path, initial.len() as u64, &mut split_store, options)
            .unwrap();
    assert_eq!(appended.failed, 0, "{:?}", appended.failures);
    assert_eq!(appended.imported_events, 1);
    let split_session = split_store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|session| session.external_session_id.as_deref() == Some("append-session"))
        .unwrap();
    let split_events = split_store.events_for_session(split_session.id).unwrap();
    assert_eq!(split_events, full_events);
    let output = split_events
        .iter()
        .find(|event| event.event_type == EventType::CommandOutput)
        .unwrap();
    assert_eq!(output.payload["body"]["tool"], "shell");
    assert!(output.payload["body"].get("command").is_none());
    assert!(output.payload["body"].get("result_content_ref").is_some());
    assert!(!output.payload.to_string().contains("append failure"));
    let content_ref =
        serde_json::from_value::<ContentRef>(output.payload["body"]["result_content_ref"].clone())
            .unwrap();
    let locator = PersistedCompleteContentLocatorV1::from_metadata_value(
        &output.sync.metadata[RESULT_CONTENT_LOCATOR_METADATA_KEY],
    )
    .unwrap();
    assert_eq!(locator.body_sha256().as_str(), content_ref.sha256());
    assert_eq!(
        output.payload["body"]["output_bytes"],
        content_ref.byte_len()
    );
}

#[test]
fn successful_output_keeps_typed_evidence_and_full_content_identity_without_body_text() {
    let mut contexts = BTreeMap::new();
    contexts.insert(
        "call-success".to_owned(),
        CodexToolCallContext {
            tool_name: "exec_command".to_owned(),
            command_preview: Some("git commit".to_owned()),
            arguments_preview: None,
        },
    );
    let occurred_at = "2026-07-21T00:00:00Z".parse().unwrap();
    let event = codex_tool_output_event(
        &json!({
            "type": "function_call_output",
            "call_id": "call-success",
            "output": "Created commit 0123456789abcdef0123456789abcdef01234567; https://github.com/ctxrs/ctx/pull/123"
        }),
        2,
        occurred_at,
        &contexts,
    )
    .unwrap();

    let output = "Created commit 0123456789abcdef0123456789abcdef01234567; https://github.com/ctxrs/ctx/pull/123";
    assert_eq!(event.payload["call_id"], "call-success");
    assert_eq!(event.payload["output_bytes"], output.len());
    assert!(event.payload.get("output_preview").is_none());
    assert!(!event.payload.to_string().contains(output));
    assert!(event
        .payload
        .get("result_evidence")
        .is_some_and(|evidence| evidence
            .to_string()
            .contains("0123456789abcdef0123456789abcdef01234567")));
    let content_ref =
        serde_json::from_value::<ContentRef>(event.payload["result_content_ref"].clone()).unwrap();
    assert_eq!(
        content_ref,
        ContentRef::from_bytes(output.as_bytes()).unwrap()
    );

    let diff = codex_tool_output_event(
        &json!({
            "type": "function_call_output",
            "call_id": "call-success",
            "output": "diff --git a/src/lib.rs b/src/lib.rs\n@@\n-old\n+new\n"
        }),
        3,
        occurred_at,
        &contexts,
    )
    .unwrap();
    let rendered = diff.payload.to_string();
    assert!(!rendered.contains("output_preview"));
    assert!(!rendered.contains("diff --git"));
    assert!(!rendered.contains("-old"));
    let content_ref =
        serde_json::from_value::<ContentRef>(diff.payload["result_content_ref"].clone()).unwrap();
    assert_eq!(
        content_ref,
        ContentRef::from_bytes(b"diff --git a/src/lib.rs b/src/lib.rs\n@@\n-old\n+new\n").unwrap()
    );
}
