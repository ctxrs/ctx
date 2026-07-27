use crate::provider::codex::events::{
    codex_output_text, codex_result_content, codex_tool_output_event, codex_tool_output_projection,
};
use crate::provider::codex::session::should_parse_codex_session_line;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::importer::{
    provider_command_run_from_event, provider_source_event_seq, provider_source_event_uuid,
    provider_source_session_uuid, ProviderCommandRunInput,
};
use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{
    compute_payload_hash, import_codex_session_jsonl, import_codex_session_tree,
    CodexSessionImportOptions, ANTIGRAVITY_CLI_SOURCE_FORMAT, CLAUDE_PROJECTS_SOURCE_FORMAT,
    COPILOT_CLI_SOURCE_FORMAT, CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, FACTORY_DROID_SOURCE_FORMAT,
    FORGECODE_SQLITE_SOURCE_FORMAT, GEMINI_CLI_SOURCE_FORMAT, OPENCODE_SQLITE_SOURCE_FORMAT,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{
    new_id, CaptureProvider, ContentRef, EventRole, EventType, Fidelity, FileChangeKind,
    ProviderEventEnvelope,
};
use ctx_history_store::Store;
use serde_json::{json, Value};
use std::{borrow::Cow, fs, path::Path};

fn test_provider_event(event_type: EventType) -> ProviderEventEnvelope {
    ProviderEventEnvelope {
        provider_event_index: 0,
        provider_event_hash: Some("event-hash".to_owned()),
        cursor: None,
        event_type,
        role: Some(EventRole::Tool),
        occurred_at: "2026-07-03T12:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: json!({}),
        metadata: json!({}),
    }
}

#[test]
fn provider_command_run_rejects_negative_duration() {
    let event = test_provider_event(EventType::CommandOutput);
    let err = provider_command_run_from_event(ProviderCommandRunInput {
        provider: CaptureProvider::Codex,
        provider_session_id: "duration-session",
        session_id: new_id(),
        source_id: new_id(),
        run_source_id: None,
        history_record_id: None,
        event: &event,
        payload: &json!({
            "command": "cargo test",
            "duration_ms": -1
        }),
        event_hash: "event-hash",
    })
    .unwrap_err();

    assert!(err.to_string().contains("duration_ms must be nonnegative"));
}

#[test]
fn codex_session_tree_keeps_results_source_backed() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-rich-sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_tree(
        &fixture,
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T01:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 7);
    assert_eq!(summary.skipped_events, 1);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-rich-session");
    let events = store.events_for_session(session_id).unwrap();
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ToolCall
            && event.payload.to_string().contains("apply_patch")));
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::Summary
            && event
                .payload
                .to_string()
                .contains("sample command completed")));

    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("cargo test -p sample -- --token fixture-secret-token"));
    assert!(!rendered.contains("unit tests passed in /workspace/ctx-rich-fixture"));
    assert!(!rendered.contains("*** Begin Patch"));
    assert!(!rendered.contains("old_fixture"));
    assert!(!rendered.contains("new_fixture"));
    assert!(!rendered.contains("patch_apply_end"));
    assert!(!rendered.contains("opaque-private-reasoning-payload"));
}

#[test]
fn codex_default_line_filter_parses_policy_relevant_lines() {
    let session_meta =
        br#"{"timestamp":"2026-06-24T01:00:00.000Z","type":"session_meta","payload":{"id":"s"}}"#;
    let user_message = br#"{"timestamp":"2026-06-24T01:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"question"}]}}"#;
    let assistant_message = br#"{"timestamp":"2026-06-24T01:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}}"#;
    let developer_message = br#"{"timestamp":"2026-06-24T01:00:02.500Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"instruction"}]}}"#;
    let tool_call = br#"{"timestamp":"2026-06-24T01:00:03.000Z","type":"response_item","payload":{"type":"function_call","call_id":"call-1","name":"shell","arguments":"cargo test"}}"#;
    let tool_output = br#"{"timestamp":"2026-06-24T01:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"passed"}}"#;
    let structured_failed_tool_output = br#"{"timestamp":"2026-06-24T01:00:04.500Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-2","output":{"message":{"exitCode":1,"output":"failed"}}}}"#;
    let reasoning = br#"{"timestamp":"2026-06-24T01:00:05.000Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"thinking"}]}}"#;
    let notice = br#"{"timestamp":"2026-06-24T01:00:06.000Z","type":"event_msg","payload":{"type":"task_complete"}}"#;
    let apply_patch = br#"{"timestamp":"2026-06-24T01:00:07.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Update File: crates/ctx-cli/src/main.rs\n@@\n-old\n+new\n*** End Patch","call_id":"call-patch","status":"completed"}}"#;

    for line in [
        session_meta.as_slice(),
        user_message.as_slice(),
        assistant_message.as_slice(),
        developer_message.as_slice(),
        tool_call.as_slice(),
        apply_patch.as_slice(),
        reasoning.as_slice(),
    ] {
        assert!(should_parse_codex_session_line(line));
    }
    assert!(should_parse_codex_session_line(tool_output));
    assert!(should_parse_codex_session_line(
        structured_failed_tool_output
    ));
    assert!(!should_parse_codex_session_line(notice));
}

