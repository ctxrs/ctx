use crate::tests::support::fixtures::jsonl::write_nanoclaw_smoke_project;
use crate::tests::support::paths::tempdir;
use crate::tests::support::provider_state::stored_provider_session_id;
use crate::{
    import_codebuddy_history, import_junie_history, import_nanoclaw_project,
    provider_source_for_path, CodeBuddyImportOptions,
    JunieImportOptions, NanoClawImportOptions, ProviderSourceStatus, CODEBUDDY_SOURCE_FORMAT,
    JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
};
use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs;

#[test]
fn native_codebuddy_cli_jsonl_imports_searches_and_reimports() {
    let temp = tempdir();
    let fixture = temp
        .path()
        .join("codebuddy-cli/.codebuddy/projects/sanitized-workspace");
    fs::create_dir_all(&fixture).unwrap();
    fs::write(
        fixture.join("codebuddy-cli-native.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({
                "id": "codebuddy-cli-user",
                "timestamp": 1783170001000i64,
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "codebuddy cli jsonl oracle prompt"}],
                "providerData": {"agent": "cli"},
                "sessionId": "codebuddy-cli-native",
                "cwd": "/workspace/codebuddy"
            }),
            json!({
                "id": "codebuddy-cli-snapshot",
                "timestamp": 1783170001500i64,
                "type": "file-history-snapshot",
                "isSnapshotUpdate": false,
                "snapshot": {"messageId": "codebuddy-cli-user", "trackedFileBackups": {}},
                "sessionId": "codebuddy-cli-native",
                "cwd": "/workspace/codebuddy"
            }),
            json!({
                "id": "codebuddy-cli-assistant",
                "parentId": "codebuddy-cli-user",
                "timestamp": 1783170002000i64,
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "CodeBuddy CLI JSONL native import ok"}],
                "providerData": {
                    "model": "tencent/hy3-20260706:free",
                    "requestModelId": "custom-local:tencent/hy3:free",
                    "requestModelName": "OpenRouter Tencent Hunyuan Free",
                    "agent": "cli"
                },
                "sessionId": "codebuddy-cli-native",
                "message": {"usage": {"input_tokens": 11, "output_tokens": 13, "total_tokens": 24}},
                "cwd": "/workspace/codebuddy"
            })
        ),
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source = temp.path().join("codebuddy-cli/.codebuddy");
    let first = import_codebuddy_history(
        &source,
        &mut store,
        CodeBuddyImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(source.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-04T16:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 1);
    assert_eq!(first.imported_events, 2);

    let session = stored_provider_session_id(
        &store,
        CaptureProvider::CodeBuddy,
        "sanitized-workspace/codebuddy-cli-native",
    );
    let stored_session = store.get_session(session).unwrap();
    assert_eq!(
        stored_session.sync.metadata["metadata"]["native_shape"].as_str(),
        Some("cli_jsonl")
    );
    let session_index: Value = serde_json::from_str(
        stored_session.sync.metadata["metadata"]["session_index"]["json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(session_index["rows"].as_u64(), Some(3));
    assert!(stored_session.sync.metadata["metadata"]["limitations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .any(|limitation| limitation.contains("Non-message CLI JSONL rows are not imported")));
    let capture_source = store
        .capture_source_by_external_session(
            CaptureProvider::CodeBuddy,
            "sanitized-workspace/codebuddy-cli-native",
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        capture_source.descriptor.cwd.as_deref(),
        Some("/workspace/codebuddy")
    );
    assert!(capture_source.sync.metadata["metadata"]["source_metadata"]["schema_proof"].is_null());
    let events = store.events_for_session(session).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].role, Some(EventRole::User));
    assert_eq!(events[1].role, Some(EventRole::Assistant));
    assert_eq!(
        events[1].sync.metadata["metadata"]["source"].as_str(),
        Some("codebuddy_cli_jsonl")
    );
    assert_eq!(
        events[1]
            .sync
            .metadata
            .pointer("/metadata/model")
            .and_then(Value::as_str),
        Some("tencent/hy3-20260706:free")
    );
    assert!(store
        .search_event_hits("CodeBuddy CLI JSONL native import ok", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::CodeBuddy)));
    let source_status = provider_source_for_path(CaptureProvider::CodeBuddy, source.clone());
    assert_eq!(source_status.source_format, CODEBUDDY_SOURCE_FORMAT);
    assert_eq!(source_status.status, ProviderSourceStatus::Available);

    let second = import_codebuddy_history(
        &source,
        &mut store,
        CodeBuddyImportOptions {
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 1);
    assert_eq!(second.skipped_events, 2);
}

#[test]
fn native_codebuddy_rejects_message_id_path_traversal() {
    let temp = tempdir();
    let project = temp.path().join("codebuddy/project");
    let session = project.join("session-traversal");
    fs::create_dir_all(session.join("messages")).unwrap();
    fs::write(
        session.join("index.json"),
        json!({"messages": [{"id": "../../outside", "role": "user"}]}).to_string(),
    )
    .unwrap();
    fs::write(
        project.join("outside.json"),
        json!({"content": "codebuddy traversal content must not import"}).to_string(),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codebuddy_history(
        &project,
        &mut store,
        CodeBuddyImportOptions {
            ..CodeBuddyImportOptions::default()
        },
    )
    .unwrap();

    assert!(summary.failed >= 1, "{:?}", summary.failures);
    assert!(summary
        .failures
        .iter()
        .any(|failure| failure.error.contains("not a safe path segment")));
    assert!(store
        .search_event_hits("codebuddy traversal content must not import", 10)
        .unwrap()
        .is_empty());
    assert!(store.list_sessions().unwrap().is_empty());
}

#[test]
fn native_nanoclaw_rejects_database_path_traversal() {
    let temp = tempdir();
    let root = write_nanoclaw_smoke_project(&temp, "nanoclaw safe session oracle");
    let central = Connection::open(root.join("data/v2.db")).unwrap();
    central
        .execute(
            "INSERT INTO agent_groups VALUES ('../../outside', 'Outside', '/outside', 'codex')",
            [],
        )
        .unwrap();
    central
        .execute(
            "INSERT INTO sessions VALUES (
                'escaped', '../../outside', NULL, NULL, 'codex', 'active',
                'running', 1782259203000, 1782259203000
            )",
            [],
        )
        .unwrap();
    drop(central);

    let outside = root.join("outside/escaped");
    fs::create_dir_all(&outside).unwrap();
    let inbound = Connection::open(outside.join("inbound.db")).unwrap();
    inbound
        .execute_batch("CREATE TABLE messages_in (id TEXT PRIMARY KEY, content TEXT);")
        .unwrap();
    inbound
        .execute(
            "INSERT INTO messages_in VALUES ('escaped-message', ?1)",
            [json!({"text": "nanoclaw-traversal-sentinel"}).to_string()],
        )
        .unwrap();
    drop(inbound);

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_nanoclaw_project(
        &root,
        &mut store,
        NanoClawImportOptions {
            ..NanoClawImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert!(summary.failures[0]
        .error
        .contains("identifiers are not safe path segments"));
    assert!(store
        .search_event_hits("nanoclaw-traversal-sentinel", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn native_junie_current_cli_failure_sessions_import_and_search() {
    let temp = tempdir();
    let sessions = temp.path().join("junie-current/sessions");
    let indexed_session = sessions.join("session-260709-212712-hq1w");
    let failure_session = sessions.join("session-260709-212620-18se");
    fs::create_dir_all(&indexed_session).unwrap();
    fs::create_dir_all(&failure_session).unwrap();
    fs::write(
        sessions.join("index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": "session-260709-212712-hq1w",
                "createdAt": 1783650432344i64,
                "updatedAt": 1783650440849i64,
                "projectDir": "/tmp/ctx-junie-proxy-openrouter-router/project",
                "taskName": "Answer exact code, no file edits, no shell commands",
                "status": "Sending LLM request"
            })
        ),
    )
    .unwrap();
    fs::write(
        indexed_session.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "kind": "TaskStartedEvent",
                "taskId": "task-260709-212711-1ov9",
                "timestampMs": 1783650432366i64
            }),
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1783650435508i64,
                "event": {
                    "state": "IN_PROGRESS",
                    "agentEvent": {
                        "kind": "LlmResponseMetadataEvent",
                        "agent": { "kind": "MainAgent", "id": "main", "name": "main" },
                        "modelUsage": [{
                            "model": "openrouter/free",
                            "cost": 0.0,
                            "inputTokens": 12041,
                            "cacheInputTokens": 0,
                            "cacheCreateTokens": 0,
                            "outputTokens": 121,
                            "time": 0
                        }]
                    }
                }
            })
        ),
    )
    .unwrap();
    fs::write(
        failure_session.join("events.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({
                "kind": "TaskStartedEvent",
                "taskId": "task-260709-212620-svyz",
                "timestampMs": 1783650380750i64
            }),
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1783650390610i64,
                "event": {
                    "state": "FAILED",
                    "agentEvent": {
                        "kind": "AgentFailureEvent",
                        "agent": { "kind": "MainAgent", "id": "main", "name": "main" },
                        "message": "OpenAI: Can not parse response. JSON input: {\"solution_summary\": \"junie-real-openrouter-free-ok</arg_value:",
                        "errorCode": "ExitEarly"
                    }
                }
            }),
            json!({
                "kind": "SessionA2uxEvent",
                "timestampMs": 1783650390611i64,
                "event": {
                    "state": "FAILED",
                    "agentEvent": {
                        "kind": "AgentFailureEvent",
                        "agent": { "kind": "MainAgent", "id": "main", "name": "main" },
                        "message": "junie-second-failure-oracle",
                        "errorCode": "ExitEarly"
                    }
                }
            })
        ),
    )
    .unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let source = provider_source_for_path(CaptureProvider::Junie, sessions.clone());
    assert_eq!(source.source_format, JUNIE_SESSION_EVENTS_SOURCE_FORMAT);
    assert_eq!(source.status, ProviderSourceStatus::Available);

    let first = import_junie_history(
        &sessions,
        &mut store,
        JunieImportOptions {
            machine_id: "test-machine".into(),
            source_path: Some(sessions.clone()),
            imported_at: DateTime::parse_from_rfc3339("2026-07-10T03:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            ..JunieImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 1);
    assert!(store
        .search_event_hits("junie-real-openrouter-free-ok", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Junie)));
    assert!(store
        .search_event_hits("junie-second-failure-oracle", 10)
        .unwrap()
        .iter()
        .any(|hit| hit.provider == Some(CaptureProvider::Junie)));

    let second = import_junie_history(
        &sessions,
        &mut store,
        JunieImportOptions {
            ..JunieImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.skipped_sessions, 0);
    assert_eq!(second.skipped_events, 1);
}
