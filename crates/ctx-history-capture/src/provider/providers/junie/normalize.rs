use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, Confidence, EventRole, EventType, FileChangeKind};
use serde_json::{json, Value};

use crate::provider::normalization::{
    native_event, native_provider_capture, NativeEventDraft, NativeSessionDraft,
};
use crate::{
    complete_content::{
        jsonl::{attach_junie_record_set_locator, JunieRecordSetBinding, JunieRecordSetTarget},
        VerifiedContentRole,
    },
    ProviderAdapterContext, ProviderFileTouchedEnvelope, ProviderNormalizationResult,
    JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
};

use super::assistant::JunieStepAgg;

pub(super) fn junie_step_normalization(
    base_draft: &NativeSessionDraft,
    context: &ProviderAdapterContext,
    line_number: usize,
    event_index: u64,
    occurred_at: DateTime<Utc>,
    step: &JunieStepAgg,
) -> ProviderNormalizationResult {
    let (tool_name, text, body) = if let Some(command) = &step.command {
        (
            "Bash",
            format!("Bash: {command}"),
            json!({
                "tool_name": "Bash",
                "command": command,
                "label": step.label,
                "status": step.status,
            }),
        )
    } else if let Some(files) = &step.files {
        (
            "view",
            step.label
                .clone()
                .unwrap_or_else(|| "View files".to_owned()),
            json!({
                "tool_name": "view",
                "label": step.label,
                "files": files,
                "status": step.status,
            }),
        )
    } else {
        (
            "tool",
            step.label
                .clone()
                .unwrap_or_else(|| "Junie tool step".to_owned()),
            json!({
                "tool_name": "tool",
                "label": step.label,
                "status": step.status,
            }),
        )
    };
    let event = native_event(NativeEventDraft {
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        provider_session_id: base_draft.provider_session_id.clone(),
        provider_event_index: event_index,
        provider_event_hash: Some(format!("step:{}:tool", step.order)),
        cursor: format!(
            "{}:line:{line_number}:event:{event_index}",
            base_draft.raw_source_path
        ),
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at,
        text,
        body,
        metadata: json!({
            "source": "junie_step",
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "tool_name": tool_name,
        }),
    });
    ProviderNormalizationResult {
        captures: vec![(
            line_number,
            native_provider_capture(base_draft.clone(), context, Some(event)),
        )],
        ..ProviderNormalizationResult::default()
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn junie_step_output_normalization(
    base_draft: &NativeSessionDraft,
    context: &ProviderAdapterContext,
    line_number: usize,
    event_index: u64,
    occurred_at: DateTime<Utc>,
    step: &JunieStepAgg,
    details: &str,
    source_binding: &JunieRecordSetBinding,
) -> crate::Result<ProviderNormalizationResult> {
    let tool_name = if step.command.is_some() {
        "Bash"
    } else if step.files.is_some() {
        "view"
    } else {
        "tool"
    };
    let mut event = native_event(NativeEventDraft {
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        provider_session_id: base_draft.provider_session_id.clone(),
        provider_event_index: event_index,
        provider_event_hash: Some(format!("step:{}:output", step.order)),
        cursor: format!(
            "{}:line:{line_number}:event:{event_index}",
            base_draft.raw_source_path
        ),
        event_type: if step.command.is_some() {
            EventType::CommandOutput
        } else {
            EventType::ToolOutput
        },
        role: Some(EventRole::Tool),
        occurred_at,
        text: details.to_owned(),
        body: json!({
            "tool_name": tool_name,
            "details": details,
            "status": step.status,
        }),
        metadata: json!({
            "source": "junie_step_details",
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "tool_name": tool_name,
        }),
    });
    if let Ok(step_order) = u32::try_from(step.order) {
        attach_junie_record_set_locator(
            &mut event,
            VerifiedContentRole::ResultBody,
            details,
            source_binding,
            JunieRecordSetTarget::StepOutput(step_order),
        )?;
    }
    Ok(ProviderNormalizationResult {
        captures: vec![(
            line_number,
            native_provider_capture(base_draft.clone(), context, Some(event)),
        )],
        ..ProviderNormalizationResult::default()
    })
}

pub(super) fn junie_file_change_has_path(change: &Value) -> bool {
    change
        .get("afterRelativePath")
        .and_then(Value::as_str)
        .or_else(|| change.get("beforeRelativePath").and_then(Value::as_str))
        .is_some_and(|path| !path.trim().is_empty())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn junie_file_change_normalization(
    base_draft: &NativeSessionDraft,
    context: &ProviderAdapterContext,
    line_number: usize,
    event_index: u64,
    occurred_at: DateTime<Utc>,
    step_order: usize,
    change_index: usize,
    change: &Value,
    status: Option<&str>,
) -> ProviderNormalizationResult {
    let before_path = change.get("beforeRelativePath").and_then(Value::as_str);
    let after_path = change.get("afterRelativePath").and_then(Value::as_str);
    let path = after_path
        .or(before_path)
        .filter(|path| !path.trim().is_empty())
        .unwrap_or_default();
    let change_kind = match (before_path, after_path) {
        (None, Some(_)) => FileChangeKind::Created,
        (Some(_), None) => FileChangeKind::Deleted,
        (Some(before), Some(after)) if before != after => FileChangeKind::Renamed,
        _ => FileChangeKind::Modified,
    };
    let event = native_event(NativeEventDraft {
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        provider_session_id: base_draft.provider_session_id.clone(),
        provider_event_index: event_index,
        provider_event_hash: Some(format!("step:{step_order}:change:{change_index}")),
        cursor: format!(
            "{}:line:{line_number}:event:{event_index}",
            base_draft.raw_source_path
        ),
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at,
        text: format!("Edit: {path}"),
        body: json!({
            "tool_name": "Edit",
            "file_path": path,
            "old_string": junie_file_content_text(change.get("beforeContent")),
            "new_string": junie_file_content_text(change.get("afterContent")),
            "before_relative_path": before_path,
            "after_relative_path": after_path,
            "change_kind": change_kind.as_str(),
            "status": status,
        }),
        metadata: json!({
            "source": "junie_file_change",
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "tool_name": "Edit",
            "change_kind": change_kind.as_str(),
        }),
    });
    ProviderNormalizationResult {
        captures: vec![(
            line_number,
            native_provider_capture(base_draft.clone(), context, Some(event)),
        )],
        files_touched: vec![(
            line_number,
            ProviderFileTouchedEnvelope {
                provider: CaptureProvider::Junie,
                provider_session_id: base_draft.provider_session_id.clone(),
                provider_touch_index: event_index
                    .saturating_mul(1_000)
                    .saturating_add(change_index as u64),
                provider_event_index: Some(event_index),
                raw_source_path: Some(base_draft.raw_source_path.clone()),
                source_root: context
                    .source_root_display()
                    .or_else(|| Some(base_draft.raw_source_path.clone())),
                path: path.to_owned(),
                change_kind: Some(change_kind),
                old_path: before_path
                    .filter(|before| after_path.is_some_and(|after| after != *before))
                    .map(str::to_owned),
                line_count_delta: None,
                confidence: Confidence::Explicit,
                occurred_at,
                source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
                metadata: json!({
                    "source": "junie_file_change",
                    "step_order": step_order,
                    "change_index": change_index,
                }),
            },
        )],
        ..ProviderNormalizationResult::default()
    }
}

fn junie_file_content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(str::to_owned)
}
