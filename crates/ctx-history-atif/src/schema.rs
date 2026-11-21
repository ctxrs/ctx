//! Typed serde schema for ATIF v1.0 through v1.8 (Harbor RFC 0001).

mod content;
mod step;
mod validate;

pub use content::{
    AudioMediaType, AudioSource, ContentPart, ImageMediaType, ImageSource, MessageContent,
};
pub use step::{parse_iso8601_timestamp, Step, StepSource};
pub use validate::{AtifValidationError, MAX_SUBAGENT_NESTING_DEPTH};

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Supported ATIF specification versions (`ATIF-v1.0` through `ATIF-v1.8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SchemaVersion {
    #[serde(rename = "ATIF-v1.0")]
    V1_0,
    #[serde(rename = "ATIF-v1.1")]
    V1_1,
    #[serde(rename = "ATIF-v1.2")]
    V1_2,
    #[serde(rename = "ATIF-v1.3")]
    V1_3,
    #[serde(rename = "ATIF-v1.4")]
    V1_4,
    #[serde(rename = "ATIF-v1.5")]
    V1_5,
    #[serde(rename = "ATIF-v1.6")]
    V1_6,
    #[serde(rename = "ATIF-v1.7")]
    V1_7,
    #[serde(rename = "ATIF-v1.8")]
    V1_8,
}

impl SchemaVersion {
    pub const CURRENT: Self = Self::V1_8;

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V1_0 => "ATIF-v1.0",
            Self::V1_1 => "ATIF-v1.1",
            Self::V1_2 => "ATIF-v1.2",
            Self::V1_3 => "ATIF-v1.3",
            Self::V1_4 => "ATIF-v1.4",
            Self::V1_5 => "ATIF-v1.5",
            Self::V1_6 => "ATIF-v1.6",
            Self::V1_7 => "ATIF-v1.7",
            Self::V1_8 => "ATIF-v1.8",
        }
    }

    pub const fn requires_root_session_id(self) -> bool {
        matches!(
            self,
            Self::V1_0
                | Self::V1_1
                | Self::V1_2
                | Self::V1_3
                | Self::V1_4
                | Self::V1_5
                | Self::V1_6
        )
    }
}

impl Default for SchemaVersion {
    fn default() -> Self {
        Self::CURRENT
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Qualitative or quantitative reasoning effort (`String | Float`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReasoningEffort {
    Text(String),
    Number(f64),
}

/// Agent configuration (`AgentSchema`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_definitions: Option<Vec<Map<String, Value>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

/// Summary metrics for an entire trajectory (`FinalMetricsSchema`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cached_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

impl FinalMetrics {
    pub fn is_empty(&self) -> bool {
        self.total_prompt_tokens.is_none()
            && self.total_completion_tokens.is_none()
            && self.total_cached_tokens.is_none()
            && self.total_cost_usd.is_none()
            && self.total_steps.is_none()
            && self.extra.is_none()
    }
}

/// Per-step LLM metrics (`MetricsSchema`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_token_ids: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_token_ids: Option<Vec<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Vec<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

/// Structured tool invocation (`ToolCallSchema`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

/// Observation container (`ObservationSchema`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub results: Vec<ObservationResult>,
}

/// Individual observation result (`ObservationResultSchema`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_trajectory_ref: Option<Vec<SubagentTrajectoryRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

/// Reference to a delegated subagent trajectory (`SubagentTrajectoryRefSchema`).
///
/// Must set at least one of `trajectory_id` (embedded) or `trajectory_path`
/// (external file). `session_id` is informational only and never resolves alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentTrajectoryRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

/// Root ATIF trajectory document (`Trajectory`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trajectory {
    pub schema_version: SchemaVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    pub agent: Agent,
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<FinalMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_trajectory_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_trajectories: Option<Vec<Trajectory>>,
}

impl Trajectory {
    /// Validates all ATIF v1.0-v1.8 schema and semantic constraints, collecting
    /// every error before returning.
    pub fn validate(&self) -> Result<(), AtifValidationError> {
        validate::validate_trajectory(self)
    }

    /// Returns true if any step message or observation result contains image or
    /// audio content parts (matching Harbor's `Trajectory.has_multimodal_content`).
    pub fn has_multimodal_content(&self) -> bool {
        self.steps.iter().any(|step| {
            step.message.has_media()
                || step.observation.as_ref().is_some_and(|obs| {
                    obs.results.iter().any(|result| {
                        result
                            .content
                            .as_ref()
                            .is_some_and(MessageContent::has_media)
                    })
                })
        })
    }

    /// Resolves an embedded subagent trajectory by its document-scoped
    /// `trajectory_id` (ATIF v1.7+). Never resolves by `session_id`.
    pub fn find_embedded_subagent(&self, trajectory_id: &str) -> Option<&Trajectory> {
        self.subagent_trajectories.as_ref()?.iter().find(|sub| {
            sub.trajectory_id
                .as_deref()
                .is_some_and(|candidate| candidate == trajectory_id)
        })
    }
}
