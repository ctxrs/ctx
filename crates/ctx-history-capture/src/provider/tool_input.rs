use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

const MAX_TOOL_INPUT_BYTES: usize = 256 * 1024;
const MAX_COMMAND_BYTES: usize = 64 * 1024;
const MAX_DIRECT_ARGV_ITEMS: usize = 1_024;

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

/// Extracts one bounded command value and decodes only a direct, static argv.
///
/// Shell composition, redirection, expansion, comments, and malformed quoting
/// abstain. This is lexical decoding for provider-supplied tool input; it does
/// not resolve executables, source profiles, or emulate a shell.
pub(crate) fn direct_argv(value: &Value) -> Option<Vec<String>> {
    direct_command_argv(&direct_command(value)?)
}

fn direct_command(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => top_level_command(object),
        Value::String(text) if text.len() <= MAX_TOOL_INPUT_BYTES => {
            match serde_json::from_str::<Value>(text) {
                Ok(Value::Object(_)) => serde_json::from_str::<DirectCommandObject>(text)
                    .ok()?
                    .into_command(),
                Ok(_) => None,
                Err(_) => bounded_command(text),
            }
        }
        Value::Array(_) | Value::String(_) | Value::Null | Value::Bool(_) | Value::Number(_) => {
            None
        }
    }
}

#[derive(Deserialize)]
struct DirectCommandObject {
    cmd: Option<String>,
    command: Option<String>,
    shell_command: Option<String>,
}

impl DirectCommandObject {
    fn into_command(self) -> Option<String> {
        let commands = [self.cmd, self.command, self.shell_command];
        let mut commands = commands.into_iter().flatten();
        let command = commands.next()?;
        if commands.next().is_some() {
            return None;
        }
        bounded_command(&command)
    }
}

fn top_level_command(object: &serde_json::Map<String, Value>) -> Option<String> {
    let mut selected = None;
    for name in ["cmd", "command", "shell_command"] {
        if let Some(value) = object.get(name) {
            if selected.is_some() {
                return None;
            }
            selected = Some(value.as_str()?);
        }
    }
    bounded_command(selected?)
}

fn bounded_command(command: &str) -> Option<String> {
    let command = command.trim();
    (!command.is_empty() && command.len() <= MAX_COMMAND_BYTES && !command.contains('\0'))
        .then(|| command.to_owned())
}

pub(crate) fn direct_command_argv(command: &str) -> Option<Vec<String>> {
    if command.is_empty() || command.len() > MAX_COMMAND_BYTES || command.contains('\0') {
        return None;
    }

    let mut argv = Vec::new();
    let mut token = String::new();
    let mut token_started = false;
    let mut quote = None;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        match quote {
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                } else {
                    token.push(character);
                }
            }
            Some('"') => match character {
                '"' => quote = None,
                '$' | '`' => return None,
                '\\' => {
                    let escaped = characters.next()?;
                    if !matches!(escaped, '"' | '\\' | '$' | '`') {
                        return None;
                    }
                    token.push(escaped);
                }
                other => token.push(other),
            },
            Some(_) => return None,
            None => match character {
                '\'' | '"' => {
                    quote = Some(character);
                    token_started = true;
                }
                '\\' => {
                    let escaped = characters.next()?;
                    if matches!(escaped, '\n' | '\r' | '\0') {
                        return None;
                    }
                    token.push(escaped);
                    token_started = true;
                }
                ';' | '|' | '&' | '<' | '>' | '(' | ')' | '\n' | '\r' | '$' | '`' | '*' | '?'
                | '[' | ']' | '{' | '}' => return None,
                '#' if !token_started => return None,
                value if value.is_whitespace() => {
                    if token_started {
                        push_direct_arg(&mut argv, std::mem::take(&mut token))?;
                        token_started = false;
                    }
                }
                other => {
                    token.push(other);
                    token_started = true;
                }
            },
        }
        if token.len() > MAX_COMMAND_BYTES {
            return None;
        }
    }
    if quote.is_some() {
        return None;
    }
    if token_started {
        push_direct_arg(&mut argv, token)?;
    }
    (!argv.is_empty()).then_some(argv)
}

fn push_direct_arg(argv: &mut Vec<String>, value: String) -> Option<()> {
    if argv.len() >= MAX_DIRECT_ARGV_ITEMS {
        return None;
    }
    argv.push(value);
    Some(())
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

    use super::{
        command, direct_argv, direct_command_argv, is_command_tool, MAX_DIRECT_ARGV_ITEMS,
    };

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

    #[test]
    fn direct_argv_decodes_only_one_static_execution_atom() {
        assert!(is_command_tool("exec_command"));
        assert_eq!(
            direct_command_argv("ctx search 'quoted query' --term=two words"),
            Some(vec![
                "ctx".to_owned(),
                "search".to_owned(),
                "quoted query".to_owned(),
                "--term=two".to_owned(),
                "words".to_owned(),
            ])
        );
        assert_eq!(
            direct_argv(&json!({"cmd": "ctx.exe show event abc"})),
            Some(vec![
                "ctx.exe".to_owned(),
                "show".to_owned(),
                "event".to_owned(),
                "abc".to_owned(),
            ])
        );
        assert_eq!(
            direct_argv(&json!(r#"{"command":"ctx search exact"}"#)),
            Some(vec![
                "ctx".to_owned(),
                "search".to_owned(),
                "exact".to_owned(),
            ])
        );
        assert_eq!(
            direct_command_argv("ctx search '' escaped\\ query"),
            Some(vec![
                "ctx".to_owned(),
                "search".to_owned(),
                String::new(),
                "escaped query".to_owned(),
            ])
        );
    }

    #[test]
    fn direct_argv_abstains_on_shell_behavior_and_bounds() {
        for command in [
            "cd /tmp && ctx search query",
            "ctx search query | tee result",
            "ctx search query > result",
            "ctx search $(dynamic)",
            "ctx search \"$DYNAMIC\"",
            "ctx search *.md",
            "ctx search query # comment",
            "ctx search 'unterminated",
            "ctx search one\ntwo",
        ] {
            assert!(
                direct_command_argv(command).is_none(),
                "accepted {command:?}"
            );
        }

        let too_many = std::iter::repeat_n("x", MAX_DIRECT_ARGV_ITEMS + 1)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(direct_command_argv(&too_many).is_none());
    }

    #[test]
    fn direct_argv_uses_only_one_exact_top_level_command_field() {
        for input in [
            json!({"arguments": {"cmd": "ctx search nested"}}),
            json!({"cmd": "ctx search one", "command": "ctx search two"}),
            json!({"cmd": 42}),
            json!(r#"{"input":{"cmd":"ctx search nested"}}"#),
            json!(r#"{"cmd":"ctx search one","cmd":"ctx search two"}"#),
            json!(r#"{"cmd":"ctx search exact"} trailing"#),
        ] {
            assert!(direct_argv(&input).is_none(), "accepted {input:?}");
        }
    }
}
