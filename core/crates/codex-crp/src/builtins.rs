use crate::app_server::ModelInfo;
use crate::protocol::{CrpCommandInfo, CrpModelInfo, CrpSessionConfig};
use anyhow::{anyhow, Result};
use directories::BaseDirs;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const OPENROUTER_STREAM_IDLE_TIMEOUT_MS: i64 = 120_000;
const OPENROUTER_REQUEST_MAX_RETRIES: i64 = 4;
const OPENROUTER_STREAM_MAX_RETRIES: i64 = 10;

#[path = "builtins/commands.rs"]
mod commands;
#[path = "builtins/prompts.rs"]
mod prompts;

pub fn build_session_command_infos(codex_home: &Path) -> Vec<CrpCommandInfo> {
    let mut commands = commands::build_builtin_command_infos();
    let mut exclude = commands
        .iter()
        .map(|command| command.name.clone())
        .collect::<HashSet<_>>();
    let prompts_dir = codex_home.join("prompts");
    commands.extend(prompts::build_prompt_command_infos(
        &prompts_dir,
        &mut exclude,
    ));
    commands
}

pub fn command_names(commands: &[CrpCommandInfo]) -> Vec<String> {
    commands
        .iter()
        .map(|command| command.name.clone())
        .collect()
}

pub fn resolve_codex_home() -> PathBuf {
    if let Some(home) = std::env::var("CODEX_HOME")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(home);
    }
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".codex"))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

pub fn split_model_and_effort(model: &str) -> (String, Option<String>) {
    let Some((base, effort)) = model.rsplit_once('/') else {
        return (model.to_string(), None);
    };
    let normalized = normalize_effort_id(effort);
    if normalized.is_some() && !base.trim().is_empty() {
        (base.trim().to_string(), normalized)
    } else {
        (model.to_string(), None)
    }
}