#[test]
fn codex_current_nested_legacy_and_wait_outputs_keep_stable_call_linkage() {
    let temp = tempdir();
    let fixture = temp.path().join("codex-output-shapes.jsonl");
    let lines = [
        json!({
            "timestamp": "2026-07-18T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-output-shapes",
                "timestamp": "2026-07-18T12:00:00Z",
                "cwd": "/workspace/ctx"
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "name": "exec",
                "call_id": "call-current",
                "input": "git commit -m fixture"
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call-current",
                "output": [
                    {"type": "input_text", "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"},
                    {"type": "input_text", "text": "[codex/zig-conformance db817fa] test(zig): harden semantic acceptance\n"}
                ]
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:03Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell",
                "call_id": "call-legacy",
                "arguments": "git status --short"
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:04Z",
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "call_id": "call-legacy",
                "output": "Process exited with code 0\nOutput:\nclean\n"
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:05Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "name": "wait",
                "call_id": "call-wait",
                "input": "cell-123"
            }
        }),
        json!({
            "timestamp": "2026-07-18T12:00:06Z",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call_output",
                "call_id": "call-wait",
                "output": [{"type": "input_text", "text": "Script running with cell ID cell-123"}]
            }
        }),
    ];
    fs::write(
        &fixture,
        lines
            .iter()
            .map(|line| serde_json::to_string(line).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary =
        import_codex_session_jsonl(&fixture, &mut store, CodexSessionImportOptions::default())
            .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 6);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-output-shapes");
    let events = store.events_for_session(session_id).unwrap();
    let current = events
        .iter()
        .find(|event| {
            event
                .payload
                .pointer("/body/call_id")
                .and_then(Value::as_str)
                == Some("call-current")
                && event.event_type == EventType::CommandOutput
        })
        .expect("current custom output should be canonicalized");
    assert_eq!(
        current
            .payload
            .pointer("/body/exit_code")
            .and_then(Value::as_i64),
        Some(0)
    );
    assert_eq!(
        current
            .payload
            .pointer("/body/duration_ms")
            .and_then(Value::as_i64),
        Some(100)
    );
    assert!(current
        .payload
        .pointer("/body/result_content_ref")
        .is_some());
    assert!(!current.payload.to_string().contains("Script completed"));
    assert!(!current
        .payload
        .to_string()
        .contains("harden semantic acceptance"));

    let legacy = events
        .iter()
        .find(|event| {
            event
                .payload
                .pointer("/body/call_id")
                .and_then(Value::as_str)
                == Some("call-legacy")
                && event.event_type == EventType::CommandOutput
        })
        .expect("legacy function output should be canonicalized");
    assert_eq!(
        legacy
            .payload
            .pointer("/body/exit_code")
            .and_then(Value::as_i64),
        Some(0)
    );

    let wait = events
        .iter()
        .find(|event| {
            event
                .payload
                .pointer("/body/call_id")
                .and_then(Value::as_str)
                == Some("call-wait")
                && event.event_type == EventType::ToolOutput
        })
        .expect("running wait output should retain its call linkage");
    assert!(wait
        .payload
        .pointer("/body/exit_code")
        .is_none_or(Value::is_null));
    assert_eq!(
        wait.payload
            .pointer("/body/timed_out")
            .and_then(Value::as_bool),
        Some(false)
    );
}

