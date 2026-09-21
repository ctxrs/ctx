use std::borrow::Cow;

use chrono::{DateTime, Utc};
use ctx_history_capture_model::acp::{
    acp_terminal_status as terminal_status, acp_update as update, acp_update_kind as update_kind,
    acp_visible_text as visible_text,
};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokBuildResultSubrecord<'a> {
    pub subrecord_index: u32,
    pub content: Option<Cow<'a, str>>,
    pub call_id: Option<&'a str>,
    pub tool_name: Option<&'a str>,
}

fn envelope_meta(value: &Value) -> Option<&Value> {
    value
        .pointer("/params/_meta")
        .or_else(|| value.get("_meta"))
}

pub fn header_session_id(value: &Value) -> Option<String> {
    value
        .pointer("/params/sessionId")
        .or_else(|| value.get("sessionId"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}

pub fn event_identity(value: &Value) -> Option<&str> {
    envelope_meta(value)
        .and_then(|meta| meta.get("eventId"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
}

pub fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    envelope_meta(value)
        .and_then(|meta| meta.get("agentTimestampMs"))
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .or_else(|| {
            value
                .get("timestamp")
                .and_then(Value::as_i64)
                .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
        })
}

pub fn event_type(value: &Value) -> EventType {
    match update_kind(value) {
        Some("user_message_chunk" | "agent_message_chunk") => EventType::Message,
        Some("agent_thought_chunk" | "plan" | "rewind_marker") => EventType::Summary,
        Some("tool_call") => EventType::ToolCall,
        Some("tool_call_update")
            if terminal_status(value).is_some()
                && grok_build_tool_kind(value) == Some("execute") =>
        {
            EventType::CommandOutput
        }
        Some("tool_call_update") if terminal_status(value).is_some() => EventType::ToolOutput,
        _ => EventType::Notice,
    }
}

pub fn role(value: &Value) -> EventRole {
    match update_kind(value) {
        Some("user_message_chunk") => EventRole::User,
        Some("agent_message_chunk" | "agent_thought_chunk" | "tool_call") => EventRole::Assistant,
        Some("tool_call_update") => EventRole::Tool,
        _ => EventRole::System,
    }
}

pub fn event_text(value: &Value) -> String {
    let update = update(value);
    match update_kind(value) {
        Some("user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk") => update
            .get("content")
            .and_then(visible_text)
            .unwrap_or_default(),
        Some("tool_call") => structured_tool_call_text(value).unwrap_or_default(),
        Some("plan" | "rewind_marker" | "tool_call_update") => update
            .get("content")
            .and_then(visible_text)
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_default(),
        // New ACP update variants are intentionally contentless until their
        // shape has been audited. This prevents a future metadata payload from
        // silently becoming searchable user-visible history.
        Some(_) | None => String::new(),
    }
}

pub fn structured_tool_call_text(value: &Value) -> Option<String> {
    let update = update(value);
    (update_kind(value) == Some("tool_call")).then(|| {
        json!({
            "toolCallId": update.get("toolCallId"),
            "title": update.get("title"),
            "rawInput": update.get("rawInput"),
        })
        .to_string()
    })
}

pub fn enumerate_results(value: &Value) -> Vec<GrokBuildResultSubrecord<'_>> {
    if update_kind(value) != Some("tool_call_update") {
        return Vec::new();
    }
    if terminal_status(value).is_none() {
        return Vec::new();
    }
    let update = update(value);
    let content = grok_build_result_content(update)
        .filter(|text| !text.trim().is_empty())
        .map(Cow::Owned);
    let call_id = update
        .get("toolCallId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty());
    vec![GrokBuildResultSubrecord {
        subrecord_index: 0,
        content,
        call_id,
        tool_name: update.get("title").and_then(Value::as_str),
    }]
}

/// Retains only the audited linkage envelope for a terminal result whose ACP
/// content union has no recognized visible-text variant.
pub fn contentless_result_evidence(value: &Value) -> Value {
    let update = update(value);
    json!({
        "sessionUpdate": update.get("sessionUpdate"),
        "toolCallId": update.get("toolCallId"),
        "status": update.get("status"),
    })
}

fn grok_build_tool_kind(value: &Value) -> Option<&str> {
    update(value)
        .pointer("/_meta/x.ai~1tool/kind")
        .and_then(Value::as_str)
        .or_else(|| update(value).get("kind").and_then(Value::as_str))
        .or_else(|| {
            (update(value)
                .pointer("/rawOutput/type")
                .and_then(Value::as_str)
                == Some("Bash"))
            .then_some("execute")
        })
}

fn grok_build_result_content(update: &Value) -> Option<String> {
    if let Some(content) = update.get("content").filter(|content| !content.is_null()) {
        return visible_text(content).filter(|content| !content.trim().is_empty());
    }
    raw_output_visible_text(update.get("rawOutput")?)
}

fn raw_output_visible_text(raw_output: &Value) -> Option<String> {
    let output_type = raw_output.get("type").and_then(Value::as_str)?;
    let selected = match output_type {
        "ListDir" => [
            "/Content/content",
            "/NotFound",
            "/IsAFile",
            "/NotADirectory",
            "/PermissionDenied",
            "/Error",
        ]
        .into_iter()
        .find_map(|pointer| raw_output.pointer(pointer).and_then(Value::as_str))
        .map(str::to_owned),
        "WebSearch" => raw_output
            .get("pre_formatted")
            .and_then(Value::as_str)
            .or_else(|| raw_output.get("content").and_then(Value::as_str))
            .map(str::to_owned),
        "Todo" => [
            "/TodosUpdated/summary_for_prompt",
            "/DuplicateId",
            "/InvalidArgument",
        ]
        .into_iter()
        .find_map(|pointer| raw_output.pointer(pointer).and_then(Value::as_str))
        .map(str::to_owned),
        "MCP" => ["/output/OkayOutput", "/output/Error"]
            .into_iter()
            .find_map(|pointer| raw_output.pointer(pointer).and_then(Value::as_str))
            .map(str::to_owned),
        _ => None,
    };
    selected.filter(|content| !content.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_real_fixture_preserves_audited_projection_shapes() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../tests/fixtures/provider-history/grok-build/v1.0.3/sessions/synthetic-workspace/01990000-0000-7000-8000-000000000001/updates.jsonl",
        );
        let values = std::fs::read_to_string(&fixture)
            .unwrap_or_else(|error| panic!("read {}: {error}", fixture.display()))
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("valid sanitized Grok JSONL"))
            .collect::<Vec<_>>();

        assert_eq!(values.len(), 21);
        assert!(values.iter().all(|value| {
            header_session_id(value).as_deref() == Some("01990000-0000-7000-8000-000000000001")
        }));
        assert_eq!(
            values
                .iter()
                .filter(|value| event_type(value) == EventType::Message)
                .count(),
            2
        );
        assert_eq!(
            values
                .iter()
                .filter(|value| event_type(value) == EventType::ToolCall)
                .count(),
            6
        );
        let results = values
            .iter()
            .flat_map(enumerate_results)
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 6);
        assert!(results.iter().any(|result| {
            result
                .content
                .as_deref()
                .is_some_and(|text| text.contains("string to replace was not found"))
        }));
        assert!(results.iter().any(|result| {
            result
                .content
                .as_deref()
                .is_some_and(|text| text.contains("\"type\":\"diff\""))
        }));
        assert!(values.iter().any(|value| {
            event_type(value) == EventType::CommandOutput
                && enumerate_results(value)[0]
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("# pass 2"))
        }));
        assert!(values.iter().all(|value| {
            timestamp(value).is_some_and(|time| time.timestamp_millis() == 1_786_547_760_000)
        }));
    }

    #[test]
    fn nested_terminal_text_and_failure_are_retained() {
        let value = json!({
            "timestamp": 1,
            "method": "session/update",
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call",
                    "status": "failed",
                    "content": [{"type": "content", "content": {"type": "text", "text": "failed safely"}}]
                },
                "_meta": {"eventId": "event", "agentTimestampMs": 1_700_000_000_000_i64}
            }
        });
        assert_eq!(event_type(&value), EventType::ToolOutput);
        let results = enumerate_results(&value);
        assert_eq!(results[0].content.as_deref(), Some("failed safely"));
    }

    #[test]
    fn raw_output_is_not_used_as_ordinary_visible_content() {
        let value = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call",
                    "status": "completed",
                    "rawOutput": {"absolute_path": "/private/source.rs"}
                }
            }
        });
        assert!(enumerate_results(&value)[0].content.is_none());
    }

    #[test]
    fn execute_completion_is_command_output() {
        let value = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call",
                    "status": "completed",
                    "content": [{"type": "content", "content": {"type": "text", "text": "ok"}}],
                    "_meta": {"x.ai/tool": {"kind": "execute"}}
                }
            }
        });
        assert_eq!(event_type(&value), EventType::CommandOutput);
    }

    #[test]
    fn native_bash_results_preserve_literal_content_without_inferred_outcome() {
        let value = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call",
                    "status": "failed",
                    "content": "timed out",
                    "rawOutput": {"type": "Bash", "timed_out": true, "exit_code": 0}
                }
            }
        });
        let results = enumerate_results(&value);
        assert_eq!(results[0].content.as_deref(), Some("timed out"));

        let nonzero = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "call",
                    "status": "completed",
                    "content": "command failed",
                    "rawOutput": {"type": "Bash", "timed_out": false, "exit_code": 17}
                }
            }
        });
        let results = enumerate_results(&nonzero);
        assert_eq!(results[0].content.as_deref(), Some("command failed"));
    }

    #[test]
    fn typed_diff_and_raw_output_only_results_are_retained() {
        let diff = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "diff-call",
                    "status": "completed",
                    "content": [{"type": "diff", "path": "/repo/lib.rs", "oldText": "old", "newText": "new"}],
                    "rawOutput": {"type": "SearchReplace"}
                }
            }
        });
        let diff_result = enumerate_results(&diff);
        assert!(diff_result[0]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("\"type\":\"diff\"")));

        let list = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "list-call",
                    "status": "completed",
                    "rawOutput": {"type": "ListDir", "Content": {"content": "src/\\nCargo.toml", "absolute_root_path": "/repo"}}
                }
            }
        });
        let list_result = enumerate_results(&list);
        assert_eq!(list_result[0].content.as_deref(), Some("src/\\nCargo.toml"));
    }

    #[test]
    fn timestamp_prefers_agent_milliseconds_and_falls_back_to_envelope_seconds() {
        let preferred = json!({
            "timestamp": 1,
            "params": {"_meta": {"agentTimestampMs": 1_700_000_000_123_i64}}
        });
        assert_eq!(
            timestamp(&preferred).unwrap().timestamp_millis(),
            1_700_000_000_123
        );

        let fallback = json!({"timestamp": 1_700_000_001_i64});
        assert_eq!(
            timestamp(&fallback).unwrap().timestamp_millis(),
            1_700_000_001_000
        );
    }

    #[test]
    fn unknown_future_update_does_not_retain_untrusted_body() {
        let value = json!({
            "params": {
                "sessionId": "session",
                "update": {
                    "sessionUpdate": "future_update_v2",
                    "content": {"type": "text", "text": "future-sensitive-marker"},
                    "rawOutput": {"type": "FutureResult", "secret": "future-secret"}
                }
            }
        });
        assert_eq!(event_type(&value), EventType::Notice);
        assert!(event_text(&value).is_empty());
        assert!(enumerate_results(&value).is_empty());
    }

    #[test]
    fn unknown_terminal_content_union_variants_stay_contentless() {
        for content in [
            json!([{"type": "image", "data": "future-sensitive-image-marker"}]),
            json!([{"type": "resource", "uri": "future-sensitive-resource-marker"}]),
            json!([{"type": "future_v2", "secret": "future-sensitive-v2-marker"}]),
        ] {
            let value = json!({
                "params": {
                    "sessionId": "session",
                    "update": {
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": "call",
                        "status": "completed",
                        "content": content
                    }
                }
            });
            let results = enumerate_results(&value);
            assert_eq!(results.len(), 1);
            assert!(results[0].content.is_none());
            let evidence = contentless_result_evidence(&value);
            assert_eq!(evidence["sessionUpdate"], "tool_call_update");
            assert_eq!(evidence["toolCallId"], "call");
            assert_eq!(evidence["status"], "completed");
            assert!(!serde_json::to_string(&evidence)
                .unwrap()
                .contains("future-sensitive"));
        }
    }

    #[test]
    fn unpinned_terminal_statuses_are_not_projected() {
        for status in ["cancelled", "timed_out", "future_terminal"] {
            let value = json!({
                "params": {
                    "sessionId": "session",
                    "update": {
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": "call",
                        "status": status,
                        "content": "must not be retained"
                    }
                }
            });
            assert!(enumerate_results(&value).is_empty());
            assert_eq!(event_type(&value), EventType::Notice);
        }
    }
}
