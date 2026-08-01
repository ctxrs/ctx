use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use ctx_history_core::{RepositoryFileObservationKind, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::events::{codex_tool_name, CodexToolCallContext};
use crate::{
    repository_attribution::{UnscopedFileObservation, MAX_COMMAND_BYTES},
    OutputOutcomeMetadata,
};

#[path = "repository/outcomes.rs"]
mod outcomes;

pub(crate) use outcomes::CodexRepositoryResultEvidence;

const MAX_STRUCTURED_ARGUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_WORKDIR_BYTES: usize = 16 * 1024;
const MAX_CALL_ID_BYTES: usize = 1024;
const MAX_CONTINUATION_CELL_ID_BYTES: usize = 1024;
const MAX_STATIC_NESTED_TOOL_CALLS: usize = 24;
const MAX_STATIC_BINDINGS: usize = 24;
const MAX_STATIC_PATCH_PATHS: usize = 256;
const MAX_STATIC_LITERAL_DEPTH: usize = 32;
const MAX_STATIC_LITERAL_ITEMS: usize = 256;
const CODEX_CONTINUATION_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/continuation-call-id/v1\0";
const CODEX_COMMAND_DOMAIN: &[u8] = b"ctx/codex-nativepath/exact-command/v1\0";

pub(crate) fn repository_result_evidence(
    payload: &Value,
    context: &CodexToolCallContext,
    result_call_id: &str,
    result_record_sha256: [u8; 32],
    observed_at_unix_ms: i64,
    result_outcome: &OutputOutcomeMetadata,
) -> Option<CodexRepositoryResultEvidence> {
    if let Some(evidence) = outcomes::repository_result_evidence(
        payload,
        context,
        result_call_id,
        result_record_sha256,
        observed_at_unix_ms,
        result_outcome,
    ) {
        return Some(evidence);
    }
    exact_linked_result_context(payload, context, result_call_id, result_record_sha256)
}

fn exact_linked_result_context(
    payload: &Value,
    context: &CodexToolCallContext,
    result_call_id: &str,
    result_record_sha256: [u8; 32],
) -> Option<CodexRepositoryResultEvidence> {
    repository_result_output(payload)?;
    let command = context.exact_command.clone()?;
    let origin_call_id = bounded_literal(
        context.origin_call_id.as_deref()?,
        MAX_CALL_ID_BYTES,
        control_identifier,
    )?;
    let result_call_id = bounded_literal(result_call_id, MAX_CALL_ID_BYTES, control_identifier)?;
    let origin_event_sequence = context.origin_event_sequence?;
    if context.command_too_large
        || context.correlation_ambiguous
        || context.continuation_capacity_exceeded
        || context.continuation_call_id_sha256.len()
            > crate::provider::codex::nativepath::MAX_CODEX_TOOL_CONTEXTS
        || context
            .continuation_call_id_sha256
            .iter()
            .collect::<HashSet<_>>()
            .len()
            != context.continuation_call_id_sha256.len()
        || (context.continuation_cell_id.is_some() && !terminal_continuation_result(payload))
    {
        return None;
    }
    Some(CodexRepositoryResultEvidence {
        command: Some(command),
        command_too_large: false,
        declared_workdir: context.declared_workdir.clone(),
        outcome_operation_repository_path: None,
        outcome_output_repository_path: None,
        structured_content: json!({
            "provider_native_tool_result": {
                "provider": "codex",
                "origin_call_id": origin_call_id,
                "result_call_id": result_call_id,
                "origin_event_sequence": origin_event_sequence,
                "continuation_call_id_sha256": context.continuation_call_id_sha256
                    .iter()
                    .map(hex_digest)
                    .collect::<Vec<_>>(),
                "result_record_sha256": hex_digest(&result_record_sha256),
                "result_context_schema": "codex_exact_linked_result_context_v1",
                "outcome_capture_revision": CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
                "captured_outcomes": 0,
                "raw_output_retained": false,
            }
        }),
        provider_native_repository_aliases: Vec::new(),
        outcomes: Vec::new(),
        abstentions: Vec::new(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexRepositoryToolEvidence {
    pub(crate) tool_name: String,
    pub(crate) command: Option<String>,
    pub(crate) command_too_large: bool,
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
    if tool_name == "apply_patch" {
        return native_patch_tool_evidence(payload, item_type)
            .into_iter()
            .collect();
    }
    if !matches!(tool_name.as_str(), "exec_command" | "wait") {
        return Vec::new();
    }
    native_tool_evidence(payload, tool_name)
        .into_iter()
        .collect()
}

fn native_patch_tool_evidence(
    payload: &Value,
    item_type: &str,
) -> Option<CodexRepositoryToolEvidence> {
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
    let input = exact_one_of(payload, "arguments", "input")?;
    let (patch, schema) = match input {
        Value::String(patch) if item_type == "custom_tool_call" => {
            (patch.clone(), "codex_apply_patch_input_v1")
        }
        Value::Object(_) | Value::String(_) if item_type == "function_call" => {
            let arguments = decode_top_level_argument_object(input)?;
            (
                arguments.get("patch")?.as_str()?.to_owned(),
                "codex_apply_patch_args_v1",
            )
        }
        _ => return None,
    };
    patch_tool_evidence(call_id, None, schema, &patch)?
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

    let (
        command,
        command_too_large,
        declared_workdir,
        continuation_cell_id,
        schema,
        command_sha256,
    ) = if tool_name == "exec_command" {
        let raw_command = arguments.get("cmd")?.as_str()?;
        let (command, command_too_large) = bounded_command(raw_command)?;
        let declared_workdir = match arguments.get("workdir") {
            Some(value) => Some(bounded_literal(value.as_str()?, MAX_WORKDIR_BYTES, |_| {
                true
            })?),
            None => None,
        };
        let command_sha256 = digest_hex(CODEX_COMMAND_DOMAIN, raw_command.trim().as_bytes());
        (
            command,
            command_too_large,
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
        (None, false, None, Some(cell_id), "codex_wait_args_v1", None)
    };

    Some(CodexRepositoryToolEvidence {
        tool_name: tool_name.clone(),
        command,
        command_too_large,
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
                "command_evidence": if command_too_large { "too_large" } else { "exact" },
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
                let raw_command = arguments.get("cmd")?.as_str()?;
                let (command, command_too_large) = bounded_command(raw_command)?;
                let declared_workdir = match arguments.get("workdir") {
                    Some(value) => {
                        let value = bounded_literal(value.as_str()?, MAX_WORKDIR_BYTES, |_| true)?;
                        Some(bounded_absolute_path(&value).then_some(value)?)
                    }
                    None => None,
                };
                let command_sha256 =
                    digest_hex(CODEX_COMMAND_DOMAIN, raw_command.trim().as_bytes());
                evidence.push(CodexRepositoryToolEvidence {
                    tool_name: "exec_command".to_owned(),
                    command,
                    command_too_large,
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
                            "command_evidence": if command_too_large { "too_large" } else { "exact" },
                            "raw_arguments_retained": false,
                        }
                    }),
                });
            }
            StaticNestedToolCall::ApplyPatch(patch) => {
                if let Some(patch_evidence) = patch_tool_evidence(
                    call_id.clone(),
                    Some(index),
                    "codex_nested_apply_patch_literal_v3",
                    &patch,
                )? {
                    evidence.push(patch_evidence);
                }
            }
        }
    }
    Some(evidence)
}

fn patch_tool_evidence(
    call_id: String,
    nested_activity_index: Option<usize>,
    schema: &'static str,
    patch: &str,
) -> Option<Option<CodexRepositoryToolEvidence>> {
    if patch.len() > MAX_STRUCTURED_ARGUMENT_BYTES {
        return None;
    }
    let file_observations = static_patch_file_observations(patch)?;
    if file_observations.is_empty() {
        return Some(None);
    }
    let static_patch_paths = file_observations.len();
    Some(Some(CodexRepositoryToolEvidence {
        tool_name: "apply_patch".to_owned(),
        command: None,
        command_too_large: false,
        declared_workdir: None,
        continuation_cell_id: None,
        file_observations,
        structured_content: json!({
            "provider_native_tool": {
                "provider": "codex",
                "name": "apply_patch",
                "outer_name": nested_activity_index.map(|_| "exec"),
                "call_id": call_id,
                "nested_activity_index": nested_activity_index,
                "argument_schema": schema,
                "static_patch_paths": static_patch_paths,
                "raw_arguments_retained": false,
            }
        }),
    }))
}

fn static_patch_file_observations(patch: &str) -> Option<Vec<UnscopedFileObservation>> {
    let mut lines = patch.lines();
    if lines.next()?.trim_end() != "*** Begin Patch" {
        return None;
    }
    let mut observations = Vec::new();
    let mut pending_update = None;
    let mut ended = false;
    for line in lines {
        if ended {
            if !line.trim().is_empty() {
                return None;
            }
            continue;
        }
        if line.trim_end() == "*** End Patch" {
            push_pending_patch_update(&mut observations, &mut pending_update)?;
            ended = true;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            push_pending_patch_update(&mut observations, &mut pending_update)?;
            push_patch_observation(
                &mut observations,
                path,
                None,
                RepositoryFileObservationKind::Created,
            )?;
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            push_pending_patch_update(&mut observations, &mut pending_update)?;
            pending_update = Some(bounded_patch_path(path)?);
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            push_pending_patch_update(&mut observations, &mut pending_update)?;
            push_patch_observation(
                &mut observations,
                path,
                None,
                RepositoryFileObservationKind::Deleted,
            )?;
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            let prior_path = pending_update.take()?;
            push_patch_observation(
                &mut observations,
                path,
                Some(prior_path),
                RepositoryFileObservationKind::Renamed,
            )?;
        }
    }
    ended.then_some(observations)
}

fn push_pending_patch_update(
    observations: &mut Vec<UnscopedFileObservation>,
    pending_update: &mut Option<String>,
) -> Option<()> {
    if let Some(path) = pending_update.take() {
        push_patch_observation(
            observations,
            &path,
            None,
            RepositoryFileObservationKind::Modified,
        )?;
    }
    Some(())
}

fn push_patch_observation(
    observations: &mut Vec<UnscopedFileObservation>,
    path: &str,
    prior_path: Option<String>,
    kind: RepositoryFileObservationKind,
) -> Option<()> {
    let path = bounded_patch_path(path)?;
    if let Some(existing) = observations
        .iter()
        .find(|observation| observation.path == path)
    {
        return (existing.prior_path == prior_path && existing.kind == kind).then_some(());
    }
    if observations.len() >= MAX_STATIC_PATCH_PATHS {
        return None;
    }
    observations.push(UnscopedFileObservation {
        path,
        prior_path,
        kind,
    });
    Some(())
}

fn bounded_patch_path(path: &str) -> Option<String> {
    let path = path.trim();
    bounded_path(path).then(|| path.to_owned())
}

enum StaticNestedToolCall {
    ExecCommand(Map<String, Value>),
    ApplyPatch(String),
}

struct StaticJsParser<'a> {
    source: &'a [u8],
    cursor: usize,
    static_strings: HashMap<String, String>,
}

impl<'a> StaticJsParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            cursor: 0,
            static_strings: HashMap::new(),
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
                if let Some((name, value)) = self.parse_static_string_declaration() {
                    if self.static_strings.len() >= MAX_STATIC_BINDINGS
                        || self.static_strings.insert(name, value).is_some()
                    {
                        return None;
                    }
                    continue;
                }
                if let Some(call) = self.parse_tool_statement() {
                    calls.push(call);
                    if calls.len() > MAX_STATIC_NESTED_TOOL_CALLS {
                        return None;
                    }
                    continue;
                }
                if let Some(call) = self.parse_wrapped_tool_statement() {
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

    fn parse_static_string_declaration(&mut self) -> Option<(String, String)> {
        let checkpoint = self.cursor;
        let parsed = self.parse_static_string_declaration_inner();
        if parsed.is_none() {
            self.cursor = checkpoint;
        }
        parsed
    }

    fn parse_static_string_declaration_inner(&mut self) -> Option<(String, String)> {
        self.consume_keyword("const").then_some(())?;
        self.skip_whitespace();
        let name = self.parse_identifier()?;
        self.skip_whitespace();
        self.consume_byte(b'=').then_some(())?;
        self.skip_whitespace();
        let value = self.parse_json_string()?;
        self.consume_statement_terminator().then_some(())?;
        Some((name, value))
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
        let argument_ok = if self.source.get(self.cursor) == Some(&b'`') {
            self.parse_output_template()
        } else if self.source.get(self.cursor) == Some(&b'"') {
            self.parse_json_string().is_some()
        } else {
            self.parse_member_reference()
        };
        if !argument_ok {
            self.cursor = checkpoint;
            return false;
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

    fn parse_output_template(&mut self) -> bool {
        if !self.consume_byte(b'`') {
            return false;
        }
        while let Some(byte) = self.source.get(self.cursor).copied() {
            match byte {
                b'`' => {
                    self.cursor += 1;
                    return true;
                }
                b'\\' => {
                    self.cursor += 1;
                    if self.source.get(self.cursor).is_none() {
                        return false;
                    }
                    self.cursor += 1;
                }
                b'$' if self
                    .source
                    .get(self.cursor.saturating_add(1))
                    .is_some_and(|next| *next == b'{') =>
                {
                    self.cursor += 2;
                    self.skip_whitespace();
                    if !self.parse_member_reference() {
                        return false;
                    }
                    self.skip_whitespace();
                    if !self.consume_byte(b'}') {
                        return false;
                    }
                }
                _ => self.cursor += 1,
            }
        }
        false
    }

    fn parse_member_reference(&mut self) -> bool {
        if self.parse_identifier().is_none() {
            return false;
        }
        loop {
            if !self.consume_byte(b'.') {
                return true;
            }
            if self.parse_identifier().is_none() {
                return false;
            }
        }
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
        let parsed = self.parse_tool_statement_inner();
        if parsed.is_none() {
            self.cursor = checkpoint;
        }
        parsed
    }

    fn parse_tool_statement_inner(&mut self) -> Option<StaticNestedToolCall> {
        if self.consume_keyword("const") {
            self.skip_whitespace();
            self.parse_identifier()?;
            self.skip_whitespace();
            self.consume_byte(b'=').then_some(())?;
            self.skip_whitespace();
        }
        let call = self.parse_tool_invocation()?;
        self.consume_statement_terminator().then_some(())?;
        Some(call)
    }

    fn parse_wrapped_tool_statement(&mut self) -> Option<StaticNestedToolCall> {
        let checkpoint = self.cursor;
        let parsed = self.parse_wrapped_tool_statement_inner();
        if parsed.is_none() {
            self.cursor = checkpoint;
        }
        parsed
    }

    fn parse_wrapped_tool_statement_inner(&mut self) -> Option<StaticNestedToolCall> {
        self.consume_keyword("text").then_some(())?;
        self.skip_whitespace();
        self.consume_byte(b'(').then_some(())?;
        self.skip_whitespace();
        let call = self.parse_tool_invocation()?;
        self.skip_whitespace();
        self.consume_byte(b')').then_some(())?;
        self.consume_statement_terminator().then_some(())?;
        Some(call)
    }

    fn parse_tool_invocation(&mut self) -> Option<StaticNestedToolCall> {
        self.consume_keyword("await").then_some(())?;
        self.skip_whitespace();
        self.consume_bytes(b"tools.").then_some(())?;
        let method = self.parse_identifier()?;
        self.skip_whitespace();
        self.consume_byte(b'(').then_some(())?;
        self.skip_whitespace();
        let value = if method == "apply_patch"
            && self
                .source
                .get(self.cursor)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
        {
            let binding = self.parse_identifier()?;
            Value::String(self.static_strings.get(&binding)?.clone())
        } else {
            self.parse_static_value(0)?
        };
        self.skip_whitespace();
        self.consume_byte(b')').then_some(())?;
        match (method.as_str(), value) {
            ("exec_command", Value::Object(arguments)) => {
                Some(StaticNestedToolCall::ExecCommand(arguments))
            }
            ("apply_patch", Value::String(patch)) => Some(StaticNestedToolCall::ApplyPatch(patch)),
            _ => None,
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

fn bounded_command(value: &str) -> Option<(Option<String>, bool)> {
    let value = value.trim();
    if value.is_empty() || value.contains('\0') {
        return None;
    }
    if value.len() > MAX_COMMAND_BYTES {
        return Some((None, true));
    }
    Some((Some(value.to_owned()), false))
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

fn hex_digest(value: &[u8; 32]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{RepositoryAbstentionReason, RepositoryFileObservationKind};
    use serde_json::json;

    use super::repository_tool_evidence;
    use crate::provider::codex::events::CodexToolCallContext;
    use crate::repository_attribution::{attribute, AttributionInput, CommandEvidenceDisposition};
    use crate::{OutputOutcome, OutputOutcomeMetadata};

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
    fn oversized_native_command_retains_typed_abstention_and_blocks_cwd_fallback() {
        let temp = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("/usr/bin/git")
            .args(["init", "-q"])
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success());
        let oversized = "x".repeat(super::MAX_COMMAND_BYTES + 1);
        let payload = json!({
            "type": "function_call",
            "name": "exec_command",
            "call_id": "oversized-command",
            "arguments": json!({"cmd": oversized}).to_string(),
        });
        let evidence = repository_tool_evidence(&payload).remove(0);
        assert!(evidence.command.is_none());
        assert!(evidence.command_too_large);

        let annotation = attribute(AttributionInput {
            session_cwd: Some(temp.path().to_string_lossy().into_owned()),
            command: evidence.command,
            command_disposition: CommandEvidenceDisposition::CommandTooLarge,
            ..AttributionInput::default()
        });
        assert!(annotation.repository_bindings.is_empty());
        assert!(annotation.repository_abstentions.iter().any(|abstention| {
            abstention.reason == RepositoryAbstentionReason::CommandTooLarge
        }));
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
    fn genuine_terminal_template_preserves_declared_workdir_for_call_and_linked_result() {
        let temp = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("/usr/bin/git")
            .args(["init", "-q"])
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success());
        let workdir = temp.path().to_string_lossy();
        let payload = json!({
            "type": "custom_tool_call",
            "name": "exec",
            "call_id": "outer-template",
            "input": format!(
                "const r = await tools.exec_command({{\"cmd\":\"git diff --check && git diff && cargo test\",\"workdir\":{workdir:?},\"yield_time_ms\":30000}});\ntext(r.output);\ntext(`exit=${{r.exit_code}}`);\n"
            ),
        });
        let mut evidence = repository_tool_evidence(&payload);
        assert_eq!(evidence.len(), 1);
        let evidence = evidence.remove(0);
        assert_eq!(evidence.tool_name, "exec_command");
        assert_eq!(evidence.declared_workdir.as_deref(), Some(workdir.as_ref()));
        assert_eq!(
            evidence.command.as_deref(),
            Some("git diff --check && git diff && cargo test")
        );

        let call_annotation = attribute(AttributionInput {
            declared_tool_workdir: evidence.declared_workdir.clone(),
            command: evidence.command.clone(),
            ..AttributionInput::default()
        });
        assert_eq!(call_annotation.repository_bindings.len(), 1);

        let context = CodexToolCallContext {
            tool_name: evidence.tool_name,
            exact_command: evidence.command,
            declared_workdir: evidence.declared_workdir,
            origin_call_id: Some("outer-template".to_owned()),
            origin_event_sequence: Some(39),
            ..CodexToolCallContext::default()
        };
        let result = super::repository_result_evidence(
            &json!({"output": "Script completed\nWall time 0.1 seconds\nOutput:\n"}),
            &context,
            "outer-template",
            [7; 32],
            10,
            &OutputOutcomeMetadata {
                outcome: OutputOutcome::Success,
                exit_code: Some(0),
                duration_ms: Some(100),
            },
        )
        .unwrap();
        assert_eq!(result.declared_workdir.as_deref(), Some(workdir.as_ref()));
        let result_annotation = attribute(AttributionInput {
            declared_tool_workdir: result.declared_workdir,
            command: result.command,
            outcome_operation_repository_path: result.outcome_operation_repository_path,
            outcome_output_repository_path: result.outcome_output_repository_path,
            outcome_observations: result.outcomes,
            outcome_abstentions: result.abstentions,
            ..AttributionInput::default()
        });
        assert_eq!(result_annotation.repository_bindings.len(), 1);
        assert_eq!(
            result_annotation.repository_bindings[0].binding_id,
            call_annotation.repository_bindings[0].binding_id
        );
    }

    #[test]
    fn genuine_bound_patch_wrapper_emits_exact_file_observations() {
        let payload = json!({
            "type": "custom_tool_call",
            "name": "exec",
            "call_id": "outer-patch",
            "input": r#"
                const patch = "*** Begin Patch\n*** Add File: /repo/src/new.rs\n*** Update File: /repo/src/lib.rs\n*** Move to: /repo/src/moved.rs\n*** Delete File: /repo/src/old.rs\n*** End Patch";
                text(await tools.apply_patch(patch));
            "#,
        });
        let evidence = repository_tool_evidence(&payload);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].tool_name, "apply_patch");
        assert_eq!(evidence[0].file_observations.len(), 3);
        assert_eq!(evidence[0].file_observations[0].path, "/repo/src/new.rs");
        assert_eq!(
            evidence[0].file_observations[0].kind,
            RepositoryFileObservationKind::Created
        );
        assert_eq!(evidence[0].file_observations[1].path, "/repo/src/moved.rs");
        assert_eq!(
            evidence[0].file_observations[1].prior_path.as_deref(),
            Some("/repo/src/lib.rs")
        );
        assert_eq!(
            evidence[0].file_observations[1].kind,
            RepositoryFileObservationKind::Renamed
        );
        assert_eq!(evidence[0].file_observations[2].path, "/repo/src/old.rs");
        assert_eq!(
            evidence[0].file_observations[2].kind,
            RepositoryFileObservationKind::Deleted
        );
        assert_eq!(
            evidence[0].structured_content["provider_native_tool"]["argument_schema"],
            "codex_nested_apply_patch_literal_v3"
        );
    }

    #[test]
    fn direct_native_patch_input_uses_the_same_bounded_parser() {
        let payload = json!({
            "type": "custom_tool_call",
            "name": "apply_patch",
            "call_id": "direct-patch",
            "input": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch\n",
        });
        let evidence = repository_tool_evidence(&payload);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].tool_name, "apply_patch");
        assert_eq!(evidence[0].file_observations.len(), 1);
        assert_eq!(evidence[0].file_observations[0].path, "src/lib.rs");
        assert_eq!(
            evidence[0].file_observations[0].kind,
            RepositoryFileObservationKind::Modified
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
            r#"const patch = choosePatch(); text(await tools.apply_patch(patch));"#,
            r#"const patch = "*** Begin Patch\n*** Add File: /repo/dynamic.rs\n*** End Patch";
                text(await tools.apply_patch(transform(patch)));"#,
            r#"const result = await tools.exec_command({cmd:"git status",workdir:"/repo"});
                text(`${sideEffect()}`);"#,
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
