use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::events::{codex_is_command_tool, codex_tool_name};
use crate::provider::tool_input;

const MAX_STRUCTURED_ARGUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_ARGUMENT_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexRepositoryToolEvidence {
    pub(crate) command: Option<String>,
    pub(crate) declared_workdir: Option<String>,
    pub(crate) structured_content: Value,
}

pub(crate) fn repository_tool_evidence(payload: &Value) -> Option<CodexRepositoryToolEvidence> {
    let item_type = payload.get("type").and_then(Value::as_str)?;
    let tool_name = codex_tool_name(payload, item_type);
    if !codex_is_command_tool(&tool_name) {
        return None;
    }
    let arguments = payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("action"))
        .or_else(|| payload.get("execution"))?;
    if serde_json::to_vec(arguments).ok()?.len() > MAX_STRUCTURED_ARGUMENT_BYTES {
        return None;
    }
    let command = tool_input::command(arguments);
    let declared_workdir = unique_named_literal(arguments, &["workdir"]);
    Some(CodexRepositoryToolEvidence {
        command,
        declared_workdir,
        structured_content: json!({
            "provider_native_tool": {
                "name": tool_name,
                "call_id": payload.get("call_id"),
                "arguments": arguments,
            }
        }),
    })
}

fn unique_named_literal(value: &Value, names: &[&str]) -> Option<String> {
    let mut values = BTreeSet::new();
    collect_named_literals(value, names, &mut values, 0);
    (values.len() == 1)
        .then(|| values.into_iter().next())
        .flatten()
}

fn collect_named_literals(
    value: &Value,
    names: &[&str],
    values: &mut BTreeSet<String>,
    depth: usize,
) {
    if depth > MAX_ARGUMENT_DEPTH || values.len() > 1 {
        return;
    }
    match value {
        Value::Object(object) => {
            for name in names {
                if let Some(value) = object.get(*name).and_then(Value::as_str) {
                    insert_literal(value, values);
                }
            }
            for key in ["arguments", "args", "input", "execution"] {
                if let Some(value) = object.get(key) {
                    collect_named_literals(value, names, values, depth + 1);
                }
            }
        }
        Value::String(text) if text.len() <= MAX_STRUCTURED_ARGUMENT_BYTES => {
            if let Ok(decoded) = serde_json::from_str::<Value>(text) {
                collect_named_literals(&decoded, names, values, depth + 1);
            } else {
                collect_javascript_literals(text, names, values);
            }
        }
        Value::Array(_) | Value::String(_) | Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn collect_javascript_literals(text: &str, names: &[&str], values: &mut BTreeSet<String>) {
    for name in names {
        for marker in [
            format!("\"{name}\""),
            format!("'{name}'"),
            (*name).to_owned(),
        ] {
            let mut remainder = text;
            while let Some(index) = remainder.find(&marker) {
                if marker == *name && !bare_name_is_bounded(remainder, index, name.len()) {
                    remainder = remainder.get(index + marker.len()..).unwrap_or_default();
                    continue;
                }
                let after = remainder.get(index + marker.len()..).unwrap_or_default();
                let Some(after_colon) = after.trim_start().strip_prefix(':') else {
                    remainder = after;
                    continue;
                };
                let literal = after_colon.trim_start();
                if let Some((value, consumed)) = quoted_literal(literal) {
                    insert_literal(&value, values);
                    remainder = literal.get(consumed..).unwrap_or_default();
                } else {
                    remainder = after;
                }
                if values.len() > 1 {
                    return;
                }
            }
        }
    }
}

fn bare_name_is_bounded(value: &str, index: usize, length: usize) -> bool {
    let identifier = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$');
    let bytes = value.as_bytes();
    index
        .checked_sub(1)
        .is_none_or(|before| !identifier(bytes[before]))
        && bytes
            .get(index.saturating_add(length))
            .is_none_or(|after| !identifier(*after))
}

fn quoted_literal(value: &str) -> Option<(String, usize)> {
    let mut characters = value.char_indices();
    let (_, quote) = characters.next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let mut output = String::new();
    let mut escaped = false;
    for (index, character) in characters {
        if escaped {
            output.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            return Some((output, index + character.len_utf8()));
        } else {
            output.push(character);
        }
    }
    None
}

fn insert_literal(value: &str, values: &mut BTreeSet<String>) {
    let value = value.trim();
    if !value.is_empty() && !value.contains('\0') && value.len() <= MAX_STRUCTURED_ARGUMENT_BYTES {
        values.insert(value.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::repository_tool_evidence;

    #[test]
    fn retains_raw_arguments_and_exact_custom_exec_literals() {
        let payload = json!({
            "type": "custom_tool_call",
            "name": "exec_command",
            "call_id": "call-1",
            "arguments": "const r = await tools.exec_command({cmd:\"git status\",workdir:\"/repo\",yield_time_ms:10000}); text(r.output);"
        });
        let evidence = repository_tool_evidence(&payload).unwrap();
        assert_eq!(evidence.command.as_deref(), Some("git status"));
        assert_eq!(evidence.declared_workdir.as_deref(), Some("/repo"));
        assert_eq!(
            evidence.structured_content["provider_native_tool"]["arguments"],
            payload["arguments"]
        );
    }
}
