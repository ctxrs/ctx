use std::collections::BTreeSet;

use serde_json::Value;

const MAX_TOOL_INPUT_BYTES: usize = 256 * 1024;
const MAX_COMMAND_BYTES: usize = 64 * 1024;

pub(crate) fn is_command_tool(name: &str) -> bool {
    matches!(name, "exec" | "exec_command" | "shell" | "bash" | "command")
}

pub(crate) fn command(value: &Value) -> Option<String> {
    let mut values = BTreeSet::new();
    collect_object_strings(value, &["cmd", "command", "shell_command"], &mut values, 0);
    if values.len() == 1 {
        return values.into_iter().next();
    }
    if !values.is_empty() {
        return None;
    }
    let text = value.as_str()?.trim();
    if text.is_empty()
        || text.len() > MAX_COMMAND_BYTES
        || text.contains('\0')
        || looks_like_exec_wrapper(text)
    {
        return None;
    }
    Some(text.to_owned())
}

fn collect_object_strings(
    value: &Value,
    names: &[&str],
    values: &mut BTreeSet<String>,
    depth: usize,
) {
    if depth > 8 || values.len() > 1 {
        return;
    }
    match value {
        Value::Object(object) => {
            for name in names {
                if let Some(value) = object.get(*name).and_then(Value::as_str) {
                    insert_bounded(value, values);
                }
            }
            for key in ["arguments", "args", "input", "execution"] {
                if let Some(value) = object.get(key) {
                    collect_object_strings(value, names, values, depth + 1);
                }
            }
        }
        Value::String(text) if text.len() <= MAX_TOOL_INPUT_BYTES => {
            if let Ok(decoded) = serde_json::from_str::<Value>(text) {
                collect_object_strings(&decoded, names, values, depth + 1);
            } else {
                collect_named_literals(text, names, values);
            }
        }
        Value::Array(_) | Value::String(_) | Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn collect_named_literals(text: &str, names: &[&str], values: &mut BTreeSet<String>) {
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
                let Some(after_marker) = remainder.get(index + marker.len()..) else {
                    return;
                };
                let suffix = after_marker.trim_start();
                let Some(suffix) = suffix.strip_prefix(':') else {
                    remainder = after_marker;
                    continue;
                };
                let literal = suffix.trim_start();
                let leading_whitespace = suffix.len().saturating_sub(literal.len());
                if let Some((value, consumed)) = quoted_literal(literal) {
                    insert_bounded(&value, values);
                    remainder = suffix
                        .get(leading_whitespace.saturating_add(consumed)..)
                        .unwrap_or_default();
                } else {
                    remainder = after_marker;
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
            output.push(match character {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
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

fn insert_bounded(value: &str, values: &mut BTreeSet<String>) {
    let value = value.trim();
    if !value.is_empty() && value.len() <= MAX_COMMAND_BYTES && !value.contains('\0') {
        values.insert(value.to_owned());
    }
}

fn looks_like_exec_wrapper(value: &str) -> bool {
    value.contains("tools.exec_command") || value.contains("tools.exec(")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::command;

    #[test]
    fn extracts_exact_custom_exec_command_without_executing_wrapper() {
        let input = json!(
            "const r = await tools.exec_command({cmd:\"git cherry-pick b29a185e\",workdir:\"/workspace/ctx\",yield_time_ms:30000}); text(r.output);"
        );
        assert_eq!(command(&input).as_deref(), Some("git cherry-pick b29a185e"));
    }

    #[test]
    fn ambiguous_wrappers_and_unparsed_wrapper_code_abstain() {
        let ambiguous = json!("tools.exec_command({cmd:'git status', command:'git commit'});");
        assert!(command(&ambiguous).is_none());
        assert!(command(&json!("tools.exec_command(dynamic);")).is_none());
        assert_eq!(command(&json!("git status")).as_deref(), Some("git status"));
    }

    #[test]
    fn ignores_unbounded_property_names_and_handles_whitespace_between_colon_and_value() {
        let input = json!(
            "tools.exec_command({notcmd:'git reset --hard', cmd   :   'git status', workdir :  \"/workspace/ctx\"});"
        );
        assert_eq!(command(&input).as_deref(), Some("git status"));
    }
}
