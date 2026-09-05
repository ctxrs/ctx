use std::fmt;

use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture, CaptureProvider,
    CoreActivity, TypedKey, CORE_ACTIVITY_REVISION,
};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{value::RawValue, Map, Number, Value};

use crate::{NativeJsonlRuntime, COPILOT_CLI_SOURCE_FORMAT};

pub(super) const COPILOT_DIRECT_NATIVE_JSONL_PARSER_REVISION: &str =
    "copilot-cli-direct-native-jsonl-v9-record-admission-order";

const COPILOT_EVENT_TYPE_MAX_BYTES: usize = 64;
const COPILOT_CALL_ID_MAX_BYTES: usize = 64 * 1024;
const COPILOT_ACTIVITY_COMPONENT_MAX_BYTES: usize = 64 * 1024;
const PARSER_REVISION: &str = "direct-native-jsonl-parser-v7-record-admission-order";

pub const fn copilot_source_backed_adapter<R: NativeJsonlRuntime>(
) -> super::DirectJsonlFamilyAdapter<R> {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::CopilotCli,
        COPILOT_CLI_SOURCE_FORMAT,
        "copilot-cli-direct-native-jsonl-v1",
        PARSER_REVISION,
    )
}

pub(super) fn copilot_event_identity(value: &Value) -> Option<&str> {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
}

pub(super) fn copilot_start_requires_generic_body(bytes: &[u8]) -> bool {
    let Some((kind, data)) = exact_copilot_activity_data(bytes) else {
        return true;
    };
    kind != CopilotEventKind::Start
        || data.mcp_server_name.value.is_some()
        || data.mcp_server_name.duplicate
        || data.mcp_tool_name.value.is_some()
        || data.mcp_tool_name.duplicate
}

/// Projects only source-authoritative provider activity. Invocation and result
/// events remain separate and share the exact native call ID; no status,
/// success, failure, effect, or repository semantics are inferred.
pub(super) fn copilot_activity(bytes: &[u8]) -> Option<CoreActivity> {
    let (event_kind, data) = exact_copilot_activity_data(bytes)?;
    let provider_call_id =
        match exact_bounded_string(data.tool_call_id.value, COPILOT_CALL_ID_MAX_BYTES) {
            ExactBoundedString::Exact(provider_call_id) if !data.tool_call_id.duplicate => {
                TypedKey::utf8(provider_call_id).ok()?
            }
            ExactBoundedString::Exact(_)
            | ExactBoundedString::Invalid
            | ExactBoundedString::Exceeded => return None,
        };

    match event_kind {
        CopilotEventKind::Start => {
            let server =
                exact_unique_string(&data.mcp_server_name, COPILOT_ACTIVITY_COMPONENT_MAX_BYTES)?;
            let tool =
                exact_unique_string(&data.mcp_tool_name, COPILOT_ACTIVITY_COMPONENT_MAX_BYTES)?;
            Some(CoreActivity {
                revision: CORE_ACTIVITY_REVISION,
                provider_call_id: Some(provider_call_id),
                invocation: Some(ActivityInvocation {
                    protocol: Some("mcp".to_owned()),
                    server: Some(server),
                    tool,
                    arguments: capture_json(&data.arguments),
                    started_at_unix_ms: None,
                }),
                result: None,
                facts: Vec::new(),
            })
        }
        CopilotEventKind::Completion => Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: Some(provider_call_id),
            invocation: None,
            result: Some(ActivityResult {
                status: None,
                completed_at_unix_ms: None,
                duration_ns: None,
                text: capture_result_text(&data),
                structured_content: capture_result_content(&data),
            }),
            facts: Vec::new(),
        }),
    }
}

fn exact_unique_string(field: &CopilotRawField<'_>, maximum_bytes: usize) -> Option<String> {
    match exact_bounded_string(field.value, maximum_bytes) {
        ExactBoundedString::Exact(value) if !field.duplicate => Some(value),
        ExactBoundedString::Exact(_)
        | ExactBoundedString::Invalid
        | ExactBoundedString::Exceeded => None,
    }
}

fn capture_json(field: &CopilotRawField<'_>) -> ActivityJsonCapture {
    if field.duplicate {
        return ActivityJsonCapture::Unavailable;
    }
    let Some(raw) = field.value else {
        return ActivityJsonCapture::Absent;
    };
    parse_complete_json(raw).map_or(ActivityJsonCapture::Unavailable, |value| {
        ActivityJsonCapture::Present { value }
    })
}