pub fn normalize_ctx_system_prompt_append(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn build_app_server_config_overrides(config: &CrpSessionConfig) -> Option<Value> {
    let mut out = Map::new();
    if let Some(enabled) = config.reasoning_trace_enabled {
        out.insert("show_raw_agent_reasoning".to_string(), Value::Bool(enabled));
    }
    if let Some(base_url) = config.openai_base_url.as_ref() {
        out.insert(
            "openai_base_url".to_string(),
            Value::String(base_url.clone()),
        );
        if let Some(model_provider) = endpoint_model_provider(config.model_provider.as_deref()) {
            out.insert(
                format!("model_providers.{model_provider}.name"),
                Value::String(model_provider.to_string()),
            );
            out.insert(
                format!("model_providers.{model_provider}.base_url"),
                Value::String(base_url.clone()),
            );
            out.insert(
                format!("model_providers.{model_provider}.env_key"),
                Value::String("OPENAI_API_KEY".to_string()),
            );
            out.insert(
                format!("model_providers.{model_provider}.wire_api"),
                Value::String("responses".to_string()),
            );
            if model_provider == "openrouter" {
                out.insert(
                    "stream_idle_timeout_ms".to_string(),
                    Value::from(OPENROUTER_STREAM_IDLE_TIMEOUT_MS),
                );
                out.insert(
                    "model_providers.openrouter.request_max_retries".to_string(),
                    Value::from(OPENROUTER_REQUEST_MAX_RETRIES),
                );
                out.insert(
                    "model_providers.openrouter.stream_max_retries".to_string(),
                    Value::from(OPENROUTER_STREAM_MAX_RETRIES),
                );
            }
        }
    }
    if let Some(mcp_servers) = &config.mcp_servers {
        for (name, server) in mcp_servers {
            if let Some(value) = mcp_server_to_value(server.clone()) {
                out.insert(format!("mcp_servers.{name}"), value);
            }
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

pub fn build_app_server_launch_config_overrides(config: &CrpSessionConfig) -> Vec<String> {
    if endpoint_model_provider(config.model_provider.as_deref()) == Some("openrouter") {
        return vec![format!(
            "stream_idle_timeout_ms={OPENROUTER_STREAM_IDLE_TIMEOUT_MS}"
        )];
    }
    Vec::new()
}

fn endpoint_model_provider(model_provider: Option<&str>) -> Option<&str> {
    let provider = model_provider
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if provider == "openai" {
        None
    } else {
        Some(provider)
    }
}

pub fn parse_cli_config_overrides(raw_entries: &[String]) -> Result<Option<Value>> {
    let mut out = Map::new();
    for raw_entry in raw_entries {
        let trimmed = raw_entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((key, raw_value)) = trimmed.split_once('=') else {
            return Err(anyhow!(
                "invalid -c/--config override `{trimmed}`; expected key=value"
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(anyhow!(
                "invalid -c/--config override `{trimmed}`; key must not be empty"
            ));
        }
        out.insert(key.to_string(), parse_override_value(raw_value.trim()));
    }
    Ok((!out.is_empty()).then_some(Value::Object(out)))
}

pub fn merge_config_overrides(base: Option<Value>, overlay: Option<Value>) -> Option<Value> {
    match (base, overlay) {
        (None, None) => None,
        (Some(value), None) | (None, Some(value)) => Some(value),
        (Some(Value::Object(mut base)), Some(Value::Object(overlay))) => {
            for (key, value) in overlay {
                base.insert(key, value);
            }
            Some(Value::Object(base))
        }
        (_, Some(overlay)) => Some(overlay),
    }
}

pub fn build_model_infos(models: &[ModelInfo]) -> Vec<CrpModelInfo> {
    let mut out = Vec::new();
    for model in models {
        if model.hidden {
            continue;
        }
        if model.supported_reasoning_efforts.len() >= 2 {
            let mut seen = HashSet::new();
            for effort in &model.supported_reasoning_efforts {
                if !seen.insert(effort.reasoning_effort.clone()) {
                    continue;
                }
                let id = format!("{}/{}", model.id, effort.reasoning_effort);
                let name = format!("{} ({})", model.display_name, effort.reasoning_effort);
                out.push(CrpModelInfo {
                    id,
                    name: Some(name),
                });
            }
        } else {
            out.push(CrpModelInfo {
                id: model.id.clone(),
                name: Some(model.display_name.clone()),
            });
        }
    }
    out
}

pub fn build_current_model_id(
    config: Option<&CrpSessionConfig>,
    models: &[ModelInfo],
    current_model: Option<&str>,
    current_effort: Option<&str>,
) -> Option<String> {
    if let Some(model) = current_model {
        let model = model.trim();
        if model.is_empty() {
            return None;
        }
        if let Some(effort) = current_effort.filter(|value| !value.trim().is_empty()) {
            return Some(format!("{model}/{}", effort.trim()));
        }
        return Some(model.to_string());
    }

    if let Some(config) = config {
        if let Some(model) = config.model.as_deref() {
            let (model, effort) = split_model_and_effort(model);
            if let Some(effort) =
                effort.or_else(|| normalize_optional_effort(config.reasoning_effort.as_deref()))
            {
                return Some(format!("{model}/{effort}"));
            }
            return Some(model);
        }
    }

    let model = models.iter().find(|candidate| candidate.is_default)?;
    if model.supported_reasoning_efforts.len() >= 2 {
        return Some(format!("{}/{}", model.id, model.default_reasoning_effort));
    }
    Some(model.id.clone())
}

fn mcp_server_to_value(config: crate::protocol::CrpMcpServerConfig) -> Option<Value> {
    let mut out = Map::new();
    if let Some(timeout) = config.tool_timeout_sec {
        out.insert("tool_timeout_sec".to_string(), Value::from(timeout));
    }
    if let Some(enabled_tools) = config.enabled_tools {
        out.insert(
            "enabled_tools".to_string(),
            Value::Array(enabled_tools.into_iter().map(Value::String).collect()),
        );
    }
    if let Some(disabled_tools) = config.disabled_tools {
        out.insert(
            "disabled_tools".to_string(),
            Value::Array(disabled_tools.into_iter().map(Value::String).collect()),
        );
    }
    if let Some(command) = config.command {
        out.insert("command".to_string(), Value::String(command));
        if let Some(args) = config.args.filter(|args| !args.is_empty()) {
            out.insert(
                "args".to_string(),
                Value::Array(args.into_iter().map(Value::String).collect()),
            );
        }
        if let Some(env) = config.env.filter(|env| !env.is_empty()) {
            out.insert("env".to_string(), map_to_value(env));
        }
        if let Some(env_vars) = config.env_vars.filter(|vars| !vars.is_empty()) {
            out.insert(
                "env_vars".to_string(),
                Value::Array(env_vars.into_iter().map(Value::String).collect()),
            );
        }
        if let Some(cwd) = config.cwd {
            out.insert(
                "cwd".to_string(),
                Value::String(cwd.to_string_lossy().to_string()),
            );
        }
        return Some(Value::Object(out));
    }
    if let Some(url) = config.url {
        out.insert("url".to_string(), Value::String(url));
        if let Some(headers) = config.http_headers.filter(|headers| !headers.is_empty()) {
            out.insert("http_headers".to_string(), map_to_value(headers));
        }
        if let Some(headers) = config
            .env_http_headers
            .filter(|headers| !headers.is_empty())
        {
            out.insert("env_http_headers".to_string(), map_to_value(headers));
        }
        return Some(Value::Object(out));
    }
    None
}

fn map_to_value(input: HashMap<String, String>) -> Value {
    let mut out = Map::new();
    for (key, value) in input {
        out.insert(key, Value::String(value));
    }
    Value::Object(out)
}

fn normalize_effort_id(raw: &str) -> Option<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" => {
            Some(raw.trim().to_ascii_lowercase())
        }
        _ => None,
    }
}

fn normalize_optional_effort(raw: Option<&str>) -> Option<String> {
    raw.and_then(normalize_effort_id)
}

fn parse_override_value(raw: &str) -> Value {
    if raw.is_empty() {
        return Value::String(String::new());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn split_model_and_effort_supports_reasoning_suffixes() {
        assert_eq!(
            split_model_and_effort("gpt-5.4/xhigh"),
            ("gpt-5.4".to_string(), Some("xhigh".to_string()))
        );
        assert_eq!(
            split_model_and_effort("gpt-5.4"),
            ("gpt-5.4".to_string(), None)
        );
    }

    #[test]
    fn parse_cli_config_overrides_parses_json_literals() {
        let overrides = parse_cli_config_overrides(&[
            "show_raw_agent_reasoning=true".to_string(),
            "model_reasoning_summary=\"detailed\"".to_string(),
            "max_output_tokens=4096".to_string(),
        ])
        .expect("config overrides should parse");
        assert_eq!(
            overrides,
            Some(serde_json::json!({
                "show_raw_agent_reasoning": true,
                "model_reasoning_summary": "detailed",
                "max_output_tokens": 4096
            }))
        );
    }

    #[test]
    fn build_current_model_id_uses_explicit_reasoning_effort() {
        let config = CrpSessionConfig {
            model: Some("gpt-5.4".to_string()),
            reasoning_effort: Some("high".to_string()),
            ..CrpSessionConfig::default()
        };
        assert_eq!(
            build_current_model_id(Some(&config), &[], None, None),
            Some("gpt-5.4/high".to_string())
        );
    }

    #[test]
    fn build_app_server_config_overrides_includes_openai_base_url() {
        let config = CrpSessionConfig {
            openai_base_url: Some("https://openrouter.ai/api/v1".to_string()),
            ..CrpSessionConfig::default()
        };

        assert_eq!(
            build_app_server_config_overrides(&config),
            Some(serde_json::json!({
                "openai_base_url": "https://openrouter.ai/api/v1"
            }))
        );
    }

    #[test]
    fn build_app_server_config_overrides_defines_endpoint_model_provider() {
        let config = CrpSessionConfig {
            model_provider: Some("openrouter".to_string()),
            openai_base_url: Some("https://openrouter.ai/api/v1".to_string()),
            ..CrpSessionConfig::default()
        };

        assert_eq!(
            build_app_server_config_overrides(&config),
            Some(serde_json::json!({
                "openai_base_url": "https://openrouter.ai/api/v1",
                "model_providers.openrouter.name": "openrouter",
                "model_providers.openrouter.base_url": "https://openrouter.ai/api/v1",
                "model_providers.openrouter.env_key": "OPENAI_API_KEY",
                "model_providers.openrouter.wire_api": "responses",
                "model_providers.openrouter.request_max_retries": 4,
                "model_providers.openrouter.stream_max_retries": 10,
                "stream_idle_timeout_ms": 120000
            }))
        );
    }

    #[test]
    fn build_app_server_launch_config_overrides_sets_openrouter_startup_timeout() {
        let config = CrpSessionConfig {
            model_provider: Some("openrouter".to_string()),
            openai_base_url: Some("https://openrouter.ai/api/v1".to_string()),
            ..CrpSessionConfig::default()
        };

        assert_eq!(
            build_app_server_launch_config_overrides(&config),
            vec!["stream_idle_timeout_ms=120000"]
        );
        assert!(
            build_app_server_launch_config_overrides(&config)
                .iter()
                .all(|entry| !entry.starts_with("model_providers.")),
            "launch config must not create partial provider tables before session.opened"
        );
    }

    #[test]
    fn build_app_server_launch_config_overrides_ignores_openai_provider() {
        let config = CrpSessionConfig {
            model_provider: Some("openai".to_string()),
            openai_base_url: Some("https://api.openai.com/v1".to_string()),
            ..CrpSessionConfig::default()
        };

        assert!(build_app_server_launch_config_overrides(&config).is_empty());
    }

    #[test]
    fn build_app_server_config_overrides_does_not_redefine_openai_provider() {
        let config = CrpSessionConfig {
            model_provider: Some("openai".to_string()),
            openai_base_url: Some("https://api.openai.com/v1".to_string()),
            ..CrpSessionConfig::default()
        };

        assert_eq!(
            build_app_server_config_overrides(&config),
            Some(serde_json::json!({
                "openai_base_url": "https://api.openai.com/v1"
            }))
        );
    }

    #[test]
    fn parse_frontmatter_extracts_description_and_argument_hint() {
        let (description, argument_hint, body) = prompts::parse_frontmatter(
            "---\ndescription: \"Review\"\nargument_hint: \"[path]\"\n---\nBody\n",
        );
        assert_eq!(description.as_deref(), Some("Review"));
        assert_eq!(argument_hint.as_deref(), Some("[path]"));
        assert_eq!(body, "Body\n");
    }

    #[test]
    fn discover_prompts_reads_markdown_files() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("alpha.md"), "hello").expect("write prompt");
        let prompts = prompts::discover_prompts_in_excluding(dir.path(), &HashSet::new());
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "alpha");
    }
}
