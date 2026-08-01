use std::path::Path;

use ctx_history_core::RepositoryFileObservationKind;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::events::codex_tool_name;
use crate::repository_attribution::UnscopedFileObservation;

#[path = "repository/outcomes.rs"]
mod outcomes;

pub(crate) use outcomes::{repository_result_evidence, CodexRepositoryResultEvidence};

const MAX_STRUCTURED_ARGUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_WORKDIR_BYTES: usize = 16 * 1024;
const MAX_CALL_ID_BYTES: usize = 1024;
const MAX_CONTINUATION_CELL_ID_BYTES: usize = 1024;
const MAX_STATIC_NESTED_TOOL_CALLS: usize = 24;
const MAX_STATIC_LITERAL_DEPTH: usize = 32;
const MAX_STATIC_LITERAL_ITEMS: usize = 256;
const CODEX_CONTINUATION_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/continuation-call-id/v1\0";
const CODEX_COMMAND_DOMAIN: &[u8] = b"ctx/codex-nativepath/exact-command/v1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexRepositoryToolEvidence {
    pub(crate) tool_name: String,
    pub(crate) command: Option<String>,
    pub(crate) declared_workdir: Option<String>,
    pub(crate) continuation_cell_id: Option<String>,
    pub(crate) file_observations: Vec<UnscopedFileObservation>,
    pub(crate) structured_content: Value,
}

/// Reads measured native argument objects and Codex's static nested-tool
/// orchestration envelope. The nested path decodes JSON-compatible literals;
/// it never evaluates JavaScript or executes captured commands.
pub(crate) fn repository_tool_evidence(payload: &Value) -> Vec<CodexRepositoryToolEvidence> {
    let Some(item_type) = payload.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };
    let tool_name = codex_tool_name(payload, item_type);
    if item_type == "custom_tool_call" && tool_name == "exec" {
        return nested_exec_tool_evidence(payload).unwrap_or_default();
    }
    if !matches!(tool_name.as_str(), "exec_command" | "wait") {
        return Vec::new();
    }
    native_tool_evidence(payload, tool_name)
        .into_iter()
        .collect()
}

fn native_tool_evidence(payload: &Value, tool_name: String) -> Option<CodexRepositoryToolEvidence> {
    if payload
        .get("name")
        .zip(payload.get("tool"))
        .is_some_and(|(name, tool)| name != tool)
    {
        return None;
    }
    let call_id = bounded_literal(
        payload.get("call_id")?.as_str()?,
        MAX_CALL_ID_BYTES,
        control_identifier,
    )?;
    let arguments = exact_one_of(payload, "arguments", "input")?;
    let arguments = decode_top_level_argument_object(arguments)?;

    let (command, declared_workdir, continuation_cell_id, schema, command_sha256) =
        if tool_name == "exec_command" {
            let command =
                bounded_literal(arguments.get("cmd")?.as_str()?, MAX_COMMAND_BYTES, |_| true)?;
            let declared_workdir = match arguments.get("workdir") {
                Some(value) => Some(bounded_literal(value.as_str()?, MAX_WORKDIR_BYTES, |_| {
                    true
                })?),
                None => None,
            };
            let command_sha256 = digest_hex(CODEX_COMMAND_DOMAIN, command.as_bytes());
            (
                Some(command),
                declared_workdir,
                None,
                "codex_exec_command_args_v1",
                Some(command_sha256),
            )
        } else {
            let cell_id = bounded_literal(
                arguments.get("cell_id")?.as_str()?,
                MAX_CONTINUATION_CELL_ID_BYTES,
                control_identifier,
            )?;
            (None, None, Some(cell_id), "codex_wait_args_v1", None)
        };

    Some(CodexRepositoryToolEvidence {
        tool_name: tool_name.clone(),
        command,
        declared_workdir,
        continuation_cell_id,
        file_observations: Vec::new(),
        structured_content: json!({
            "provider_native_tool": {
                "provider": "codex",
                "name": tool_name,
                "call_id": call_id,
                "argument_schema": schema,
                "command_sha256": command_sha256,
                "raw_arguments_retained": false,
            }
        }),
    })
}