fn capture_result_content(data: &CopilotRawData<'_>) -> ActivityJsonCapture {
    if data.success.duplicate
        || data.result.duplicate
        || data.error.duplicate
        || data.content.duplicate
    {
        return ActivityJsonCapture::Unavailable;
    }
    let mut content = Map::new();
    for (name, field) in [
        ("success", &data.success),
        ("result", &data.result),
        ("error", &data.error),
        ("content", &data.content),
    ] {
        let Some(raw) = field.value else {
            continue;
        };
        let Some(value) = parse_complete_json(raw) else {
            return ActivityJsonCapture::Unavailable;
        };
        content.insert(name.to_owned(), value);
    }
    if content.is_empty() {
        ActivityJsonCapture::Absent
    } else {
        ActivityJsonCapture::Present {
            value: Value::Object(content),
        }
    }
}

fn capture_result_text(data: &CopilotRawData<'_>) -> ActivityTextCapture {
    if data.result.duplicate || data.error.duplicate || data.content.duplicate {
        return ActivityTextCapture::Unavailable;
    }
    if let Some(raw) = data.content.value {
        return serde_json::from_str::<String>(raw.get())
            .ok()
            .map(|value| ActivityTextCapture::Present { value })
            .unwrap_or(ActivityTextCapture::Unavailable);
    }
    for (field, text_key) in [(&data.result, "content"), (&data.error, "message")] {
        let Some(raw) = field.value else {
            continue;
        };
        let Some(value) = parse_complete_json(raw) else {
            return ActivityTextCapture::Unavailable;
        };
        return match value.get(text_key) {
            Some(Value::String(value)) => ActivityTextCapture::Present {
                value: value.clone(),
            },
            None | Some(Value::Null) => ActivityTextCapture::Absent,
            Some(_) => ActivityTextCapture::Unavailable,
        };
    }
    ActivityTextCapture::Absent
}

fn exact_copilot_activity_data(bytes: &[u8]) -> Option<(CopilotEventKind, CopilotRawData<'_>)> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let envelope = CopilotRawEnvelope::deserialize(&mut deserializer).ok()?;
    deserializer.end().ok()?;
    if envelope.event_type.duplicate || envelope.data.duplicate || envelope.explicitly_redacted {
        return None;
    }
    let event_type =
        match exact_bounded_string(envelope.event_type.value, COPILOT_EVENT_TYPE_MAX_BYTES) {
            ExactBoundedString::Exact(event_type) => event_type,
            ExactBoundedString::Invalid | ExactBoundedString::Exceeded => return None,
        };
    let event_kind = match event_type.as_str() {
        "tool.execution_start" => CopilotEventKind::Start,
        "tool.execution_complete" => CopilotEventKind::Completion,
        _ => return None,
    };
    let raw_data = envelope.data.value?;
    let mut deserializer = serde_json::Deserializer::from_str(raw_data.get());
    let data = CopilotRawData::deserialize(&mut deserializer).ok()?;
    deserializer.end().ok()?;
    (data.object && !data.explicitly_redacted).then_some((event_kind, data))
}

fn parse_complete_json(raw: &RawValue) -> Option<Value> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let CompleteJsonValue(value) = CompleteJsonValue::deserialize(&mut deserializer).ok()?;
    deserializer.end().ok()?;
    Some(value)
}

struct CompleteJsonValue(Value);

impl<'de> Deserialize<'de> for CompleteJsonValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CompleteJsonValueVisitor)
    }
}

struct CompleteJsonValueVisitor;

