use std::time::Duration;

use anyhow::Result;
use ctx_history_core::EventType;
use serde_json::Value;
use uuid::Uuid;

use super::{invalid_tool_request, provider_names};
use crate::{
    cli_supported_provider, ProviderArg, SearchBackendArg, SourceIdentityFilterArgs, TranscriptMode,
};

pub(super) fn optional_string(arguments: &Value, key: &str) -> Result<Option<String>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_tool_request(format!("{key} must be a string"))),
    }
}

pub(super) fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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

pub(super) fn optional_provider(arguments: &Value, key: &str) -> Result<Option<ProviderArg>> {
    let Some(provider) = optional_string(arguments, key)? else {
        return Ok(None);
    };
    ProviderArg::parse_name(&provider)
        .filter(|provider| cli_supported_provider(provider.capture_provider()))
        .map(Some)
        .ok_or_else(|| {
            invalid_tool_request(format!(
                "provider must be one of {}",
                provider_names().join(", ")
            ))
        })
}

pub(super) fn optional_search_backend(
    arguments: &Value,
    key: &str,
) -> Result<Option<SearchBackendArg>> {
    let Some(value) = optional_string(arguments, key)? else {
        return Ok(None);
    };
    match value.as_str() {
        "hybrid" => Ok(Some(SearchBackendArg::Hybrid)),
        "lexical" => Ok(Some(SearchBackendArg::Lexical)),
        "semantic" => Ok(Some(SearchBackendArg::Semantic)),
        _ => Err(invalid_tool_request(
            "backend must be one of hybrid, semantic, lexical",
        )),
    }
}

pub(super) fn allowed_tool_arguments(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "status" | "sources" | "pro_status" => Some(&[]),
        "search" => Some(&[
            "query",
            "limit",
            "provider",
            "history_source",
            "provider_key",
            "source_id",
            "source_format",
            "workspace",
            "since",
            "primary_only",
            "include_subagents",
            "event_type",
            "file",
            "session",
            "events",
            "include_current_session",
            "backend",
            "semantic_weight",
        ]),
        "sql" => Some(&[
            "sql",
            "max_rows",
            "max_columns",
            "max_value_bytes",
            "max_sql_bytes",
            "timeout_ms",
        ]),
        "show_session" => Some(&["ctx_session_id", "mode"]),
        "show_event" => Some(&["ctx_event_id", "before", "after", "window"]),
        "blame" => Some(&["target", "limit", "cursor"]),
        _ => None,
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
    provider: Option<&ProviderArg>,
    source_identity: &SourceIdentityFilterArgs,
    session: Option<&str>,
    since: Option<&str>,
    event_type: Option<&str>,
) -> Result<()> {
    let normalized =
        crate::search_filters::normalize_source_identity_filters(source_identity.clone())
            .map_err(|error| invalid_tool_request(error.to_string()))?;
    if !normalized.is_empty()
        && provider.is_some_and(|provider| !matches!(provider, ProviderArg::Custom))
    {
        return Err(invalid_tool_request(
            "custom history source filters can only be combined with --provider custom",
        ));
    }
    if let Some(value) = session {
        let value = value.trim();
        if Uuid::parse_str(value).is_err() {
            crate::transcript::normalize_uuid_prefix(value, "session")
                .map_err(|error| invalid_tool_request(error.to_string()))?;
        }
    }
    if let Some(value) = since {
        crate::search_filters::parse_since_filter(value)
            .map_err(|error| invalid_tool_request(error.to_string()))?;
    }
    if let Some(value) = event_type {
        value
            .parse::<EventType>()
            .map_err(|error| invalid_tool_request(error.to_string()))?;
    };
    Ok(())
}

pub(super) fn optional_transcript_mode(
    arguments: &Value,
    key: &str,
) -> Result<Option<TranscriptMode>> {
    let Some(mode) = optional_string(arguments, key)? else {
        return Ok(None);
    };
    match mode.as_str() {
        "full" => Ok(Some(TranscriptMode::Full)),
        "lite" => Ok(Some(TranscriptMode::Lite)),
        "log" => Ok(Some(TranscriptMode::Log)),
        _ => Err(invalid_tool_request("mode must be one of full, lite, log")),
    }
}
