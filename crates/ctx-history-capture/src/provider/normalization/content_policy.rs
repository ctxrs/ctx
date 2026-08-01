use ctx_history_core::EventType;
use serde_json::{json, Value};

use super::value::provider_local_preview;
use crate::{PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeTextOmissionPolicy {
    None,
}

impl NativeTextOmissionPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NativeTextRetentionPolicy {
    limit_chars: Option<usize>,
    omission_policy: NativeTextOmissionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderPolicyText {
    pub(crate) text: String,
    pub(crate) retention: ProviderTextRetention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderTextRetention {
    limit_chars: Option<usize>,
    truncated: bool,
    omission_policy: NativeTextOmissionPolicy,
    omission_applied: bool,
}

impl ProviderTextRetention {
    pub(crate) fn as_json(self) -> Value {
        let mode = if self.limit_chars.is_some() {
            "bounded"
        } else {
            "none"
        };
        json!({
            "mode": mode,
            "limit_chars": self.limit_chars,
            "truncated": self.truncated,
            "omission_policy": self.omission_policy.as_str(),
            "omission_applied": self.omission_applied,
        })
    }
}

fn native_event_text_retention_policy(
    event_type: EventType,
    _body: &Value,
) -> NativeTextRetentionPolicy {
    match event_type {
        EventType::Message | EventType::Summary => NativeTextRetentionPolicy {
            limit_chars: Some(PROVIDER_MAX_TEXT_CHARS),
            omission_policy: NativeTextOmissionPolicy::None,
        },
        EventType::ToolCall | EventType::CommandStarted | EventType::CommandFinished => {
            NativeTextRetentionPolicy {
                limit_chars: Some(PROVIDER_MAX_PREVIEW_CHARS),
                omission_policy: NativeTextOmissionPolicy::None,
            }
        }
        EventType::ToolOutput | EventType::CommandOutput => NativeTextRetentionPolicy {
            limit_chars: None,
            omission_policy: NativeTextOmissionPolicy::None,
        },
        EventType::FileTouched | EventType::VcsChange | EventType::Artifact | EventType::Notice => {
            NativeTextRetentionPolicy {
                limit_chars: None,
                omission_policy: NativeTextOmissionPolicy::None,
            }
        }
    }
}

pub(crate) fn provider_policy_event_text(
    event_type: EventType,
    text: &str,
    body: &Value,
) -> ProviderPolicyText {
    let policy = native_event_text_retention_policy(event_type, body);
    let (text, truncated) = policy
        .limit_chars
        .map(|limit_chars| provider_local_preview(text, limit_chars))
        .unwrap_or_default();
    ProviderPolicyText {
        text,
        retention: ProviderTextRetention {
            limit_chars: policy.limit_chars,
            truncated,
            omission_policy: policy.omission_policy,
            omission_applied: false,
        },
    }
}

pub(crate) fn provider_policy_body(event_type: EventType, body: &Value) -> Value {
    provider_filter_body_by_retention_policy(event_type, body, None)
}

fn provider_filter_body_by_retention_policy(
    event_type: EventType,
    value: &Value,
    key: Option<&str>,
) -> Value {
    if key.is_some_and(|key| provider_should_omit_body_field(event_type, key, value)) {
        return provider_omitted_body_field(value);
    }
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| provider_filter_body_by_retention_policy(event_type, item, key))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        provider_filter_body_by_retention_policy(event_type, value, Some(key)),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn provider_should_omit_body_field(event_type: EventType, key: &str, value: &Value) -> bool {
    let key = provider_normalized_key(key);
    if matches!(
        event_type,
        EventType::Notice | EventType::FileTouched | EventType::VcsChange | EventType::Artifact
    ) && matches!(
        key.as_str(),
        "text" | "content" | "message" | "prompt" | "summary" | "details"
    ) {
        return true;
    }
    if matches!(event_type, EventType::ToolOutput | EventType::CommandOutput)
        && (matches!(
            key.as_str(),
            "details" | "text" | "content" | "outputpreview"
        ) || (key == "message" && !value.is_object()))
    {
        return true;
    }
    matches!(
        key.as_str(),
        "output"
            | "stdout"
            | "stderr"
            | "tooloutput"
            | "toolresult"
            | "toolresults"
            | "tooluseresult"
            | "toolcallstates"
            | "commandoutput"
            | "executionoutput"
            | "result"
            | "results"
            | "diff"
            | "patch"
            | "oldstring"
            | "newstring"
            | "oldcontent"
            | "newcontent"
            | "beforecontent"
            | "aftercontent"
            | "beforetext"
            | "aftertext"
    ) || (matches!(key.as_str(), "input" | "arguments" | "args" | "params")
        && provider_value_contains_patch_or_diff(value))
}

fn provider_omitted_body_field(value: &Value) -> Value {
    json!({
        "field_retention": {
            "mode": "omitted",
            "original_bytes": provider_value_approx_bytes(value),
            "contained_patch_or_diff": provider_value_contains_patch_or_diff(value),
        },
    })
}

fn provider_normalized_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn provider_value_approx_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        _ => serde_json::to_string(value)
            .map(|text| text.len())
            .unwrap_or_default(),
    }
}

pub(crate) fn provider_value_contains_patch_or_diff(value: &Value) -> bool {
    match value {
        Value::String(text) => provider_text_contains_patch_or_diff(text),
        Value::Array(items) => items.iter().any(provider_value_contains_patch_or_diff),
        Value::Object(object) => object.values().any(provider_value_contains_patch_or_diff),
        _ => false,
    }
}

fn provider_text_contains_patch_or_diff(text: &str) -> bool {
    text.contains("*** Begin Patch")
        || text.contains("diff --git ")
        || text.starts_with("@@")
        || text.starts_with("+++ ")
        || text.starts_with("--- ")
        || text.contains("\n@@")
        || text.contains("\n+++ ")
        || text.contains("\n--- ")
}
