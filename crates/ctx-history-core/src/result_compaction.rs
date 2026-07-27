use serde_json::{Map, Value};

use crate::ContentRef;

const MAX_RESULT_IDENTIFIERS: usize = 32;
const MAX_CALL_ID_BYTES: usize = 256;
const MAX_TOOL_IDENTITY_BYTES: usize = 256;
const MAX_FORGE_URL_BYTES: usize = 512;

/// Returns the bounded, typed subset of a provider command/tool result that may
/// be persisted in the canonical Store.
///
/// The input may expose result fields directly or beneath a provider `body`
/// object. Arbitrary result text, previews, commands, and unknown fields are
/// deliberately omitted.
#[must_use]
pub fn compact_result_payload(payload: &Value) -> Value {
    let body = payload.get("body");
    let mut compact = Map::new();

    if let Some(tool) = first_result_field(payload, body, &["tool", "name"])
        .and_then(Value::as_str)
        .filter(|value| valid_result_token(value, MAX_TOOL_IDENTITY_BYTES))
    {
        compact.insert("tool".to_owned(), Value::String(tool.to_owned()));
    }
    if let Some(call_id) = first_result_field(payload, body, &["call_id"])
        .and_then(Value::as_str)
        .filter(|value| valid_result_token(value, MAX_CALL_ID_BYTES))
    {
        compact.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
    }
    if let Some(exit_code) = first_result_field(payload, body, &["exit_code"])
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
    {
        compact.insert("exit_code".to_owned(), Value::Number(exit_code.into()));
    }
    if let Some(duration_ms) = first_result_field(payload, body, &["duration_ms"])
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
    {
        compact.insert("duration_ms".to_owned(), Value::Number(duration_ms.into()));
    }
    if let Some(timed_out) =
        first_result_field(payload, body, &["timed_out"]).and_then(Value::as_bool)
    {
        compact.insert("timed_out".to_owned(), Value::Bool(timed_out));
    }
    if let Some(output_bytes) =
        first_result_field(payload, body, &["output_bytes"]).and_then(Value::as_u64)
    {
        compact.insert(
            "output_bytes".to_owned(),
            Value::Number(output_bytes.into()),
        );
    }
    if let Some(outcome @ ("success" | "failure")) =
        first_result_field(payload, body, &["result_outcome"]).and_then(Value::as_str)
    {
        compact.insert(
            "result_outcome".to_owned(),
            Value::String(outcome.to_owned()),
        );
    }
    if let Some(evidence) =
        first_result_field(payload, body, &["result_evidence"]).and_then(compact_result_evidence)
    {
        compact.insert("result_evidence".to_owned(), evidence);
    }
    if let Some(content_ref) = first_result_field(payload, body, &["result_content_ref"])
        .and_then(|value| serde_json::from_value::<ContentRef>(value.clone()).ok())
        .and_then(|value| serde_json::to_value(value).ok())
    {
        compact.insert("result_content_ref".to_owned(), content_ref);
    }

    Value::Object(compact)
}

fn first_result_field<'a>(
    payload: &'a Value,
    body: Option<&'a Value>,
    keys: &[&str],
) -> Option<&'a Value> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .or_else(|| body.and_then(|body| body.get(*key)))
            .filter(|value| !value.is_null())
    })
}

fn compact_result_evidence(value: &Value) -> Option<Value> {
    let identifiers = value.as_array()?;
    if identifiers.len() > MAX_RESULT_IDENTIFIERS {
        return None;
    }
    Some(Value::Array(
        identifiers
            .iter()
            .filter_map(compact_result_identifier)
            .collect(),
    ))
}

fn compact_result_identifier(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    if object.len() != 2 {
        return None;
    }
    let kind = object.get("kind")?.as_str()?;
    let value = object.get("value")?.as_str()?;
    valid_result_identifier(kind, value).then(|| {
        serde_json::json!({
            "kind": kind,
            "value": value,
        })
    })
}

fn valid_result_identifier(kind: &str, value: &str) -> bool {
    match kind {
        "call_id" => valid_result_token(value, MAX_CALL_ID_BYTES),
        "git_commit_summary_id" => (7..=64).contains(&value.len()) && lowercase_hex(value),
        "git_oid" => matches!(value.len(), 40 | 64) && lowercase_hex(value),
        "git_abbrev_oid" => (7..=12).contains(&value.len()) && lowercase_hex(value),
        "forge_url" => valid_forge_url(value),
        _ => false,
    }
}

fn valid_result_token(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
}

fn valid_forge_url(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_FORGE_URL_BYTES
        || !value.is_ascii()
        || value.contains(['?', '#', '\0', '\n', '\r'])
    {
        return false;
    }
    let Some((authority, path)) = value
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    matches!(
        authority.to_ascii_lowercase().as_str(),
        "github.com" | "gitlab.com" | "bitbucket.org" | "codeberg.org"
    ) && !path.is_empty()
}

fn lowercase_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn result_payload_keeps_only_valid_bounded_fields() {
        let payload = json!({
            "tool": "exec_command",
            "call_id": "call-1",
            "exit_code": 1,
            "duration_ms": 42,
            "timed_out": false,
            "output_bytes": 1024,
            "result_outcome": "failure",
            "result_evidence": [
                {"kind": "call_id", "value": "call-1"},
                {"kind": "git_oid", "value": "a".repeat(40)},
                {"kind": "future", "value": "ignored"}
            ],
            "result_content_ref": {
                "sha256": "b".repeat(64),
                "byte_len": 1024
            },
            "text": "raw body",
            "output_preview": "raw preview",
            "unknown": "not compact metadata"
        });

        assert_eq!(
            compact_result_payload(&payload),
            json!({
                "tool": "exec_command",
                "call_id": "call-1",
                "exit_code": 1,
                "duration_ms": 42,
                "timed_out": false,
                "output_bytes": 1024,
                "result_outcome": "failure",
                "result_evidence": [
                    {"kind": "call_id", "value": "call-1"},
                    {"kind": "git_oid", "value": "a".repeat(40)}
                ],
                "result_content_ref": {
                    "sha256": "b".repeat(64),
                    "byte_len": 1024
                }
            })
        );
    }

    #[test]
    fn malformed_and_oversized_compact_fields_are_dropped() {
        let payload = json!({
            "tool": "x".repeat(MAX_TOOL_IDENTITY_BYTES + 1),
            "call_id": ["not", "a", "string"],
            "exit_code": i64::from(i32::MAX) + 1,
            "duration_ms": -1,
            "timed_out": "false",
            "output_bytes": -1,
            "result_outcome": "maybe",
            "result_evidence": (0..=MAX_RESULT_IDENTIFIERS)
                .map(|index| json!({"kind": "call_id", "value": format!("call-{index}")}))
                .collect::<Vec<_>>(),
            "result_content_ref": {
                "sha256": "A".repeat(64),
                "byte_len": 1,
                "extra": true
            },
            "output_preview": "must disappear"
        });

        assert_eq!(compact_result_payload(&payload), json!({}));
    }
}