fn nested_exec_tool_evidence(payload: &Value) -> Option<Vec<CodexRepositoryToolEvidence>> {
    let call_id = bounded_literal(
        payload.get("call_id")?.as_str()?,
        MAX_CALL_ID_BYTES,
        control_identifier,
    )?;
    let source = payload.get("input")?.as_str()?;
    if source.len() > MAX_STRUCTURED_ARGUMENT_BYTES {
        return None;
    }
    let calls = StaticJsParser::new(source).parse_program()?;
    let mut evidence = Vec::new();
    for (index, call) in calls.into_iter().enumerate() {
        match call {
            StaticNestedToolCall::ExecCommand(arguments) => {
                let command =
                    bounded_literal(arguments.get("cmd")?.as_str()?, MAX_COMMAND_BYTES, |_| true)?;
                let declared_workdir = match arguments.get("workdir") {
                    Some(value) => {
                        let value = bounded_literal(value.as_str()?, MAX_WORKDIR_BYTES, |_| true)?;
                        Some(bounded_absolute_path(&value).then_some(value)?)
                    }
                    None => None,
                };
                let command_sha256 = digest_hex(CODEX_COMMAND_DOMAIN, command.as_bytes());
                evidence.push(CodexRepositoryToolEvidence {
                    tool_name: "exec_command".to_owned(),
                    command: Some(command),
                    declared_workdir,
                    continuation_cell_id: None,
                    file_observations: Vec::new(),
                    structured_content: json!({
                        "provider_native_tool": {
                            "provider": "codex",
                            "name": "exec_command",
                            "outer_name": "exec",
                            "call_id": call_id,
                            "nested_activity_index": index,
                            "argument_schema": "codex_nested_exec_command_literal_v2",
                            "command_sha256": command_sha256,
                            "raw_arguments_retained": false,
                        }
                    }),
                });
            }
            StaticNestedToolCall::ApplyPatch(patch) => {
                let mut patch_paths = patch
                    .lines()
                    .filter_map(|line| {
                        let line = line.trim();
                        [
                            ("*** Add File:", RepositoryFileObservationKind::Created),
                            ("*** Update File:", RepositoryFileObservationKind::Modified),
                            ("*** Delete File:", RepositoryFileObservationKind::Deleted),
                        ]
                        .into_iter()
                        .find_map(|(header, kind)| {
                            line.strip_prefix(header)
                                .map(str::trim)
                                .filter(|path| bounded_path(path))
                                .map(|path| (path.to_owned(), kind))
                        })
                    })
                    .collect::<Vec<_>>();
                let mut seen = std::collections::HashSet::new();
                patch_paths.retain(|(path, _)| seen.insert(path.clone()));
                if patch_paths.is_empty() {
                    continue;
                }
                evidence.push(CodexRepositoryToolEvidence {
                    tool_name: "apply_patch".to_owned(),
                    command: None,
                    declared_workdir: None,
                    continuation_cell_id: None,
                    file_observations: patch_paths
                        .iter()
                        .cloned()
                        .map(|(path, kind)| UnscopedFileObservation {
                            path,
                            prior_path: None,
                            kind,
                        })
                        .collect(),
                    structured_content: json!({
                        "provider_native_tool": {
                            "provider": "codex",
                            "name": "apply_patch",
                            "outer_name": "exec",
                            "call_id": call_id,
                            "nested_activity_index": index,
                            "argument_schema": "codex_nested_apply_patch_literal_v2",
                            "static_patch_paths": patch_paths.len(),
                            "raw_arguments_retained": false,
                        }
                    }),
                });
            }
        }
    }
    Some(evidence)
}

enum StaticNestedToolCall {
    ExecCommand(Map<String, Value>),
    ApplyPatch(String),
}

struct StaticJsParser<'a> {
    source: &'a [u8],
    cursor: usize,
}