impl<'de> Visitor<'de> for CompleteJsonValueVisitor {
    type Value = CompleteJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(CompleteJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(CompleteJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(CompleteJsonValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(CompleteJsonValue)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(CompleteJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(CompleteJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(CompleteJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(CompleteJsonValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<CompleteJsonValue>()? {
            values.push(value.0);
        }
        Ok(CompleteJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(<A::Error as serde::de::Error>::custom(
                    "duplicate JSON object key",
                ));
            }
            values.insert(key, map.next_value::<CompleteJsonValue>()?.0);
        }
        Ok(CompleteJsonValue(Value::Object(values)))
    }
}

fn observe_redaction_marker(explicitly_redacted: &mut bool, key: &str, raw: &RawValue) {
    let marked = match key {
        "redacted" | "is_redacted" | "isRedacted" => {
            serde_json::from_str::<bool>(raw.get()).ok() != Some(false)
        }
        "status" | "state" => serde_json::from_str::<String>(raw.get())
            .ok()
            .is_some_and(|state| matches!(state.as_str(), "redacted" | "output-redacted")),
        _ => false,
    };
    *explicitly_redacted |= marked;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopilotEventKind {
    Start,
    Completion,
}

enum ExactBoundedString {
    Exact(String),
    Invalid,
    Exceeded,
}

fn exact_bounded_string(raw: Option<&RawValue>, maximum_bytes: usize) -> ExactBoundedString {
    let Some(raw) = raw else {
        return ExactBoundedString::Invalid;
    };
    let encoded = raw.get().as_bytes();
    if encoded.first() != Some(&b'"') {
        return ExactBoundedString::Invalid;
    }
    if encoded.len() > maximum_bytes.saturating_mul(6).saturating_add(2) {
        return ExactBoundedString::Exceeded;
    }
    let Ok(value) = serde_json::from_str::<String>(raw.get()) else {
        return ExactBoundedString::Invalid;
    };
    if value.len() > maximum_bytes {
        ExactBoundedString::Exceeded
    } else if value.is_empty() {
        ExactBoundedString::Invalid
    } else {
        ExactBoundedString::Exact(value)
    }
}

#[derive(Default)]
struct CopilotRawField<'a> {
    value: Option<&'a RawValue>,
    duplicate: bool,
}

impl<'a> CopilotRawField<'a> {
    fn observe(&mut self, value: &'a RawValue) {
        if self.value.is_some() {
            self.duplicate = true;
        } else {
            self.value = Some(value);
        }
    }
}

#[derive(Default)]
struct CopilotRawEnvelope<'a> {
    event_type: CopilotRawField<'a>,
    data: CopilotRawField<'a>,
    explicitly_redacted: bool,
}

impl<'de> Deserialize<'de> for CopilotRawEnvelope<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CopilotRawEnvelopeVisitor)
    }
}

struct CopilotRawEnvelopeVisitor;

impl<'de> Visitor<'de> for CopilotRawEnvelopeVisitor {
    type Value = CopilotRawEnvelope<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Copilot session event JSON value")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut envelope = CopilotRawEnvelope::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "type" => envelope.event_type.observe(map.next_value::<&RawValue>()?),
                "data" => envelope.data.observe(map.next_value::<&RawValue>()?),
                "redacted" | "is_redacted" | "isRedacted" | "status" | "state" => {
                    let raw = map.next_value::<&RawValue>()?;
                    observe_redaction_marker(&mut envelope.explicitly_redacted, &key, raw);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(envelope)
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }
    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }
    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }
    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }
    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }
    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }
    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }
    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(CopilotRawEnvelope::default())
    }
}

#[derive(Default)]
struct CopilotRawData<'a> {
    object: bool,
    tool_call_id: CopilotRawField<'a>,
    mcp_server_name: CopilotRawField<'a>,
    mcp_tool_name: CopilotRawField<'a>,
    success: CopilotRawField<'a>,
    arguments: CopilotRawField<'a>,
    result: CopilotRawField<'a>,
    error: CopilotRawField<'a>,
    content: CopilotRawField<'a>,
    explicitly_redacted: bool,
}

impl<'de> Deserialize<'de> for CopilotRawData<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CopilotRawDataVisitor)
    }
}

struct CopilotRawDataVisitor;

impl<'de> Visitor<'de> for CopilotRawDataVisitor {
    type Value = CopilotRawData<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Copilot tool event data value")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut data = CopilotRawData {
            object: true,
            ..CopilotRawData::default()
        };
        while let Some(key) = map.next_key::<String>()? {
            let value = match key.as_str() {
                "toolCallId" | "mcpServerName" | "mcpToolName" | "success" | "arguments"
                | "result" | "error" | "content" | "redacted" | "is_redacted" | "isRedacted"
                | "status" | "state" => map.next_value::<&RawValue>()?,
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                    continue;
                }
            };
            match key.as_str() {
                "toolCallId" => data.tool_call_id.observe(value),
                "mcpServerName" => data.mcp_server_name.observe(value),
                "mcpToolName" => data.mcp_tool_name.observe(value),
                "success" => data.success.observe(value),
                "arguments" => data.arguments.observe(value),
                "result" => data.result.observe(value),
                "error" => data.error.observe(value),
                "content" => data.content.observe(value),
                "redacted" | "is_redacted" | "isRedacted" | "status" | "state" => {
                    observe_redaction_marker(&mut data.explicitly_redacted, &key, value);
                }
                _ => unreachable!("Copilot raw data key was filtered above"),
            }
        }
        Ok(data)
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }
    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }
    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }
    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }
    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }
    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }
    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }
    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(CopilotRawData::default())
    }
}
