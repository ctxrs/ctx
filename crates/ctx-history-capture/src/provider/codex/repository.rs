use std::{collections::HashMap, path::Path, sync::OnceLock};

use ctx_history_core::RepositoryFileObservationKind;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::events::codex_tool_name;
use crate::repository_attribution::UnscopedFileObservation;

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
    let literals = json_string_regex()
        .find_iter(source)
        .filter_map(|matched| serde_json::from_str::<String>(matched.as_str()).ok())
        .collect::<Vec<_>>();
    let commands = named_literal_regex("cmd")
        .captures_iter(source)
        .filter_map(|captures| captures.get(1))
        .filter_map(|matched| serde_json::from_str::<String>(matched.as_str()).ok())
        .collect::<Vec<_>>();
    let mut tuple_commands = HashMap::new();
    for captures in tuple_regex().captures_iter(source) {
        let Some(workdir) = captures
            .get(2)
            .and_then(|matched| serde_json::from_str::<String>(matched.as_str()).ok())
        else {
            continue;
        };
        let Some(command) = captures
            .get(3)
            .and_then(|matched| serde_json::from_str::<String>(matched.as_str()).ok())
        else {
            continue;
        };
        if bounded_absolute_path(&workdir) {
            tuple_commands.insert(workdir, command);
        }
    }
    let mut workdirs = named_literal_regex("(?:workdir|dir)")
        .captures_iter(source)
        .filter_map(|captures| captures.get(1))
        .filter_map(|matched| serde_json::from_str::<String>(matched.as_str()).ok())
        .filter(|value| bounded_absolute_path(value))
        .collect::<Vec<_>>();
    // Codex's parallel orchestration commonly places an absolute workdir in a
    // static tuple and later passes it through a shorthand `workdir` property.
    // Decode only complete JSON string literals that are themselves absolute;
    // command text is never searched for embedded paths.
    workdirs.extend(
        literals
            .iter()
            .filter(|value| bounded_absolute_path(value))
            .cloned(),
    );
    deduplicate(&mut workdirs);

    let mut evidence = Vec::new();
    if source.contains("tools.exec_command") {
        for (index, workdir) in workdirs.iter().enumerate() {
            let (command, resolution) = if let Some(command) = tuple_commands.get(workdir) {
                (Some(command.clone()), "static_tuple_literal")
            } else if commands.len() == 1 {
                (commands.first().cloned(), "shared_static_literal")
            } else if commands.len() == workdirs.len() {
                (commands.get(index).cloned(), "positional_static_literal")
            } else {
                (None, "unresolved_static_variable")
            };
            let command =
                command.and_then(|command| bounded_literal(&command, MAX_COMMAND_BYTES, |_| true));
            let command_sha256 = command
                .as_deref()
                .map(|command| digest_hex(CODEX_COMMAND_DOMAIN, command.as_bytes()));
            evidence.push(CodexRepositoryToolEvidence {
                tool_name: "exec_command".to_owned(),
                command,
                declared_workdir: Some(workdir.clone()),
                continuation_cell_id: None,
                file_observations: Vec::new(),
                structured_content: json!({
                    "provider_native_tool": {
                        "provider": "codex",
                        "name": "exec_command",
                        "outer_name": "exec",
                        "call_id": call_id,
                        "nested_activity_index": index,
                        "argument_schema": "codex_nested_exec_command_static_v1",
                        "declared_workdir": workdir,
                        "command_resolution": resolution,
                        "command_sha256": command_sha256,
                        "raw_arguments_retained": false,
                    }
                }),
            });
        }
    }

    let mut patch_paths = Vec::new();
    if source.contains("tools.apply_patch") {
        for literal in &literals {
            for line in literal.lines() {
                let line = line.trim();
                let observation = [
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
                });
                if let Some(observation) = observation {
                    patch_paths.push(observation);
                }
            }
        }
    }
    let mut seen_patch_paths = std::collections::HashSet::new();
    patch_paths.retain(|(path, _)| seen_patch_paths.insert(path.clone()));
    if !patch_paths.is_empty() {
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
                    "argument_schema": "codex_nested_apply_patch_static_headers_v1",
                    "static_patch_paths": patch_paths.len(),
                    "raw_arguments_retained": false,
                }
            }),
        });
    }
    Some(evidence)
}

fn json_string_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r#"\"(?:\\.|[^\"\\])*\""#).expect("static regex"))
}

fn named_literal_regex(name: &'static str) -> &'static Regex {
    static CMD: OnceLock<Regex> = OnceLock::new();
    static WORKDIR: OnceLock<Regex> = OnceLock::new();
    let slot = if name == "cmd" { &CMD } else { &WORKDIR };
    slot.get_or_init(|| {
        Regex::new(&format!(r#"(?s)\b{name}\s*:\s*(\"(?:\\.|[^\"\\])*\")"#)).expect("static regex")
    })
}

fn tuple_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?s)\[\s*(\"(?:\\.|[^\"\\])*\")\s*,\s*(\"(?:\\.|[^\"\\])*\")\s*,\s*(\"(?:\\.|[^\"\\])*\")\s*\]"#,
        )
        .expect("static regex")
    })
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

fn deduplicate(values: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
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
    fn statically_decodes_nested_exec_workdirs_commands_and_patch_headers() {
        let payload = json!({
            "type": "custom_tool_call",
            "name": "exec",
            "call_id": "outer-1",
            "input": r#"
                const tasks = [
                  ["one", "/repo/one", "git status"],
                  ["two", "/repo/two", "git log -1"]
                ].map(async ([name, workdir, cmd]) =>
                  tools.exec_command({cmd, workdir}));
                await tools.apply_patch({prompt: "*** Begin Patch\n*** Add File: /repo/one/src/new.rs\n*** Update File: /repo/one/src/lib.rs\n*** Delete File: /repo/one/src/old.rs\n*** End Patch"});
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
