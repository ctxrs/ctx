use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::events::codex_tool_name;

mod outcomes;

pub(crate) use outcomes::{repository_result_evidence, CodexRepositoryResultEvidence};

const MAX_STRUCTURED_ARGUMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_WORKDIR_BYTES: usize = 16 * 1024;
const MAX_CALL_ID_BYTES: usize = 1024;
const MAX_CONTINUATION_CELL_ID_BYTES: usize = 1024;
const CODEX_CONTINUATION_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/continuation-call-id/v1\0";
const CODEX_COMMAND_DOMAIN: &[u8] = b"ctx/codex-nativepath/exact-command/v1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexRepositoryToolEvidence {
    pub(crate) command: Option<String>,
    pub(crate) declared_workdir: Option<String>,
    pub(crate) continuation_cell_id: Option<String>,
    pub(crate) structured_content: Value,
}

/// Reads only the measured Codex top-level argument object.
///
/// A JSON string may be decoded once because native `function_call.arguments`
/// is JSON text. JavaScript wrappers, nested objects, comments, and arbitrary
/// strings are never searched for command or workdir literals.
pub(crate) fn repository_tool_evidence(payload: &Value) -> Option<CodexRepositoryToolEvidence> {
    let item_type = payload.get("type").and_then(Value::as_str)?;
    let tool_name = codex_tool_name(payload, item_type);
    if !matches!(tool_name.as_str(), "exec_command" | "wait") {
        return None;
    }
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
        command,
        declared_workdir,
        continuation_cell_id,
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
        let evidence = repository_tool_evidence(&payload).unwrap();
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
            assert!(repository_tool_evidence(&payload).is_none());
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
