use serde_json::Value;

use ctx_core::models::SessionEventType;

use super::normalize::normalize_tool_event;
use super::normalize::NormalizedToolEvent;
use super::state::TurnToolUpdate;

#[derive(Debug, Clone)]
pub struct ToolOutputArtifactRef {
    pub artifact_id: String,
    pub name: Option<String>,
    pub mime_type: String,
    pub bytes: i64,
}

#[derive(Debug, Clone)]
pub struct ToolOpsMeta {
    pub tool_call_id: Option<String>,
    pub tool_kind: Option<String>,
    pub title: Option<String>,
    pub status: Option<String>,
    pub input_preview: Option<Value>,
    pub cwd: Option<String>,
}

pub fn sanitize_tool_event_payload(
    event_type: &SessionEventType,
    raw_payload: &Value,
    output_artifact: Option<&ToolOutputArtifactRef>,
) -> Value {
    let normalized = normalize_tool_event(event_type, raw_payload);
    sanitize_normalized_tool_event_payload(&normalized, output_artifact)
}

pub fn sanitize_normalized_tool_event_payload(
    normalized: &NormalizedToolEvent,
    output_artifact: Option<&ToolOutputArtifactRef>,
) -> Value {
    let mut object = serde_json::Map::new();

    if let Some(tool_call_id) = normalized
        .tool_call_id
        .as_deref()
        .filter(|tool_call_id| !tool_call_id.trim().is_empty())
    {
        object.insert(
            "tool_call_id".to_string(),
            Value::String(tool_call_id.to_string()),
        );
    }
    if let Some(tool_kind) = normalized.tool_kind.as_ref() {
        object.insert("kind".to_string(), Value::String(tool_kind.clone()));
    }
    if let Some(tool_label) = normalized.tool_label.as_ref() {
        object.insert("tool_label".to_string(), Value::String(tool_label.clone()));
    }
    if let Some(tool_name) = normalized.provider_tool_name.as_ref() {
        object.insert("tool_name".to_string(), Value::String(tool_name.clone()));
    }
    if let Some(title) = normalized.title.as_ref() {
        object.insert("title".to_string(), Value::String(title.clone()));
    }
    if let Some(subtitle) = normalized.subtitle.as_ref() {
        object.insert("subtitle".to_string(), Value::String(subtitle.clone()));
    }
    object.insert(
        "status".to_string(),
        Value::String(normalized.status.clone()),
    );
    if let Some(input_preview) = normalized.input_meta.preview.as_ref() {
        object.insert("input_preview".to_string(), input_preview.clone());
    }
    if let Some(truncated) = normalized.input_meta.truncated {
        object.insert("input_truncated".to_string(), Value::Bool(truncated));
    }
    if let Some(bytes) = normalized.input_meta.original_bytes {
        object.insert(
            "input_original_bytes".to_string(),
            Value::Number(serde_json::Number::from(bytes)),
        );
    }
    if let Some(output_preview) = normalized.output_preview.as_ref() {
        object.insert(
            "output_preview".to_string(),
            Value::String(output_preview.preview.clone()),
        );
        object.insert(
            "output_truncated".to_string(),
            Value::Bool(output_preview.truncated),
        );
        object.insert(
            "output_original_bytes".to_string(),
            Value::Number(serde_json::Number::from(
                output_preview.original_bytes as i64,
            )),
        );
    }
    if let Some(crp_seq) = normalized.crp_seq.as_ref() {
        object.insert("crp_seq".to_string(), crp_seq.clone());
    }
    if let Some(crp_channel) = normalized.crp_channel.as_ref() {
        object.insert("crp_channel".to_string(), crp_channel.clone());
    }
    if let Some(order_seq) = normalized.raw_order_seq.as_ref() {
        object.insert("order_seq".to_string(), order_seq.clone());
    }
    if let Some(artifact) = output_artifact {
        let mut artifact_json = serde_json::Map::new();
        artifact_json.insert(
            "artifact_id".to_string(),
            Value::String(artifact.artifact_id.clone()),
        );
        if let Some(name) = artifact.name.as_ref() {
            artifact_json.insert("name".to_string(), Value::String(name.clone()));
        }
        artifact_json.insert(
            "mime_type".to_string(),
            Value::String(artifact.mime_type.clone()),
        );
        artifact_json.insert(
            "bytes".to_string(),
            Value::Number(serde_json::Number::from(artifact.bytes)),
        );
        object.insert("output_artifact".to_string(), Value::Object(artifact_json));
    }

    Value::Object(object)
}

pub fn build_tool_ops_meta(event_type: &SessionEventType, raw_payload: &Value) -> ToolOpsMeta {
    let normalized = normalize_tool_event(event_type, raw_payload);
    build_tool_ops_meta_from_normalized(&normalized)
}

pub fn build_tool_ops_meta_from_normalized(normalized: &NormalizedToolEvent) -> ToolOpsMeta {
    ToolOpsMeta {
        tool_call_id: normalized.tool_call_id.clone(),
        tool_kind: normalized.raw_tool_kind.clone(),
        title: normalized.raw_title.clone(),
        status: Some(normalized.status.clone()),
        input_preview: normalized.input_preview.clone(),
        cwd: normalized.cwd.clone(),
    }
}

pub fn build_turn_tool_update_from_payload(
    event_type: &SessionEventType,
    payload_json: &Value,
) -> Option<TurnToolUpdate> {
    let normalized = normalize_tool_event(event_type, payload_json);
    build_turn_tool_update(&normalized, extract_order_seq_value(payload_json))
}

pub fn build_turn_tool_update(
    normalized: &NormalizedToolEvent,
    order_seq: Option<i64>,
) -> Option<TurnToolUpdate> {
    Some(TurnToolUpdate {
        tool_call_id: normalized.tool_call_id.clone()?,
        order_seq,
        tool_kind: normalized.tool_kind.clone(),
        provider_tool_name: normalized.provider_tool_name.clone(),
        title: normalized.title.clone(),
        subtitle: normalized.subtitle.clone(),
        status: Some(normalized.status.clone()),
        input_json: normalized.input_meta.preview.clone(),
        output_text: normalized
            .output_preview
            .as_ref()
            .map(|preview| preview.preview.clone()),
        input_truncated: normalized.input_meta.truncated,
        input_original_bytes: normalized.input_meta.original_bytes,
        output_truncated: normalized
            .output_preview
            .as_ref()
            .map(|preview| preview.truncated),
        output_original_bytes: normalized
            .output_preview
            .as_ref()
            .map(|preview| preview.original_bytes as i64),
    })
}

fn extract_order_seq_value(payload: &Value) -> Option<i64> {
    payload
        .get("order_seq")
        .and_then(Value::as_i64)
        .or_else(|| payload.get("orderSeq").and_then(Value::as_i64))
}
