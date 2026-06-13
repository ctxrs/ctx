use ctx_core::models::SessionEventType;
use serde_json::json;

use super::{
    preview::{build_text_preview, TOOL_PREVIEW_MAX_LINES, TOOL_PREVIEW_MAX_LINE_CHARS},
    projections::{
        build_tool_ops_meta, build_turn_tool_update_from_payload, sanitize_tool_event_payload,
    },
};

#[test]
fn preview_truncates_lines_and_counts() {
    let lines: Vec<String> = (1..=10).map(|i| format!("line-{i}")).collect();
    let text = lines.join("\n");
    let preview = build_text_preview(&text);
    assert!(preview.truncated);
    assert_eq!(preview.preview.lines().count(), TOOL_PREVIEW_MAX_LINES);
    assert!(preview.preview.contains("... +6 lines"));
}

#[test]
fn preview_truncates_long_lines() {
    let text = "x".repeat(TOOL_PREVIEW_MAX_LINE_CHARS + 20);
    let preview = build_text_preview(&text);
    assert!(preview.truncated);
    assert_eq!(preview.preview.chars().count(), TOOL_PREVIEW_MAX_LINE_CHARS);
}

#[test]
fn sanitize_tool_payload_keeps_bounded_preview_and_artifact_ref() {
    let raw = json!({
        "tool_call_id": "call-1",
        "output_text": "line1\nline2\nline3\nline4\nline5\nline6"
    });
    let artifact = super::projections::ToolOutputArtifactRef {
        artifact_id: "artifact-1".to_string(),
        name: Some("tool-output-call-1.txt".to_string()),
        mime_type: "text/plain".to_string(),
        bytes: 35,
    };
    let sanitized =
        sanitize_tool_event_payload(&SessionEventType::ToolResult, &raw, Some(&artifact));
    assert!(sanitized.get("output_text").is_none());
    assert_eq!(
        sanitized
            .get("output_artifact")
            .and_then(|value| value.get("artifact_id"))
            .and_then(|value| value.as_str()),
        Some("artifact-1")
    );
    assert!(sanitized.get("output_preview").is_some());
    assert_eq!(
        sanitized
            .get("output_truncated")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
}

#[test]
fn tool_title_falls_back_to_provider_tool_name_consistently() {
    let raw = json!({
        "tool_call_id": "call-2",
        "kind": "execute",
        "toolCall": {
            "name": "Bash",
            "kind": "execute",
        },
        "status": "running",
        "rawInput": { "command": "pwd" }
    });

    let sanitized = sanitize_tool_event_payload(&SessionEventType::ToolCall, &raw, None);
    assert_eq!(
        sanitized.get("title").and_then(|value| value.as_str()),
        Some("Bash")
    );
    assert_eq!(
        sanitized.get("subtitle").and_then(|value| value.as_str()),
        Some("pwd")
    );
    assert_eq!(
        sanitized.get("tool_name").and_then(|value| value.as_str()),
        Some("Bash")
    );

    let meta = build_tool_ops_meta(&SessionEventType::ToolCall, &raw);
    assert_eq!(meta.title.as_deref(), Some("Bash"));

    let update = build_turn_tool_update_from_payload(&SessionEventType::ToolCall, &raw)
        .expect("tool update");
    assert_eq!(update.title.as_deref(), Some("Bash"));
    assert_eq!(update.subtitle.as_deref(), Some("pwd"));
}

#[test]
fn tool_title_prefers_explicit_labels_over_raw_name() {
    let raw = json!({
        "tool_call_id": "call-3",
        "kind": "execute",
        "tool_label": "Run shell",
        "toolCall": {
            "name": "Bash",
            "title": "Nested title",
        },
        "rawInput": {
            "description": "Run shell command"
        }
    });

    let sanitized = sanitize_tool_event_payload(&SessionEventType::ToolCall, &raw, None);
    assert_eq!(
        sanitized.get("title").and_then(|value| value.as_str()),
        Some("Run shell")
    );

    let meta = build_tool_ops_meta(&SessionEventType::ToolCall, &raw);
    assert_eq!(meta.title.as_deref(), Some("Run shell"));

    let update = build_turn_tool_update_from_payload(&SessionEventType::ToolCall, &raw)
        .expect("tool update");
    assert_eq!(update.title.as_deref(), Some("Run shell"));
    assert_eq!(update.subtitle.as_deref(), Some("Run shell command"));
}

#[test]
fn sanitize_tool_payload_infers_claude_read_from_numbered_output_preview() {
    let raw = json!({
        "tool_call_id": "toolu_read_1",
        "kind": "unknown",
        "tool_name": "unknown",
        "title": "unknown",
        "status": "completed",
        "output_preview": "1→# agent instructions\n2→follow the rules"
    });

    let sanitized = sanitize_tool_event_payload(&SessionEventType::ToolResult, &raw, None);
    assert_eq!(
        sanitized.get("kind").and_then(|value| value.as_str()),
        Some("read")
    );
    assert_eq!(
        sanitized.get("tool_name").and_then(|value| value.as_str()),
        Some("Read")
    );
    assert_eq!(
        sanitized.get("title").and_then(|value| value.as_str()),
        Some("Read")
    );

    let update = build_turn_tool_update_from_payload(&SessionEventType::ToolResult, &sanitized)
        .expect("tool update");
    assert_eq!(update.tool_kind.as_deref(), Some("read"));
    assert_eq!(update.provider_tool_name.as_deref(), Some("Read"));
    assert_eq!(update.title.as_deref(), Some("Read"));
    assert_eq!(
        update.output_text.as_deref(),
        Some("1→# agent instructions\n2→follow the rules")
    );
}

#[test]
fn sanitize_tool_payload_infers_claude_bash_from_scalar_output_preview() {
    let raw = json!({
        "tool_call_id": "toolu_exec_1",
        "kind": "unknown",
        "tool_name": "unknown",
        "title": "unknown",
        "status": "completed",
        "output_preview": "1"
    });

    let sanitized = sanitize_tool_event_payload(&SessionEventType::ToolResult, &raw, None);
    assert_eq!(
        sanitized.get("kind").and_then(|value| value.as_str()),
        Some("execute")
    );
    assert_eq!(
        sanitized.get("tool_name").and_then(|value| value.as_str()),
        Some("Bash")
    );
    assert_eq!(
        sanitized.get("title").and_then(|value| value.as_str()),
        Some("Bash")
    );
}