#[test]
fn codex_current_nested_output_has_full_content_ref_without_output_text() {
    let payload = json!({
        "type": "custom_tool_call_output",
        "call_id": "call-bounded",
        "output": [
            {"type": "input_text", "text": "Script completed\nWall time 0.1 seconds\nOutput:\n"},
            {"type": "input_text", "text": format!("[branch db817fa] fixture\n{}", "x".repeat(8_000))}
        ]
    });
    let event = codex_tool_output_event(
        &payload,
        12,
        DateTime::parse_from_rfc3339("2026-07-18T12:00:02Z")
            .unwrap()
            .with_timezone(&Utc),
        &std::collections::BTreeMap::new(),
    )
    .expect("successful nested output should be retained");
    let normalized = codex_output_text(&payload["output"]);
    let content_ref =
        serde_json::from_value::<ContentRef>(event.payload["result_content_ref"].clone()).unwrap();
    assert_eq!(
        content_ref,
        ContentRef::from_bytes(normalized.as_bytes()).unwrap()
    );
    assert!(!event.payload.to_string().contains("[branch db817fa]"));
    assert!(event.payload.get("output_preview").is_none());
    assert_eq!(event.payload["exit_code"], 0);
    assert_eq!(event.payload["duration_ms"], 100);
}

#[test]
fn result_projection_reuses_one_typed_identity_and_borrows_plain_strings() {
    let plain_payload = json!({
        "type": "function_call_output",
        "call_id": "call-plain",
        "output": "plain output body"
    });
    assert!(matches!(
        codex_result_content(&plain_payload),
        Some(Cow::Borrowed("plain output body"))
    ));

    let projected = codex_tool_output_projection(
        &plain_payload,
        4,
        "2026-07-21T00:00:00Z".parse().unwrap(),
        &std::collections::BTreeMap::new(),
    )
    .unwrap();
    let serialized =
        serde_json::from_value::<ContentRef>(projected.event.payload["result_content_ref"].clone())
            .unwrap();
    assert_eq!(projected.result_content_ref.as_ref(), Some(&serialized));
    assert_eq!(
        projected.event.payload["output_bytes"],
        serialized.byte_len()
    );

    let structured_payload = json!({
        "type": "custom_tool_call_output",
        "call_id": "call-structured",
        "output": [{"type": "input_text", "text": "structured output"}]
    });
    assert!(matches!(
        codex_result_content(&structured_payload),
        Some(Cow::Owned(_))
    ));
}

#[test]
fn codex_structured_failed_tool_output_keeps_outcome_without_body() {
    let payload = json!({
        "type": "function_call_output",
        "call_id": "call-structured-failure",
        "output": {
            "message": {
                "exitCode": 1,
                "output": "structured failed output oracle"
            }
        }
    });
    let event = codex_tool_output_event(
        &payload,
        12,
        DateTime::parse_from_rfc3339("2026-06-24T01:00:04.500Z")
            .unwrap()
            .with_timezone(&Utc),
        &std::collections::BTreeMap::new(),
    )
    .expect("structured failed output should be retained");

    assert_eq!(event.event_type, EventType::ToolOutput);
    let rendered = event.payload.to_string();
    assert!(!rendered.contains("structured failed output oracle"));
    assert!(rendered.contains("failure"));
    assert!(event.payload.get("result_content_ref").is_some());
}

#[test]
fn codex_failed_diff_output_keeps_only_outcome_and_content_ref() {
    let payload = json!({
        "type": "function_call_output",
        "call_id": "call-failed-diff",
        "output": "Process exited with code 1\nOutput:\ndiff --git a/src/lib.rs b/src/lib.rs\n@@\n-old raw diff\n+new raw diff\n"
    });
    let event = codex_tool_output_event(
        &payload,
        13,
        DateTime::parse_from_rfc3339("2026-06-24T01:00:05.000Z")
            .unwrap()
            .with_timezone(&Utc),
        &std::collections::BTreeMap::new(),
    )
    .expect("failed diff output should keep a diagnostic event");

    let rendered = event.payload.to_string();
    assert!(rendered.contains("failure"));
    assert!(event.payload.get("result_content_ref").is_some());
    assert!(!rendered.contains("diff --git"));
    assert!(!rendered.contains("old raw diff"));
    assert!(!rendered.contains("new raw diff"));
}

