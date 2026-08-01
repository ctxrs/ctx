use ctx_history_core::EventType;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderPolicyText {
    pub(crate) text: String,
    pub(crate) retention: ProviderTextRetention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderTextRetention;

impl ProviderTextRetention {
    pub(crate) fn as_json(self) -> Value {
        json!({
            "mode": "complete",
            "limit_chars": Value::Null,
            "truncated": false,
            "omission_policy": "none",
            "omission_applied": false,
        })
    }
}

/// Retain admitted Core text exactly.
///
/// Display previews are produced by provider-local presentation helpers. They
/// must never be substituted for the canonical normalized text selected here.
pub(crate) fn provider_policy_event_text(
    _event_type: EventType,
    text: &str,
    _body: &Value,
) -> ProviderPolicyText {
    ProviderPolicyText {
        text: text.to_owned(),
        retention: ProviderTextRetention,
    }
}

/// Retain admitted structured Core content exactly.
///
/// Provider-private framing, binary values, and explicit redactions must be
/// handled by the provider with truthful omission metadata. Generic field-name
/// filtering is not a Core content policy: tool inputs, outputs, patches, and
/// diffs are complete content whenever the admitted value is textual/structured.
pub(crate) fn provider_policy_body(_event_type: EventType, body: &Value) -> Value {
    body.clone()
}
