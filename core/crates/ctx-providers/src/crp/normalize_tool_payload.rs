use serde_json::{json, Value};

use super::protocol::CrpToolStatus;

pub(super) fn build_tool_started_payload(
    tool_call_id: String,
    tool_name: String,
    tool_label: Option<String>,
    input: Option<Value>,
    input_preview: Option<Value>,
    seq: u64,
) -> Value {
    let mut payload = serde_json::Map::new();
    let tool_name_for_call = tool_name.clone();
    payload.insert("tool_call_id".to_string(), json!(tool_call_id.clone()));
    payload.insert("kind".to_string(), json!(tool_name.clone()));
    if let Some(label) = tool_label.as_ref() {
        payload.insert("tool_label".to_string(), json!(label));
    }
    payload.insert("status".to_string(), json!("running"));
    let raw_input = input.clone().or_else(|| input_preview.clone());
    if let Some(input) = raw_input.clone() {
        payload.insert("rawInput".to_string(), input);
    }
    if let Some(input_preview) = input_preview.clone() {
        payload.insert("input_preview".to_string(), input_preview);
    }
    let mut tool_call_obj = serde_json::Map::new();
    tool_call_obj.insert("id".to_string(), json!(tool_call_id));
    tool_call_obj.insert("name".to_string(), json!(tool_name_for_call.clone()));
    tool_call_obj.insert("kind".to_string(), json!(tool_name_for_call));
    tool_call_obj.insert(
        "rawInput".to_string(),
        raw_input.clone().unwrap_or(Value::Null),
    );
    tool_call_obj.insert("status".to_string(), json!("running"));
    payload.insert("toolCall".to_string(), Value::Object(tool_call_obj));
    payload.insert("crp_seq".to_string(), json!(seq));
    Value::Object(payload)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_tool_completed_payload(
    tool_call_id: String,
    tool_name: String,
    tool_label: Option<String>,
    status: CrpToolStatus,
    output: Option<Value>,
    error: Option<String>,
    input: Option<Value>,
    input_preview: Option<Value>,
    seq: u64,
) -> Value {
    let mut payload = serde_json::Map::new();
    let tool_name_for_call = tool_name.clone();
    payload.insert("tool_call_id".to_string(), json!(tool_call_id.clone()));
    payload.insert("kind".to_string(), json!(tool_name.clone()));
    if let Some(label) = tool_label.as_ref() {
        payload.insert("tool_label".to_string(), json!(label));
    }
    payload.insert(
        "status".to_string(),
        json!(match status {
            CrpToolStatus::Success => "completed",
            CrpToolStatus::Error => "failed",
        }),
    );
    let raw_input = input.clone().or_else(|| input_preview.clone());
    if let Some(input) = raw_input.clone() {
        payload.insert("rawInput".to_string(), input);
    }
    if let Some(input_preview) = input_preview.clone() {
        payload.insert("input_preview".to_string(), input_preview);
    }
    if let Some(output) = output.clone() {
        if let Some(text) = extract_output_text(&output) {
            payload.insert("output_text".to_string(), json!(text));
        }
        payload.insert("rawOutput".to_string(), output);
    }
    if let Some(err) = error {
        payload.insert("error".to_string(), json!(err));
    }
    if let Some(label) = tool_label.clone() {
        payload.insert("tool_label".to_string(), json!(label));
    }
    let mut tool_call_obj = serde_json::Map::new();
    tool_call_obj.insert("id".to_string(), json!(tool_call_id));
    tool_call_obj.insert("name".to_string(), json!(tool_name_for_call.clone()));
    tool_call_obj.insert("kind".to_string(), json!(tool_name_for_call));
    tool_call_obj.insert(
        "rawInput".to_string(),
        raw_input.clone().unwrap_or(Value::Null),
    );
    tool_call_obj.insert(
        "rawOutput".to_string(),
        payload.get("rawOutput").cloned().unwrap_or(Value::Null),
    );
    tool_call_obj.insert(
        "status".to_string(),
        payload.get("status").cloned().unwrap_or(Value::Null),
    );
    payload.insert("toolCall".to_string(), Value::Object(tool_call_obj));
    payload.insert("crp_seq".to_string(), json!(seq));
    Value::Object(payload)
}

fn extract_output_text(output: &Value) -> Option<String> {
    if let Some(text) = output.get("formatted_output").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = output.get("aggregated_output").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = output.get("stdout").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    if let Some(text) = output.get("result").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    output.as_str().map(|text| text.to_string())
}
