#[allow(unused_imports)]
use super::*;

pub(crate) fn test_provider_event(event_type: EventType) -> ProviderEventEnvelope {
    ProviderEventEnvelope {
        provider_event_index: 0,
        provider_event_hash: Some("event-hash".to_owned()),
        cursor: None,
        event_type,
        role: Some(EventRole::Tool),
        occurred_at: "2026-07-03T12:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: json!({}),
        metadata: json!({}),
    }
}

#[test]
pub(crate) fn structured_file_touch_extractor_reads_nested_provider_paths() {
    let event = ProviderEventEnvelope {
        provider_event_index: 7,
        provider_event_hash: None,
        cursor: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-06-24T01:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
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
pub(crate) fn structured_file_touch_extractor_covers_provider_tool_shapes() {
    let event = ProviderEventEnvelope {
        provider_event_index: 11,
        provider_event_hash: None,
        cursor: None,
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at: "2026-06-24T01:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
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

#[test]
pub(crate) fn custom_history_jsonl_reader_import_persists_normalized_cursor() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let input = [
        r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
        r#"{"record_type":"source","source_id":"src","provider_key":"stream-agent","source_format":"stream-v1","cursor":{"after":{"stream":"native-stream","cursor":"{\"message_id\":7}","observed_at":"2026-07-01T12:00:00Z"}}}"#,
        r#"{"record_type":"session","source_id":"src","session_id":"run","started_at":"2026-07-01T11:59:00Z"}"#,
        r#"{"record_type":"event","source_id":"src","session_id":"run","event_index":0,"event_type":"message","role":"assistant","occurred_at":"2026-07-01T12:00:00Z","preview":"stream import marker"}"#,
    ]
    .join("\n");

    let summary = import_custom_history_jsonl_v1_reader(
        std::io::Cursor::new(input.into_bytes()),
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            source_path: Some(PathBuf::from("plugin://stream-agent/default")),
            imported_at: "2026-07-01T12:01:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let cursor = store
        .get_sync_cursor(
            None,
            &CustomHistoryJsonlV1ImportOptions::default().machine_id,
            &custom_history_jsonl_v1_cursor_stream("stream-agent", "src", "stream-v1"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cursor.cursor, r#"{"message_id":7}"#);
}

#[test]
pub(crate) fn custom_history_jsonl_reader_persists_source_only_cursor() {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let input = [
        r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}"#,
        r#"{"record_type":"source","source_id":"src","provider_key":"stream-agent","source_format":"stream-v1","cursor":{"after":{"stream":"native-stream","cursor":"{\"message_id\":9}","observed_at":"2026-07-01T12:02:00Z"}}}"#,
    ]
    .join("\n");

    let summary = import_custom_history_jsonl_v1_reader(
        std::io::Cursor::new(input.into_bytes()),
        &mut store,
        CustomHistoryJsonlV1ImportOptions {
            imported_at: "2026-07-01T12:03:00Z".parse().unwrap(),
            ..CustomHistoryJsonlV1ImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 0);
    assert_eq!(summary.imported_events, 0);
    let cursor = store
        .get_sync_cursor(
            None,
            &CustomHistoryJsonlV1ImportOptions::default().machine_id,
            &custom_history_jsonl_v1_cursor_stream("stream-agent", "src", "stream-v1"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(cursor.cursor, r#"{"message_id":9}"#);
}
