use chrono::{Duration, Utc};
use ctx_history_core::{utc_now, CaptureProvider, EventType};
use serde_json::Value;
use uuid::Uuid;

use super::invalid_tool_request;
use crate::tool_backend::{
    ToolBackendError, ToolSearchBackend, ToolSearchContentScope, ToolTranscriptMode,
};

type Result<T> = std::result::Result<T, ToolBackendError>;

#[derive(Debug, Clone, Default)]
pub(super) struct SourceIdentityFilterArgs {
    pub(super) history_source: Option<String>,
    pub(super) provider_key: Option<String>,
    pub(super) source_id: Option<String>,
    pub(super) source_format: Option<String>,
}

pub(super) fn optional_string(arguments: &Value, key: &str) -> Result<Option<String>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_tool_request(format!("{key} must be a string"))),
    }
}

pub(super) fn optional_strings(arguments: &Value, key: &str) -> Result<Vec<String>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| invalid_tool_request(format!("{key} entries must be strings")))
            })
            .collect(),
        Some(_) => Err(invalid_tool_request(format!("{key} must be an array"))),
    }
}

pub(super) fn optional_bool(arguments: &Value, key: &str) -> Result<Option<bool>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(invalid_tool_request(format!("{key} must be a boolean"))),
    }
}

pub(super) fn optional_usize(arguments: &Value, key: &str) -> Result<Option<usize>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => {
            let value = value.as_u64().ok_or_else(|| {
                invalid_tool_request(format!("{key} must be a non-negative integer"))
            })?;
            usize::try_from(value)
                .map(Some)
                .map_err(|_| invalid_tool_request(format!("{key} is too large")))
        }
        Some(_) => Err(invalid_tool_request(format!(
            "{key} must be a non-negative integer"
        ))),
    }
}

pub(super) fn optional_f32(arguments: &Value, key: &str) -> Result<Option<f32>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_f64()
            .map(|value| value as f32)
            .ok_or_else(|| invalid_tool_request(format!("{key} must be a number")))
            .map(Some),
        Some(_) => Err(invalid_tool_request(format!("{key} must be a number"))),
    }
}

pub(super) fn optional_provider(
    arguments: &Value,
    key: &str,
    parse_provider: impl FnOnce(&str) -> Option<CaptureProvider>,
    provider_names: &[&str],
) -> Result<Option<CaptureProvider>> {
    let Some(provider) = optional_string(arguments, key)? else {
        return Ok(None);
    };
    parse_provider(&provider).map(Some).ok_or_else(|| {
        invalid_tool_request(format!(
            "provider must be one of {}",
            provider_names.join(", ")
        ))
    })
}
pub(super) fn optional_search_backend(
    arguments: &Value,
    key: &str,
) -> Result<Option<ToolSearchBackend>> {
    let Some(value) = optional_string(arguments, key)? else {
        return Ok(None);
    };
    match value.as_str() {
        "hybrid" => Ok(Some(ToolSearchBackend::Hybrid)),
        "lexical" => Ok(Some(ToolSearchBackend::Lexical)),
        "semantic" => Ok(Some(ToolSearchBackend::Semantic)),
        _ => Err(invalid_tool_request(
            "backend must be one of hybrid, semantic, lexical",
        )),
    }
}

pub(super) fn optional_content_scope(
    arguments: &Value,
    key: &str,
) -> Result<Option<ToolSearchContentScope>> {
    let Some(value) = optional_string(arguments, key)? else {
        return Ok(None);
    };
    match value.as_str() {
        "all" => Ok(Some(ToolSearchContentScope::All)),
        "transcript" => Ok(Some(ToolSearchContentScope::Transcript)),
        "calls" => Ok(Some(ToolSearchContentScope::Calls)),
        "outputs" => Ok(Some(ToolSearchContentScope::Outputs)),
        _ => Err(invalid_tool_request(
            "content_scope must be one of all, transcript, calls, outputs",
        )),
    }
}

