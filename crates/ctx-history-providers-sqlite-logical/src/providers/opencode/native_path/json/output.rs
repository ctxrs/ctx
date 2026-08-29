use super::*;

/// Retains every complete provider output exactly as persisted.
///
/// Status, exit codes, and error-shaped fields remain uninterpreted source
/// data. The Core activity projection decides only whether exact call linkage
/// is available; it never classifies the result.
pub(super) fn project_output(body: &Value, effective_type: &str) -> OpenCodeJsonProjection {
    OpenCodeJsonProjection::Output(OpenCodeOutputJson {
        diagnostic: Some(OpenCodeRetainedJson {
            effective_type: effective_type.to_owned(),
            role: body
                .get("role")
                .and_then(Value::as_str)
                .filter(|role| !role.is_empty())
                .unwrap_or("tool")
                .to_owned(),
            body: body.clone(),
        }),
    })
}

pub(super) fn effective_type(
    column_type: &str,
    body_role: Option<&str>,
    body_type: Option<&str>,
    parent_role: Option<&str>,
) -> String {
    let column = column_type.trim().to_ascii_lowercase();
    if !column.is_empty() && column != "message" && column != "part" {
        return column;
    }
    first_nonempty(&[body_role, body_type, parent_role])
        .unwrap_or(column.as_str())
        .trim()
        .to_ascii_lowercase()
}

pub(super) fn first_nonempty<'a>(values: &[Option<&'a str>]) -> Option<&'a str> {
    values
        .iter()
        .flatten()
        .copied()
        .find(|value| !value.trim().is_empty())
}

pub(super) fn object_text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

pub(super) fn tool_call_is_retained(body: &Value) -> bool {
    body.pointer("/state/input").is_some()
        || body.get("input").is_some()
        || body.get("arguments").is_some()
        || body.get("command").is_some()
        || body.get("toolCall").is_some()
        || body.get("tool_calls").is_some()
}

pub(super) fn is_retained_type(family: Option<OpenCodeNativeSchemaFamily>, value: &str) -> bool {
    matches!(
        normalize_token(value).as_str(),
        "user"
            | "assistant"
            | "system"
            | "text"
            | "reasoning"
            | "summary"
            | "notice"
            | "patch"
            | "stepstart"
            | "stepfinish"
            | "snapshot"
            | "toolcall"
            | "tooluse"
            | "agentswitched"
            | "modelswitched"
            | "synthetic"
            | "compaction"
    ) || (family != Some(OpenCodeNativeSchemaFamily::MessagePart)
        && normalize_token(value) == "message")
}

pub(super) fn is_ignored_type(family: OpenCodeNativeSchemaFamily, value: &str) -> bool {
    family == OpenCodeNativeSchemaFamily::MessagePart && normalize_token(value) == "file"
}

pub(super) fn is_tool_token(value: &str) -> bool {
    matches!(normalize_token(value).as_str(), "tool" | "shell")
}

pub(super) fn is_direct_output_token(value: &str) -> bool {
    let value = normalize_token(value);
    matches!(
        value.as_str(),
        "result"
            | "toolresult"
            | "toolresponse"
            | "commandresult"
            | "output"
            | "tooloutput"
            | "commandoutput"
    ) || value.ends_with("result")
}

pub(super) fn is_output_key(value: &str, child: &Value, inside_tokens: bool) -> bool {
    let value = normalize_token(value);
    if inside_tokens && value == "output" && child.is_number() {
        return false;
    }
    matches!(
        value.as_str(),
        "output"
            | "result"
            | "stdout"
            | "stderr"
            | "toolresult"
            | "commandresult"
            | "tooloutput"
            | "commandoutput"
    ) || value.ends_with("result")
        || value.ends_with("output")
}

pub(super) fn normalize_token(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