#[test]
fn codex_result_digest_reuse_matches_source_backed_base_for_all_output_shapes() {
    use sha2::Digest as _;

    use crate::complete_content::{
        jsonl::JsonlCompleteContentResolver, PersistedCompleteContentLocatorV1,
        ResultContentRequest, SourceSnapshot, RESULT_CONTENT_LOCATOR_METADATA_KEY,
    };

    let fixture_root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_result_digest_reuse");
    let source_bytes = fs::read(fixture_root.join("source.jsonl")).unwrap();
    let temp = tempdir();
    let source_path = temp.path().join("source.jsonl");
    fs::write(&source_path, &source_bytes).unwrap();
    let records = source_bytes
        .split(|byte| *byte == b'\n')
        .filter(|record| !record.is_empty())
        .map(|record| serde_json::from_slice::<Value>(record).unwrap())
        .collect::<Vec<_>>();
    let expected: Value =
        serde_json::from_slice(&fs::read(fixture_root.join("expected.json")).unwrap()).unwrap();
    assert_eq!(
        expected["base_commit"],
        "1a529c8ab65e35b184ecc3a5b17fa52df349c8ef"
    );
    let cases = expected["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 4);

    for case in cases {
        let name = case["name"].as_str().unwrap();
        let line_number = case["line"].as_u64().unwrap() as usize;
        let record = &records[line_number - 1];
        let payload = &record["payload"];
        let normalized_content = codex_output_text(&payload["output"]);
        assert_eq!(
            normalized_content.as_ref(),
            case["normalized_content"].as_str().unwrap(),
            "{name} content"
        );
        let expected_content_ref =
            serde_json::from_value::<ContentRef>(case["content_ref"].clone()).unwrap();
        assert_eq!(
            ContentRef::from_bytes(normalized_content.as_bytes()).as_ref(),
            Some(&expected_content_ref),
            "{name} independently computed ContentRef"
        );

        let event = codex_tool_output_event(
            payload,
            line_number,
            record["timestamp"].as_str().unwrap().parse().unwrap(),
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(event.payload, case["normalized_payload"], "{name} payload");
        assert_eq!(
            compute_payload_hash(&event.payload).unwrap(),
            case["payload_hash"].as_str().unwrap(),
            "{name} payload hash"
        );
        assert_eq!(event.provider_event_index, (line_number - 1) as u64);
        assert_eq!(
            event.cursor.as_deref(),
            Some(format!("line:{line_number}").as_str())
        );
        assert_eq!(
            event.idempotency_key.as_deref(),
            Some(format!("provider-event:codex-session:{line_number}").as_str())
        );
        assert_eq!(event.event_type, EventType::ToolOutput);
        assert_eq!(event.role, Some(EventRole::Tool));
    }

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &source_path,
        &mut store,
        CodexSessionImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, cases.len());

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-result-digest-reuse");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), cases.len());
    let source_id = events[0].capture_source_id.unwrap();
    let source = store.get_capture_source(source_id).unwrap();
    let source_identity = source.descriptor.source_identity.clone().unwrap();
    assert_eq!(
        session_id,
        provider_source_session_uuid(&source_identity, "codex-result-digest-reuse")
    );

    let source_records = source_bytes
        .split(|byte| *byte == b'\n')
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut replay_requests = Vec::with_capacity(cases.len());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let call_id = case["call_id"].as_str().unwrap();
        let line_number = case["line"].as_u64().unwrap() as usize;
        let provider_event_index = (line_number - 1) as u64;
        let event = events
            .iter()
            .find(|event| {
                event
                    .payload
                    .pointer("/body/call_id")
                    .and_then(Value::as_str)
                    == Some(call_id)
            })
            .unwrap();
        let payload_hash = case["payload_hash"].as_str().unwrap();
        let expected_content_ref =
            serde_json::from_value::<ContentRef>(case["content_ref"].clone()).unwrap();

        assert_eq!(
            event.id,
            provider_source_event_uuid(source_id, provider_event_index),
            "{name} event UUID"
        );
        assert_eq!(
            event.seq,
            provider_source_event_seq(source_id, provider_event_index),
            "{name} event sequence"
        );
        assert_eq!(
            event.dedupe_key.as_deref(),
            Some(
                format!("provider-source:{source_id}:{provider_event_index}:{payload_hash}")
                    .as_str()
            ),
            "{name} dedupe identity"
        );
        assert_eq!(
            event.payload,
            json!({
                "provider": "codex",
                "provider_session_id": "codex-result-digest-reuse",
                "provider_event_index": provider_event_index,
                "provider_event_hash": payload_hash,
                "cursor": format!("line:{line_number}"),
                "artifacts": [],
                "body": case["persisted_body"].clone(),
            }),
            "{name} stored payload"
        );
        assert_eq!(
            event.sync.metadata["provider_event_hash"], payload_hash,
            "{name} stored payload hash"
        );
        assert_eq!(
            event.sync.metadata["provider_event_hash_authority"],
            "normalized_payload_fallback"
        );
        assert_eq!(
            event.sync.metadata["source_record_ordinal"],
            provider_event_index
        );
        assert_eq!(event.sync.metadata["source_record_subrecord_index"], 0);
        assert_eq!(
            event.sync.metadata[RESULT_CONTENT_LOCATOR_METADATA_KEY], case["locator"],
            "{name} exact locator"
        );

        let locator = PersistedCompleteContentLocatorV1::from_metadata_value(
            &event.sync.metadata[RESULT_CONTENT_LOCATOR_METADATA_KEY],
        )
        .unwrap();
        let independently_computed_record_digest = format!(
            "{:x}",
            sha2::Sha256::digest(source_records[line_number - 1])
        );
        assert_eq!(
            locator.record_sha256().as_str(),
            independently_computed_record_digest,
            "{name} record digest"
        );
        assert_eq!(
            locator.body_sha256().as_str(),
            expected_content_ref.sha256(),
            "{name} locator/ContentRef digest"
        );
        assert_eq!(
            event.payload["body"]["output_bytes"],
            expected_content_ref.byte_len()
        );

        replay_requests.push(ResultContentRequest {
            event_id: event.id,
            provider: source.descriptor.provider,
            source_format: source.descriptor.source_format.clone().unwrap(),
            raw_source_path: source.descriptor.raw_source_path.clone().unwrap().into(),
            source_root: source.descriptor.source_root.clone().map(Into::into),
            source_identity: Some(source_identity.clone()),
            source_locator: locator.source_locator().unwrap(),
            source_snapshot: SourceSnapshot::default(),
            source_record_ordinal: provider_event_index,
            source_record_subrecord_index: 0,
            expected_record_digest: locator.record_sha256().clone(),
            expected_content_ref,
        });
    }

    let replayed = JsonlCompleteContentResolver::new().resolve_results(&replay_requests);
    for ((case, request), replayed) in cases.iter().zip(&replay_requests).zip(replayed) {
        let name = case["name"].as_str().unwrap();
        let replayed = replayed.unwrap();
        assert_eq!(replayed.event_id, request.event_id, "{name} replay event");
        assert_eq!(
            replayed.content,
            case["normalized_content"].as_str().unwrap(),
            "{name} replay content"
        );
        assert_eq!(
            replayed.content_ref, request.expected_content_ref,
            "{name} replay ContentRef"
        );
        assert!(replayed.verification.is_verified(), "{name} replay proof");
    }

    let replay_summary = import_codex_session_jsonl(
        &source_path,
        &mut store,
        CodexSessionImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replay_summary.failed, 0, "{:?}", replay_summary.failures);
    assert_eq!(
        store.events_for_session(session_id).unwrap(),
        events,
        "re-import must preserve deterministic identities and payloads"
    );
}

