use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use ctx_core::boolish::parse_boolish;
use ctx_core::provider_ids::CODEX_PROVIDER_ID;
use ctx_core::provider_policy::{
    CTX_CRP_LAUNCH_POLICY_ENV, CTX_CRP_LAUNCH_POLICY_FULL, FULL_YOLO_APPROVAL_POLICY,
    FULL_YOLO_SANDBOX_MODE,
};
use serde_json::{json, Value};
use tokio::time::Duration;

use crate::adapters::TurnInput;
use crate::container_exec::{
    container_exec_spec, rewrite_ctx_mcp_command_for_env, translate_thread_cwd_for_container,
};

use super::protocol::{CrpMcpServerConfig, CrpModelInfo, CrpModelsProbe, CrpSessionConfig};

const DEFAULT_CTX_MCP_TOOL_TIMEOUT_SECS: u64 = 2 * 60 * 60;

pub(super) fn model_override_disabled(env: &HashMap<String, String>) -> bool {
    env.get("CTX_CRP_DISABLE_MODEL_OVERRIDE")
        .and_then(|value| parse_boolish(value))
        .unwrap_or(false)
}

pub(super) fn build_crp_session_config(
    env: &HashMap<String, String>,
    workdir: &Path,
) -> Result<CrpSessionConfig> {
    build_crp_session_config_with_mcp(env, workdir, true)
}

pub(super) fn build_crp_auth_session_config(
    env: &HashMap<String, String>,
    workdir: &Path,
) -> Result<CrpSessionConfig> {
    build_crp_session_config_with_mcp(env, workdir, false)
}

fn build_crp_session_config_with_mcp(
    env: &HashMap<String, String>,
    workdir: &Path,
    include_mcp_servers: bool,
) -> Result<CrpSessionConfig> {
    let mcp_enabled = env
        .get("CTX_MCP_DISABLED")
        .and_then(|value| parse_boolish(value))
        .map(|disabled| !disabled)
        .unwrap_or(true);

    let (model, reasoning_effort) = if model_override_disabled(env) {
        (None, None)
    } else {
        env.get("CTX_MODEL_ID")
            .map(|value| split_model_id_and_effort(value))
            .unwrap_or((None, None))
    };

    let mcp_servers = if mcp_enabled && include_mcp_servers {
        let mut mcp_env = HashMap::new();
        if let Some(url) = env.get("CTX_DAEMON_URL") {
            mcp_env.insert("CTX_DAEMON_URL".to_string(), url.clone());
        }
        if let Some(token) = env.get("CTX_MCP_TOKEN") {
            mcp_env.insert("CTX_MCP_TOKEN".to_string(), token.clone());
        }

        let mcp_command = resolve_session_mcp_command(env)?;
        let tool_timeout_sec = env
            .get("CTX_MCP_TOOL_TIMEOUT_SEC")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_CTX_MCP_TOOL_TIMEOUT_SECS);

        let mut map = HashMap::new();
        map.insert(
            "ctx".to_string(),
            CrpMcpServerConfig {
                command: Some(mcp_command),
                args: Some(vec!["--stdio".to_string()]),
                env: Some(mcp_env),
                env_vars: None,
                cwd: None,
                url: None,
                http_headers: None,
                env_http_headers: None,
                enabled_tools: None,
                disabled_tools: None,
                tool_timeout_sec: Some(tool_timeout_sec as f64),
            },
        );
        Some(map)
    } else {
        None
    };

    let container_cwd = translate_thread_cwd_for_container(env, workdir)?;
    let launch_policy = crp_launch_policy_from_env(env)?;
    Ok(CrpSessionConfig {
        cwd: Some(container_cwd.clone()),
        spawn_cwd: Some(container_cwd),
        model,
        reasoning_effort,
        approval_policy: launch_policy.approval_policy,
        sandbox_mode: launch_policy.sandbox_mode,
        model_provider: env_string(env, "CTX_MODEL_PROVIDER"),
        openai_base_url: env_string(env, "OPENAI_BASE_URL"),
        reasoning_trace_enabled: Some(true),
        personality: env
            .get("CTX_PROVIDER_ID")
            .map(|provider_id| provider_id.as_str())
            .filter(|provider_id| *provider_id == CODEX_PROVIDER_ID)
            .map(|_| "pragmatic".to_string()),
        mcp_servers,
    })
}

fn resolve_session_mcp_command(env: &HashMap<String, String>) -> Result<String> {
    let configured = env
        .get("CTX_MCP_COMMAND")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let command = configured
        .ok_or_else(|| anyhow::anyhow!("CTX_MCP_COMMAND is required when ctx MCP is enabled"))?;
    if container_exec_spec(env).is_none() {
        validate_explicit_mcp_command(command)?;
        return Ok(command.to_string());
    }
    let rewritten = rewrite_ctx_mcp_command_for_env(env, command)
        .context("rewriting CTX_MCP_COMMAND for container execution")?;
    validate_explicit_mcp_command(&rewritten)?;
    Ok(rewritten)
}

fn validate_explicit_mcp_command(command: &str) -> Result<()> {
    let path = Path::new(command);
    if !path.is_absolute() && !looks_like_windows_absolute_path(command) {
        anyhow::bail!("CTX_MCP_COMMAND must be an explicit absolute path, got `{command}`");
    }
    if !path.exists() {
        anyhow::bail!("CTX_MCP_COMMAND path does not exist: {command}");
    }
    Ok(())
}