pub(super) fn validate_argument_keys(arguments: &Value, allowed: &[&str]) -> Result<()> {
    let object = arguments
        .as_object()
        .ok_or_else(|| invalid_tool_request("arguments must be an object"))?;
    if let Some(key) = object
        .keys()
        .find(|key| !allowed.iter().any(|allowed| allowed == &key.as_str()))
    {
        return Err(invalid_tool_request(format!("unknown argument {key}")));
    }
    Ok(())
}

pub(super) fn validate_search_filter_arguments(
    provider: Option<&CaptureProvider>,
    source_identity: &SourceIdentityFilterArgs,
    session: Option<&str>,
    since: Option<&str>,
    event_type: Option<&str>,
) -> Result<()> {
    let normalized = normalize_source_identity_filters(source_identity.clone())?;
    if normalized && provider.is_some_and(|provider| *provider != CaptureProvider::Custom) {
        return Err(invalid_tool_request(
            "custom history source filters can only be combined with --provider custom",
        ));
    }
    if let Some(value) = session {
        let value = value.trim();
        if Uuid::parse_str(value).is_err() {
            normalize_uuid_prefix(value, "session")?;
        }
    }
    if let Some(value) = since {
        parse_since_filter(value)?;
    }
    if let Some(value) = event_type {
        value
            .parse::<EventType>()
            .map_err(|error| invalid_tool_request(error.to_string()))?;
    };
    Ok(())
}

fn normalize_source_identity_filters(input: SourceIdentityFilterArgs) -> Result<bool> {
    let history_source = normalize_source_identity_filter("history-source", input.history_source)?;
    if history_source
        .as_deref()
        .is_some_and(|value| !value.contains('/'))
    {
        return Err(invalid_tool_request(
            "--history-source expects plugin/source or provider_key/source_id",
        ));
    }
    let provider_key = normalize_source_identity_filter("provider-key", input.provider_key)?;
    let source_id = normalize_source_identity_filter("source-id", input.source_id)?;
    let source_format = normalize_source_identity_filter("source-format", input.source_format)?;
    Ok(history_source.is_some()
        || provider_key.is_some()
        || source_id.is_some()
        || source_format.is_some())
}

fn normalize_source_identity_filter(label: &str, value: Option<String>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid_tool_request(format!("--{label} cannot be empty")));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid_tool_request(format!(
            "--{label} cannot contain control characters"
        )));
    }
    Ok(Some(value.to_owned()))
}

fn normalize_uuid_prefix(value: &str, kind: &str) -> Result<String> {
    let prefix = value.trim();
    if prefix.len() < 8 {
        return Err(invalid_tool_request(format!(
            "{kind} id prefix must be at least 8 hex characters, or pass a full ctx UUID"
        )));
    }
    if prefix.contains('-')
        || !prefix
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(invalid_tool_request(format!(
            "{kind} id must be a full ctx UUID or an unambiguous hex prefix from verbose search output"
        )));
    }
    Ok(prefix.to_ascii_lowercase())
}

fn parse_since_filter(value: &str) -> Result<chrono::DateTime<Utc>> {
    let trimmed = value.trim();
    if let Some(days) = trimmed.strip_suffix('d') {
        let days: i64 = days.parse().map_err(|error| {
            invalid_tool_request(format!("invalid --since day window: {value}: {error}"))
        })?;
        let duration = Duration::try_days(days).ok_or_else(|| {
            invalid_tool_request(format!(
                "invalid --since day window: {value}: value too large"
            ))
        })?;
        return utc_now().checked_sub_signed(duration).ok_or_else(|| {
            invalid_tool_request(format!(
                "invalid --since day window: {value}: value too large"
            ))
        });
    }
    chrono::DateTime::parse_from_rfc3339(trimmed)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| invalid_tool_request(format!("invalid --since value: {value}: {error}")))
}

pub(super) fn optional_transcript_mode(
    arguments: &Value,
    key: &str,
) -> Result<Option<ToolTranscriptMode>> {
    let Some(mode) = optional_string(arguments, key)? else {
        return Ok(None);
    };
    match mode.as_str() {
        "full" => Ok(Some(ToolTranscriptMode::Full)),
        "lite" => Ok(Some(ToolTranscriptMode::Lite)),
        "log" => Ok(Some(ToolTranscriptMode::Log)),
        _ => Err(invalid_tool_request("mode must be one of full, lite, log")),
    }
}