impl<'a> StaticJsParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            cursor: 0,
        }
    }

    fn parse_program(mut self) -> Option<Vec<StaticNestedToolCall>> {
        let mut calls = Vec::new();
        let mut terminal_output_statements = 0_usize;
        loop {
            self.skip_program_trivia()?;
            if self.cursor == self.source.len() {
                break;
            }
            if terminal_output_statements == 0 {
                if let Some(call) = self.parse_tool_statement() {
                    calls.push(call);
                    if calls.len() > MAX_STATIC_NESTED_TOOL_CALLS {
                        return None;
                    }
                    continue;
                }
            }
            if self.parse_terminal_output_statement() {
                terminal_output_statements += 1;
                if terminal_output_statements > MAX_STATIC_NESTED_TOOL_CALLS {
                    return None;
                }
                continue;
            }
            return None;
        }
        Some(calls)
    }

    fn parse_terminal_output_statement(&mut self) -> bool {
        let checkpoint = self.cursor;
        if !self.consume_keyword("text") {
            return false;
        }
        self.skip_whitespace();
        if !self.consume_byte(b'(') {
            self.cursor = checkpoint;
            return false;
        }
        self.skip_whitespace();
        if self.parse_identifier().is_none() {
            self.cursor = checkpoint;
            return false;
        }
        loop {
            if !self.consume_byte(b'.') {
                break;
            }
            if self.parse_identifier().is_none() {
                self.cursor = checkpoint;
                return false;
            }
        }
        self.skip_whitespace();
        if !self.consume_byte(b')') {
            self.cursor = checkpoint;
            return false;
        }
        if !self.consume_statement_terminator() {
            self.cursor = checkpoint;
            return false;
        }
        true
    }

    fn consume_statement_terminator(&mut self) -> bool {
        self.skip_whitespace();
        if self.consume_byte(b';') {
            return true;
        }
        let saved = self.cursor;
        if self.skip_program_trivia().is_some() && self.cursor == self.source.len() {
            self.cursor = saved;
            true
        } else {
            self.cursor = saved;
            false
        }
    }

    fn parse_tool_statement(&mut self) -> Option<StaticNestedToolCall> {
        let checkpoint = self.cursor;
        if self.consume_keyword("const") {
            self.skip_whitespace();
            self.parse_identifier()?;
            self.skip_whitespace();
            if !self.consume_byte(b'=') {
                self.cursor = checkpoint;
                return None;
            }
            self.skip_whitespace();
        }
        if !self.consume_keyword("await") {
            self.cursor = checkpoint;
            return None;
        }
        self.skip_whitespace();
        if !self.consume_bytes(b"tools.") {
            self.cursor = checkpoint;
            return None;
        }
        let method = self.parse_identifier()?;
        self.skip_whitespace();
        if !self.consume_byte(b'(') {
            self.cursor = checkpoint;
            return None;
        }
        self.skip_whitespace();
        let value = self.parse_static_value(0)?;
        self.skip_whitespace();
        if !self.consume_byte(b')') {
            self.cursor = checkpoint;
            return None;
        }
        if !self.consume_statement_terminator() {
            self.cursor = checkpoint;
            return None;
        }
        match (method.as_str(), value) {
            ("exec_command", Value::Object(arguments)) => {
                Some(StaticNestedToolCall::ExecCommand(arguments))
            }
            ("apply_patch", Value::String(patch)) => Some(StaticNestedToolCall::ApplyPatch(patch)),
            _ => {
                self.cursor = checkpoint;
                None
            }
        }
    }

    fn parse_static_value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_STATIC_LITERAL_DEPTH {
            return None;
        }
        self.skip_whitespace();
        match self.source.get(self.cursor).copied()? {
            b'"' => self.parse_json_string().map(Value::String),
            b'{' => self.parse_static_object(depth + 1).map(Value::Object),
            b'[' => self.parse_static_array(depth + 1).map(Value::Array),
            b't' if self.consume_keyword("true") => Some(Value::Bool(true)),
            b'f' if self.consume_keyword("false") => Some(Value::Bool(false)),
            b'n' if self.consume_keyword("null") => Some(Value::Null),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        }
    }

    fn parse_static_object(&mut self, depth: usize) -> Option<Map<String, Value>> {
        self.consume_byte(b'{').then_some(())?;
        let mut object = Map::new();
        loop {
            self.skip_whitespace();
            if self.consume_byte(b'}') {
                return Some(object);
            }
            if object.len() >= MAX_STATIC_LITERAL_ITEMS {
                return None;
            }
            let key = if self.source.get(self.cursor) == Some(&b'"') {
                self.parse_json_string()?
            } else {
                self.parse_identifier()?
            };
            self.skip_whitespace();
            self.consume_byte(b':').then_some(())?;
            let value = self.parse_static_value(depth)?;
            if object.insert(key, value).is_some() {
                return None;
            }
            self.skip_whitespace();
            if self.consume_byte(b'}') {
                return Some(object);
            }
            self.consume_byte(b',').then_some(())?;
        }
    }

    fn parse_static_array(&mut self, depth: usize) -> Option<Vec<Value>> {
        self.consume_byte(b'[').then_some(())?;
        let mut values = Vec::new();
        loop {
            self.skip_whitespace();
            if self.consume_byte(b']') {
                return Some(values);
            }
            if values.len() >= MAX_STATIC_LITERAL_ITEMS {
                return None;
            }
            values.push(self.parse_static_value(depth)?);
            self.skip_whitespace();
            if self.consume_byte(b']') {
                return Some(values);
            }
            self.consume_byte(b',').then_some(())?;
        }
    }

    fn parse_json_string(&mut self) -> Option<String> {
        let start = self.cursor;
        self.consume_byte(b'"').then_some(())?;
        let mut escaped = false;
        while let Some(byte) = self.source.get(self.cursor).copied() {
            self.cursor += 1;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return serde_json::from_slice(self.source.get(start..self.cursor)?).ok();
            } else if byte.is_ascii_control() {
                return None;
            }
        }
        None
    }

    fn parse_number(&mut self) -> Option<Value> {
        let start = self.cursor;
        while self.source.get(self.cursor).is_some_and(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E')
        }) {
            self.cursor += 1;
        }
        serde_json::from_slice(self.source.get(start..self.cursor)?).ok()
    }

    fn parse_identifier(&mut self) -> Option<String> {
        let start = self.cursor;
        let first = self.source.get(self.cursor).copied()?;
        if !first.is_ascii_alphabetic() && !matches!(first, b'_' | b'$') {
            return None;
        }
        self.cursor += 1;
        while self
            .source
            .get(self.cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        {
            self.cursor += 1;
        }
        std::str::from_utf8(self.source.get(start..self.cursor)?)
            .ok()
            .map(str::to_owned)
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        let start = self.cursor;
        if !self.consume_bytes(keyword.as_bytes()) {
            return false;
        }
        if self
            .source
            .get(self.cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        {
            self.cursor = start;
            return false;
        }
        true
    }

    fn consume_bytes(&mut self, expected: &[u8]) -> bool {
        if self
            .source
            .get(self.cursor..self.cursor.saturating_add(expected.len()))
            == Some(expected)
        {
            self.cursor += expected.len();
            true
        } else {
            false
        }
    }

    fn consume_byte(&mut self, expected: u8) -> bool {
        if self.source.get(self.cursor) == Some(&expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .source
            .get(self.cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.cursor += 1;
        }
    }

    fn skip_program_trivia(&mut self) -> Option<()> {
        loop {
            self.skip_whitespace();
            if self.source.get(self.cursor..self.cursor.saturating_add(2)) == Some(b"//") {
                self.cursor += 2;
                while self
                    .source
                    .get(self.cursor)
                    .is_some_and(|byte| *byte != b'\n')
                {
                    self.cursor += 1;
                }
                continue;
            }
            if self.source.get(self.cursor..self.cursor.saturating_add(2)) == Some(b"/*") {
                self.cursor += 2;
                while self.source.get(self.cursor..self.cursor.saturating_add(2)) != Some(b"*/") {
                    self.cursor += 1;
                    if self.cursor >= self.source.len() {
                        return None;
                    }
                }
                self.cursor += 2;
                continue;
            }
            return Some(());
        }
    }
}

fn bounded_absolute_path(value: &str) -> bool {
    bounded_path(value) && Path::new(value).is_absolute()
}

fn bounded_path(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= MAX_WORKDIR_BYTES
        && !value.contains('\0')
        && !value.contains(['\r', '\n'])
}

fn decode_top_level_argument_object(value: &Value) -> Option<Map<String, Value>> {
    if serde_json::to_vec(value).ok()?.len() > MAX_STRUCTURED_ARGUMENT_BYTES {
        return None;
    }
    let decoded = match value {
        Value::Object(object) => Value::Object(object.clone()),
        Value::String(text) if text.len() <= MAX_STRUCTURED_ARGUMENT_BYTES => {
            serde_json::from_str::<Value>(text).ok()?
        }
        _ => return None,
    };
    let object = decoded.as_object()?.clone();
    (serde_json::to_vec(&object).ok()?.len() <= MAX_STRUCTURED_ARGUMENT_BYTES).then_some(object)
}

fn bounded_literal(value: &str, maximum: usize, predicate: impl Fn(u8) -> bool) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= maximum
        && !value.contains('\0')
        && value.bytes().all(predicate))
    .then(|| value.to_owned())
}

