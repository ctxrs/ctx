use serde_json::json;

use super::projection::{
    crush_normalized_result_content, project_message, CrushMessageRow, CrushRecordProjection,
    CrushSessionRow,
};
use crate::{OutputOutcome, Result};

#[test]
fn result_content_uses_only_ordered_schema_owned_fields() {
    let parts = json!([
        {"type": "text", "data": {"output": "not a result"}},
        {"type": "tool_result", "data": {
            "content": "tool body",
            "output": "lower priority"
        }},
        {"type": "shell_command", "data": {
            "stdout": "shell body",
            "stderr": "lower priority"
        }},
        {"type": "unknown", "data": {"output": "not discovered"}}
    ]);
    assert_eq!(
        crush_normalized_result_content(&parts),
        Some("tool body\nshell body".to_owned())
    );
}

#[test]
fn projection_keeps_complete_success_failure_and_unknown_results() -> Result<()> {
    for (status, expected) in [
        (Some("success"), OutputOutcome::Success),
        (Some("failed"), OutputOutcome::Failure),
        (None, OutputOutcome::Unknown),
    ] {
        let body = format!("{expected:?} native result body");
        let mut data = json!({
            "content": body,
            "tool_call_id": "call-1",
            "name": "shell",
        });
        if let Some(status) = status {
            data["status"] = json!(status);
        }
        let row = crush_message(json!([{"type": "tool_result", "data": data}]));
        let projection = match project_message(&row, Some(&crush_session()))? {
            CrushRecordProjection::Message(projection) => projection,
            CrushRecordProjection::Rejection => panic!("valid tool result was rejected"),
        };
        let output = projection.output.expect("tool output projection");
        assert_eq!(projection.complete_text.as_deref(), Some(body.as_str()));
        assert_eq!(output.outcome.outcome, expected);
        assert_eq!(output.call_id.as_deref(), Some("call-1"));
        assert_eq!(output.tool_name.as_deref(), Some("shell"));
        assert!(output.linkage_exact);
    }

    let status_only = crush_message(json!([{
        "type": "tool_result",
        "data": {"status": "success", "tool_call_id": "call-1"}
    }]));
    assert!(matches!(
        project_message(&status_only, Some(&crush_session()))?,
        CrushRecordProjection::Rejection
    ));
    Ok(())
}

fn crush_message(parts: serde_json::Value) -> CrushMessageRow {
    CrushMessageRow {
        rowid: 1,
        id: "message-1".to_owned(),
        session_id: "session-1".to_owned(),
        role: "tool".to_owned(),
        parts: parts.to_string(),
        created_at: Some(1),
        updated_at: Some(1),
        provider: None,
        model: None,
        is_summary_message: false,
    }
}

fn crush_session() -> CrushSessionRow {
    CrushSessionRow {
        id: "session-1".to_owned(),
        parent_session_id: None,
        title: None,
        created_at: Some(1),
        updated_at: Some(1),
        prompt_tokens: None,
        completion_tokens: None,
        cost: None,
        summary_message_id: None,
    }
}
