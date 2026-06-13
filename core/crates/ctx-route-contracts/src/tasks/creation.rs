use ctx_core::ids::TaskId;
use ctx_core::models::ExecutionEnvironment;
use serde::Deserialize;

use super::common::{parse_task_id, TaskRouteError};
use super::responses::ExecutionEnvironmentRouteValue;

const KNOWN_REASONING_EFFORT_IDS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTaskRouteRequest {
    #[serde(default)]
    id: Option<String>,
    title: String,
    description: Option<String>,
    #[serde(default)]
    default_session: Option<CreateTaskDefaultSessionRouteRequest>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTaskDefaultSessionRouteRequest {
    #[serde(default)]
    id: Option<String>,
    #[serde(deserialize_with = "deserialize_provider_id")]
    provider_id: String,
    #[serde(deserialize_with = "deserialize_concrete_model_id")]
    model_id: String,
    #[serde(default, deserialize_with = "deserialize_optional_reasoning_effort")]
    reasoning_effort: Option<String>,
    #[serde(default)]
    remember_model_preference: bool,
    #[serde(default)]
    initial_prompt: Option<String>,
    #[serde(default)]
    initial_message_id: Option<String>,
    #[serde(default)]
    initial_turn_id: Option<String>,
    #[serde(default)]
    worktree_id: Option<String>,
    #[serde(default)]
    execution_environment: Option<ExecutionEnvironmentRouteValue>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTaskSessionRouteRequest {
    #[serde(default)]
    id: Option<String>,
    #[serde(deserialize_with = "deserialize_provider_id")]
    provider_id: String,
    #[serde(deserialize_with = "deserialize_concrete_model_id")]
    model_id: String,
    #[serde(default, deserialize_with = "deserialize_optional_reasoning_effort")]
    reasoning_effort: Option<String>,
    #[serde(default)]
    remember_model_preference: bool,
    parent_session_id: Option<String>,
    relationship: Option<String>,
    #[serde(default)]
    initial_prompt: Option<String>,
    #[serde(default)]
    initial_message_id: Option<String>,
    #[serde(default)]
    initial_turn_id: Option<String>,
    #[serde(default)]
    worktree_id: Option<String>,
    #[serde(default)]
    execution_environment: Option<ExecutionEnvironmentRouteValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTaskRouteSpec {
    pub task_id: Option<TaskId>,
    pub title: String,
    pub description: Option<String>,
    pub default_session: Option<CreateTaskSessionRouteSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTaskSessionRouteSpec {
    pub id: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub remember_model_preference: bool,
    pub parent_session_id: Option<String>,
    pub relationship: Option<String>,
    pub initial_prompt: Option<String>,
    pub initial_message_id: Option<String>,
    pub initial_turn_id: Option<String>,
    pub worktree_id: Option<String>,
    pub execution_environment: Option<ExecutionEnvironment>,
    pub run_id_header: Option<String>,
}

impl CreateTaskRouteRequest {
    pub fn into_spec(self) -> Result<CreateTaskRouteSpec, TaskRouteError> {
        let task_id = match self.id.as_deref().map(str::trim) {
            Some("") | None => None,
            Some(raw) => Some(parse_task_id(raw)?),
        };
        Ok(CreateTaskRouteSpec {
            task_id,
            title: self.title,
            description: self.description,
            default_session: self
                .default_session
                .map(|default_session| default_session.into_task_session_spec(None)),
        })
    }
}

impl CreateTaskDefaultSessionRouteRequest {
    fn into_task_session_spec(self, run_id_header: Option<String>) -> CreateTaskSessionRouteSpec {
        CreateTaskSessionRouteSpec {
            id: self.id,
            provider_id: self.provider_id,
            model_id: self.model_id,
            reasoning_effort: self.reasoning_effort,
            remember_model_preference: self.remember_model_preference,
            parent_session_id: None,
            relationship: None,
            initial_prompt: self.initial_prompt,
            initial_message_id: self.initial_message_id,
            initial_turn_id: self.initial_turn_id,
            worktree_id: self.worktree_id,
            execution_environment: self.execution_environment.map(Into::into),
            run_id_header,
        }
    }
}

impl CreateTaskSessionRouteRequest {
    pub fn into_spec(self, run_id_header: Option<String>) -> CreateTaskSessionRouteSpec {
        CreateTaskSessionRouteSpec {
            id: self.id,
            provider_id: self.provider_id,
            model_id: self.model_id,
            reasoning_effort: self.reasoning_effort,
            remember_model_preference: self.remember_model_preference,
            parent_session_id: self.parent_session_id,
            relationship: self.relationship,
            initial_prompt: self.initial_prompt,
            initial_message_id: self.initial_message_id,
            initial_turn_id: self.initial_turn_id,
            worktree_id: self.worktree_id,
            execution_environment: self.execution_environment.map(Into::into),
            run_id_header,
        }
    }
}

fn deserialize_concrete_model_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(serde::de::Error::custom("model_id must not be empty"));
    }
    if trimmed.eq_ignore_ascii_case("default") {
        return Err(serde::de::Error::custom(
            "model_id must be a concrete model id",
        ));
    }
    Ok(trimmed.to_string())
}

fn deserialize_provider_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(serde::de::Error::custom("provider_id must not be empty"));
    }
    Ok(trimmed.to_string())
}

fn deserialize_optional_reasoning_effort<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(raw) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let normalized = normalize_reasoning_effort(trimmed);
    if !KNOWN_REASONING_EFFORT_IDS.contains(&normalized.as_str()) {
        return Err(serde::de::Error::custom(format!(
            "invalid reasoning_effort '{trimmed}'"
        )));
    }
    Ok(Some(normalized))
}

fn normalize_reasoning_effort(value: &str) -> String {
    let raw = value.trim().to_lowercase();
    match raw.as_str() {
        "extra_high" | "extra-high" | "extra high" | "extrahigh" => "xhigh".to_string(),
        _ => raw,
    }
}