fn control_identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
}

pub(crate) fn running_continuation_cell_id(payload: &Value) -> Option<String> {
    let output = repository_result_output(payload)?.as_str()?;
    let first_line = output.lines().next()?.trim();
    let cell_id = first_line.strip_prefix("Script running with cell ID ")?;
    bounded_literal(cell_id, MAX_CONTINUATION_CELL_ID_BYTES, control_identifier)
}

pub(crate) fn terminal_continuation_result(payload: &Value) -> bool {
    repository_result_output(payload)
        .and_then(Value::as_str)
        .is_some_and(|output| {
            let output = output.trim_start();
            output == "Script completed"
                || output.starts_with("Script completed\n")
                || output.starts_with("Process exited with code ")
        })
}

fn repository_result_output(payload: &Value) -> Option<&Value> {
    exact_one_of(payload, "output", "result")
}

fn exact_one_of<'a>(payload: &'a Value, left: &str, right: &str) -> Option<&'a Value> {
    match (payload.get(left), payload.get(right)) {
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) | (Some(_), Some(_)) => None,
    }
}

pub(crate) fn continuation_call_id_sha256(call_id: &str) -> [u8; 32] {
    digest(CODEX_CONTINUATION_CALL_ID_DOMAIN, call_id.as_bytes())
}

