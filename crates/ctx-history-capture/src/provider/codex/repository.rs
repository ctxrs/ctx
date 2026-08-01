use std::{collections::HashSet, path::Path};

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
mod static_js;

pub(crate) use outcomes::CodexRepositoryResultEvidence;
use static_js::{StaticJsParser, StaticNestedToolCall};

const MAX_STRUCTURED_ARGUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_WORKDIR_BYTES: usize = 16 * 1024;
const MAX_CALL_ID_BYTES: usize = 1024;
const MAX_CONTINUATION_CELL_ID_BYTES: usize = 1024;
const MAX_STATIC_PATCH_PATHS: usize = 256;
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
mod tests;
