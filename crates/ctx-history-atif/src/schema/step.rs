//! Step model and ISO 8601 timestamp parsing for ATIF trajectories.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{MessageContent, Metrics, Observation, ReasoningEffort, ToolCall};

/// Originator of one ATIF trajectory step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepSource {
    System,
    User,
    Agent,
}

impl StepSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}

/// One step in an ATIF trajectory (`StepObject`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub step_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub source: StepSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    pub message: MessageContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_copied_context: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_call_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

/// Parses an ATIF ISO 8601 timestamp to UTC.
///
/// Accepts RFC 3339 timestamps (with `Z` or explicit offset, `T` or space
/// separator) as well as naive ISO 8601 datetimes produced by Python's
/// `datetime.isoformat()`, matching Harbor's `Step.validate_timestamp`.
pub fn parse_iso8601_timestamp(raw: &str) -> Result<DateTime<Utc>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed != raw {
        return Err("timestamp must not be empty or have surrounding whitespace".to_owned());
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Ok(parsed.with_timezone(&Utc));
    }
    if let Some(normalized) = raw.strip_suffix('z') {
        let candidate = format!("{normalized}Z");
        if let Ok(parsed) = DateTime::parse_from_rfc3339(&candidate) {
            return Ok(parsed.with_timezone(&Utc));
        }
    }
    for format in [
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M:%S%.f%#z",
        "%Y-%m-%d %H:%M:%S%.f%#z",
    ] {
        if let Ok(parsed) = DateTime::parse_from_str(raw, format) {
            return Ok(parsed.with_timezone(&Utc));
        }
    }
    for naive_format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, naive_format) {
            return Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
        }
    }
    Err(format!("invalid ISO 8601 timestamp `{raw}`"))
}