fn digest(domain: &[u8], value: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(value);
    hasher.finalize().into()
}

fn digest_hex(domain: &[u8], value: &[u8]) -> String {
    digest(domain, value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use ctx_history_core::RepositoryFileObservationKind;
    use serde_json::json;

    use super::repository_tool_evidence;

    #[test]
    fn accepts_only_one_top_level_native_argument_decode_and_redacts_it() {
        let payload = json!({
            "type": "function_call",
            "name": "exec_command",
            "call_id": "call-1",
            "arguments": json!({
                "cmd": "git status",
                "workdir": "/repo",
                "yield_time_ms": 10000,
                "decoy": {"cmd": "git commit -m decoy", "workdir": "/other"}
            }).to_string()
        });
        let evidence = repository_tool_evidence(&payload).remove(0);
        assert_eq!(evidence.command.as_deref(), Some("git status"));
        assert_eq!(evidence.declared_workdir.as_deref(), Some("/repo"));
        let encoded = serde_json::to_string(&evidence.structured_content).unwrap();
        assert!(!encoded.contains("git status"));
        assert!(!encoded.contains("decoy"));
        assert_eq!(
            evidence.structured_content["provider_native_tool"]["raw_arguments_retained"],
            false
        );
    }

    #[test]
    fn javascript_wrappers_nested_only_commands_and_missing_call_ids_abstain() {
        for payload in [
            json!({
                "type": "custom_tool_call",
                "name": "exec_command",
                "call_id": "call-1",
                "arguments": "tools.exec_command({cmd:'git status',workdir:'/repo'})"
            }),
            json!({
                "type": "function_call",
                "name": "exec_command",
                "call_id": "call-2",
                "arguments": {"dead_branch": {"cmd": "git status", "workdir": "/repo"}}
            }),
            json!({
                "type": "function_call",
                "name": "exec_command",
                "arguments": {"cmd": "git status", "workdir": "/repo"}
            }),
            json!({
                "type": "function_call",
                "name": "exec_command",
                "call_id": "call-3",
                "arguments": {"cmd": "git status"},
                "input": {"cmd": "git commit -m decoy"}
            }),
            json!({
                "type": "function_call",
                "name": "exec_command",
                "tool": "wait",
                "call_id": "call-4",
                "arguments": {"cmd": "git status"}
            }),
        ] {
            assert!(repository_tool_evidence(&payload).is_empty());
        }
    }

    #[test]
    fn exact_top_level_literal_calls_decode_commands_and_patch_headers() {
        let payload = json!({
            "type": "custom_tool_call",
            "name": "exec",
            "call_id": "outer-1",
            "input": r#"
                const first = await tools.exec_command({cmd:"git status",workdir:"/repo/one",yield_time_ms:10000});
                const second = await tools.exec_command({cmd:"git log -1",workdir:"/repo/two"});
                const patched = await tools.apply_patch("*** Begin Patch\n*** Add File: /repo/one/src/new.rs\n*** Update File: /repo/one/src/lib.rs\n*** Delete File: /repo/one/src/old.rs\n*** End Patch");
                text(first.output);
            "#
        });
        let evidence = repository_tool_evidence(&payload);
        assert_eq!(evidence.len(), 3);
        assert_eq!(evidence[0].tool_name, "exec_command");
        assert_eq!(evidence[0].declared_workdir.as_deref(), Some("/repo/one"));
        assert_eq!(evidence[0].command.as_deref(), Some("git status"));
        assert_eq!(evidence[1].declared_workdir.as_deref(), Some("/repo/two"));
        assert_eq!(evidence[1].command.as_deref(), Some("git log -1"));
        assert_eq!(evidence[2].tool_name, "apply_patch");
        assert_eq!(evidence[2].file_observations.len(), 3);
        assert_eq!(
            evidence[2].file_observations[0].path,
            "/repo/one/src/new.rs"
        );
        assert_eq!(
            evidence[2].file_observations[0].kind,
            RepositoryFileObservationKind::Created
        );
        assert_eq!(
            evidence[2].file_observations[1].path,
            "/repo/one/src/lib.rs"
        );
        assert_eq!(
            evidence[2].file_observations[1].kind,
            RepositoryFileObservationKind::Modified
        );
        assert_eq!(
            evidence[2].file_observations[2].path,
            "/repo/one/src/old.rs"
        );
        assert_eq!(
            evidence[2].file_observations[2].kind,
            RepositoryFileObservationKind::Deleted
        );
    }

    #[test]
    fn inert_or_dynamic_javascript_never_emits_executed_tool_evidence() {
        for source in [
            r#"const example = "tools.exec_command({cmd:\"git commit -m inert\",workdir:\"/repo\"})"; text(example);"#,
            r#"// tools.exec_command({cmd:"git commit -m comment",workdir:"/repo"})
                text("*** Add File: /repo/inert.rs");"#,
            r#"if (false) { await tools.exec_command({cmd:"git commit -m dead",workdir:"/repo"}); }"#,
            r#"const args = {cmd:"git commit -m dynamic",workdir:"/repo"};
                const result = await tools.exec_command(args);"#,
            r#"const result = await tools.exec_command({cmd:"git status",workdir:"/repo"});
                observeDynamically(result);"#,
            r#"text(prior.output);
                const result = await tools.exec_command({cmd:"git status",workdir:"/repo"});"#,
            r#"const patch = "*** Begin Patch\n*** Add File: /repo/inert.rs\n*** End Patch"; text(patch);"#,
        ] {
            let payload = json!({
                "type": "custom_tool_call",
                "name": "exec",
                "call_id": "outer-inert",
                "input": source,
            });
            assert!(repository_tool_evidence(&payload).is_empty(), "{source}");
        }
    }

    #[test]
    fn continuation_controls_require_exact_bounded_identifiers() {
        assert_eq!(
            super::running_continuation_cell_id(&json!({
                "output": "Script running with cell ID cell-7\n"
            }))
            .as_deref(),
            Some("cell-7")
        );
        assert!(super::running_continuation_cell_id(&json!({
            "output": "prose says Script running with cell ID cell-7"
        }))
        .is_none());
        assert!(super::terminal_continuation_result(&json!({
            "output": "Script completed\nFinal output:\nok"
        })));
        assert!(!super::terminal_continuation_result(&json!({
            "output": "Script completed",
            "result": "Process exited with code 0"
        })));
    }
}