#[test]
fn codex_nested_failed_diff_output_keeps_only_outcome_and_content_ref() {
    let payload = json!({
        "type": "function_call_output",
        "call_id": "call-nested-failed-diff",
        "output": {
            "message": {
                "exitCode": 1,
                "output": "@@ -1 +1\n-old nested diff\n+new nested diff\n"
            }
        }
    });
    let event = codex_tool_output_event(
        &payload,
        14,
        DateTime::parse_from_rfc3339("2026-06-24T01:00:05.500Z")
            .unwrap()
            .with_timezone(&Utc),
        &std::collections::BTreeMap::new(),
    )
    .expect("nested failed diff output should keep a diagnostic event");

    let rendered = event.payload.to_string();
    assert!(rendered.contains("failure"));
    assert!(event.payload.get("result_content_ref").is_some());
    assert!(!rendered.contains("old nested diff"));
    assert!(!rendered.contains("new nested diff"));
}

#[test]
fn codex_default_policy_persists_file_touches_without_raw_patch_text() {
    let temp = tempdir();
    let root = temp.path().join("codex-sessions/2026/06/24");
    fs::create_dir_all(&root).unwrap();
    let fixture = root.join("search-file-touch.jsonl");
    fs::write(
            &fixture,
            concat!(
                "{\"timestamp\":\"2026-06-24T01:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-search-file-touch\",\"cwd\":\"/workspace/ctx\"}}\n",
                "{\"timestamp\":\"2026-06-24T01:00:01.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Please update the CLI.\"}]}}\n",
                "{\"timestamp\":\"2026-06-24T01:00:02.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"name\":\"apply_patch\",\"input\":\"*** Begin Patch\\n*** Update File: crates/ctx-cli/src/main.rs\\n@@\\n-old\\n+new\\n*** End Patch\",\"call_id\":\"call-patch\",\"status\":\"completed\"}}\n",
            ),
        )
        .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_tree(
        temp.path().join("codex-sessions"),
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(temp.path().join("codex-sessions")),
            imported_at: "2026-06-24T02:00:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 2);

    let session_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-search-file-touch");
    let events = store.events_for_session(session_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, EventType::Message);
    assert_eq!(events[1].event_type, EventType::ToolCall);
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("file touches: modified:crates/ctx-cli/src/main.rs"));
    assert!(!rendered.contains("*** Begin Patch"));
    assert!(!rendered.contains("-old"));
    assert!(!rendered.contains("+new"));

    let archive = store.export_archive().unwrap();
    let touched = archive
        .files_touched
        .iter()
        .find(|file| file.path == "crates/ctx-cli/src/main.rs")
        .expect("apply_patch should create file touch metadata");
    assert_eq!(touched.change_kind, Some(FileChangeKind::Modified));
    assert!(touched.event_id.is_some());
    assert_eq!(touched.history_record_id, None);
}