fn looks_like_windows_absolute_path(command: &str) -> bool {
    let bytes = command.as_bytes();
    bytes.len() >= 3
        && bytes[1] == b':'
        && bytes[0].is_ascii_alphabetic()
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

pub(super) fn build_crp_model_probe_config(
    env: &HashMap<String, String>,
    workdir: &Path,
) -> Result<CrpSessionConfig> {
    let (model, reasoning_effort) = env
        .get("CTX_MODEL_ID")
        .map(|value| split_model_id_and_effort(value))
        .unwrap_or((None, None));
    let container_cwd = translate_thread_cwd_for_container(env, workdir)?;
    let launch_policy = crp_launch_policy_from_env(env)?;
    Ok(CrpSessionConfig {
        cwd: Some(container_cwd.clone()),
        spawn_cwd: Some(container_cwd),
        model,
        reasoning_effort,
        approval_policy: launch_policy.approval_policy,
        sandbox_mode: launch_policy.sandbox_mode,
        model_provider: env_string(env, "CTX_MODEL_PROVIDER"),
        openai_base_url: env_string(env, "OPENAI_BASE_URL"),
        reasoning_trace_enabled: None,
        personality: None,
        mcp_servers: None,
    })
}

fn env_string(env: &HashMap<String, String>, key: &str) -> Option<String> {
    env.get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

struct CrpLaunchPolicy {
    approval_policy: Option<String>,
    sandbox_mode: Option<String>,
}

fn crp_launch_policy_from_env(env: &HashMap<String, String>) -> Result<CrpLaunchPolicy> {
    match env_string(env, CTX_CRP_LAUNCH_POLICY_ENV).as_deref() {
        None => Ok(CrpLaunchPolicy {
            approval_policy: None,
            sandbox_mode: None,
        }),
        Some(CTX_CRP_LAUNCH_POLICY_FULL) => Ok(CrpLaunchPolicy {
            approval_policy: Some(FULL_YOLO_APPROVAL_POLICY.to_string()),
            sandbox_mode: Some(FULL_YOLO_SANDBOX_MODE.to_string()),
        }),
        Some(value) => anyhow::bail!("unsupported {CTX_CRP_LAUNCH_POLICY_ENV}: {value}"),
    }
}

pub(super) fn split_model_id_and_effort(model_id: &str) -> (Option<String>, Option<String>) {
    let trimmed = model_id.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    let Some((base, suffix)) = trimmed.rsplit_once('/') else {
        return (Some(trimmed.to_string()), None);
    };
    if base.trim().is_empty() {
        return (Some(trimmed.to_string()), None);
    }
    let Some(effort) = normalize_effort_id(suffix) else {
        return (Some(trimmed.to_string()), None);
    };
    (Some(base.trim().to_string()), Some(effort))
}

fn normalize_effort_id(raw: &str) -> Option<String> {
    let normalized = raw.trim().to_lowercase();
    match normalized.as_str() {
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" => Some(normalized),
        "extra_high" | "extra-high" | "extra high" => Some("xhigh".to_string()),
        _ => None,
    }
}

pub(super) fn synthetic_models_probe_for_provider(
    provider_id: &str,
    env: &HashMap<String, String>,
) -> Option<CrpModelsProbe> {
    if provider_id != "cline" {
        return None;
    }
    let model_id = env
        .get("OPENAI_MODEL")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    Some(CrpModelsProbe {
        models: vec![CrpModelInfo {
            id: model_id.clone(),
            name: Some(model_id.clone()),
        }],
        current_model_id: Some(model_id),
        catalog_source: None,
    })
}

pub(super) fn probe_timeout_for_env(
    env: &HashMap<String, String>,
    host_timeout: Duration,
    container_timeout: Duration,
) -> Duration {
    if container_exec_spec(env).is_some() {
        container_timeout
    } else {
        host_timeout
    }
}

// NOTE: The canonical CRP image transport is now blob-backed refs and inline bytes, not
// provider-visible files. Explicit `local_image` items remain compatibility-only for runtimes
// that deliberately opt into filesystem paths.

pub(super) async fn build_prompt_items(
    input: &TurnInput,
    _workdir: &Path,
    _env: &HashMap<String, String>,
) -> Result<Vec<Value>> {
    let mut items = Vec::new();
    for block in &input.context_blocks {
        if block
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| matches!(t, "text" | "image" | "image_ref" | "local_image" | "skill"))
        {
            items.push(block.clone());
        }
    }

    for att in input.attachments.iter() {
        match att {
            ctx_core::models::MessageAttachment::Image {
                mime_type,
                data_base64,
                name,
            } => {
                items.push(json!({
                    "type": "image",
                    "mime_type": mime_type,
                    "data": data_base64,
                    "name": name,
                }));
            }
            ctx_core::models::MessageAttachment::ImageRef {
                blob_id,
                mime_type,
                name,
            } => {
                items.push(json!({
                    "type": "image_ref",
                    "blob_id": blob_id,
                    "mime_type": mime_type,
                    "name": name,
                }));
            }
        }
    }

    items.push(json!({"type":"text","text": input.content}));
    Ok(items)
}

pub(super) fn provider_requires_flattened_text_prompt(provider_id: &str) -> bool {
    provider_id.eq_ignore_ascii_case("opencode")
}

pub(super) fn flatten_prompt_items_as_text(items: &[Value]) -> Result<String> {
    let mut out = Vec::new();
    for item in items {
        let text = match item {
            Value::String(text) => Some(text.as_str()),
            Value::Object(obj) => obj
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| obj.get("content").and_then(Value::as_str)),
            _ => None,
        };
        let Some(text) = text else {
            anyhow::bail!("provider requires text-only ACP prompt items");
        };
        if !text.is_empty() {
            out.push(text.to_string());
        }
    }
    if out.is_empty() {
        anyhow::bail!("provider requires a non-empty text ACP prompt");
    }
    Ok(out.join("\n\n"))
}

#[cfg(test)]
#[path = "config/tests.rs"]
mod tests;