#[test]
fn codex_default_policy_omits_non_patch_edit_tool_arguments() {
    let temp = tempdir();
    let root = temp.path().join("codex-sessions/2026/06/24");
    fs::create_dir_all(&root).unwrap();
    let fixture = root.join("edit-tool.jsonl");
    fs::write(
        &fixture,
        concat!(
            "{\"timestamp\":\"2026-06-24T01:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-edit-tool\",\"cwd\":\"/workspace/ctx\"}}\n",
            "{\"timestamp\":\"2026-06-24T01:00:01.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Please edit the file.\"}]}}\n",
            "{\"timestamp\":\"2026-06-24T01:00:02.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"edit_file\",\"arguments\":{\"path\":\"src/edit_tool.rs\",\"old_string\":\"old-edit-tool-secret\",\"new_string\":\"new-edit-tool-secret\"},\"call_id\":\"call-edit\"}}\n",
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_tree(
        temp.path().join("codex-sessions"),
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(temp.path().join("codex-sessions")),
            imported_at: "2026-06-24T02:00:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 2);

    let session_id = stored_provider_session_id(&store, CaptureProvider::Codex, "codex-edit-tool");
    let events = store.events_for_session(session_id).unwrap();
    let rendered = serde_json::to_string(&events).unwrap();
    assert!(rendered.contains("file touches:"));
    assert!(rendered.contains("src/edit_tool.rs"));
    assert!(!rendered.contains("old-edit-tool-secret"));
    assert!(!rendered.contains("new-edit-tool-secret"));
    assert!(store
        .search_event_hits("old-edit-tool-secret", 10)
        .unwrap()
        .is_empty());

    let archive = store.export_archive().unwrap();
    assert!(archive
        .files_touched
        .iter()
        .any(|file| file.path == "src/edit_tool.rs"));
}

#[test]
fn structured_file_touch_extractor_reads_nested_provider_paths() {
    let event = ProviderEventEnvelope {
        provider_event_index: 7,
        provider_event_hash: None,
        cursor: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-06-24T01:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: serde_json::json!({}),
        metadata: serde_json::json!({}),
    };
    let antigravity = serde_json::json!({
        "type": "CODE_ACTION",
        "tool_calls": [{
            "name": "write_to_file",
            "args": {
                "TargetFile": "/workspace/demo/README.md",
                "CodeContent": "# Demo\n"
            }
        }]
    });
    let cursor = serde_json::json!({
        "role": "assistant",
        "message": {
            "content": [{
                "type": "tool_use",
                "name": "write_file",
                "input": {
                    "path": "cursor-native-cli-oracle.txt",
                    "content": "proof"
                }
            }]
        }
    });

    let antigravity_touches = provider_file_touches_from_raw_value(
        CaptureProvider::Antigravity,
        "agy-session",
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
        None,
        &antigravity,
        &event,
        1,
    );
    let cursor_touches = provider_file_touches_from_raw_value(
        CaptureProvider::Cursor,
        "cursor-session",
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        None,
        &cursor,
        &event,
        1,
    );

    assert_eq!(antigravity_touches[0].1.path, "/workspace/demo/README.md");
    assert_eq!(
        antigravity_touches[0].1.change_kind,
        Some(FileChangeKind::Created)
    );
    assert_eq!(cursor_touches[0].1.path, "cursor-native-cli-oracle.txt");
    assert_eq!(
        cursor_touches[0].1.change_kind,
        Some(FileChangeKind::Created)
    );
}

#[test]
fn structured_file_touch_extractor_covers_provider_tool_shapes() {
    let event = ProviderEventEnvelope {
        provider_event_index: 11,
        provider_event_hash: None,
        cursor: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-06-24T01:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: serde_json::json!({}),
        metadata: serde_json::json!({}),
    };

    for (provider, source_format, raw, expected_path) in [
        (
            CaptureProvider::Claude,
            CLAUDE_PROJECTS_SOURCE_FORMAT,
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "name": "Edit",
                        "input": {"file_path": "src/claude_file.rs"}
                    }]
                }
            }),
            "src/claude_file.rs",
        ),
        (
            CaptureProvider::OpenCode,
            OPENCODE_SQLITE_SOURCE_FORMAT,
            serde_json::json!({
                "content": [{
                    "type": "tool",
                    "name": "write",
                    "input": {"file": "src/opencode_file.rs"}
                }]
            }),
            "src/opencode_file.rs",
        ),
        (
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            serde_json::json!({
                "type": "gemini",
                "toolCalls": [{
                    "name": "write_file",
                    "args": {"path": "src/gemini_file.rs", "content": "proof"}
                }]
            }),
            "src/gemini_file.rs",
        ),
        (
            CaptureProvider::CopilotCli,
            COPILOT_CLI_SOURCE_FORMAT,
            serde_json::json!({
                "type": "tool.execution_start",
                "data": {
                    "toolName": "write_file",
                    "args": {"path": "src/copilot_file.rs"}
                }
            }),
            "src/copilot_file.rs",
        ),
        (
            CaptureProvider::FactoryAiDroid,
            FACTORY_DROID_SOURCE_FORMAT,
            serde_json::json!({
                "type": "message",
                "content": [{
                    "type": "tool_use",
                    "name": "write_file",
                    "input": {"path": "src/droid_file.rs"}
                }]
            }),
            "src/droid_file.rs",
        ),
        (
            CaptureProvider::ForgeCode,
            FORGECODE_SQLITE_SOURCE_FORMAT,
            serde_json::json!({
                "message": {
                    "text": {
                        "tool_calls": [{
                            "name": "write",
                            "arguments": {
                                "path": "src/forge_file.rs",
                                "content": "proof"
                            }
                        }]
                    }
                }
            }),
            "src/forge_file.rs",
        ),
    ] {
        let touches = provider_file_touches_from_raw_value(
            provider,
            "provider-session",
            source_format,
            None,
            &raw,
            &event,
            1,
        );
        assert_eq!(
            touches.first().map(|(_, file)| file.path.as_str()),
            Some(expected_path),
            "{provider:?} should extract an explicit tool file path"
        );
    }
}
