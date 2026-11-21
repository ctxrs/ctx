//! Native ctx `CoreRecord` sequence conversion into ATIF v1.8 trajectories.

use std::collections::HashSet;

use chrono::{DateTime, SecondsFormat, Utc};
use ctx_history_core::{ActivityJsonCapture, CoreRecord, LiteralFactKind, TypedKey};
use serde_json::{Map, Value};

use crate::{
    schema::{
        Agent, FinalMetrics, MessageContent, Metrics, Observation, ObservationResult,
        SchemaVersion, Step, StepSource, ToolCall, Trajectory,
    },
    ATIF_SOURCE_FORMAT,
};

use super::{allocate_unique_id, AtifExportError, AtifExportSession};

struct PendingAgentStep {
    timestamp: Option<String>,
    model_name: Option<String>,
    message: String,
    reasoning_content: Option<String>,
    tool_calls: Vec<ToolCall>,
    tool_call_ids: HashSet<String>,
    observation_results: Vec<ObservationResult>,
    metrics: Option<Metrics>,
}

impl PendingAgentStep {
    fn new(timestamp: Option<String>) -> Self {
        Self {
            timestamp,
            model_name: None,
            message: String::new(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_ids: HashSet::new(),
            observation_results: Vec::new(),
            metrics: None,
        }
    }

    fn into_step(self, step_id: u64) -> Step {
        let tool_calls = if self.tool_calls.is_empty() {
            None
        } else {
            Some(self.tool_calls)
        };
        let observation = if self.observation_results.is_empty() {
            None
        } else {
            Some(Observation {
                results: self.observation_results,
            })
        };
        Step {
            step_id,
            timestamp: self.timestamp,
            source: StepSource::Agent,
            model_name: self.model_name,
            reasoning_effort: None,
            message: MessageContent::Text(self.message),
            reasoning_content: self.reasoning_content,
            tool_calls,
            observation,
            metrics: self.metrics,
            is_copied_context: None,
            llm_call_count: None,
            extra: None,
        }
    }
}

pub(super) fn export_native_trajectory(
    session: &AtifExportSession,
    records: &[&CoreRecord],
    embedded_trajectory_id: Option<String>,
) -> Result<Trajectory, AtifExportError> {
    let mut steps: Vec<Step> = Vec::new();
    let mut pending_agent: Option<PendingAgentStep> = None;
    let mut detected_model: Option<String> = None;
    let mut detected_cwd: Option<String> = None;
    let mut detected_agent_version: Option<String> = None;

    for record in records {
        if detected_model.is_none() {
            detected_model = extract_model_name(record);
        }
        if detected_cwd.is_none() {
            detected_cwd = extract_cwd_from_record(record);
        }
        if detected_agent_version.is_none() {
            detected_agent_version = extract_agent_version(record);
        }

        let timestamp = record.occurred_at_unix_ms.and_then(unix_ms_to_iso8601);
        let body = record_body_text(record);

        if is_tool_call_record(record) {
            let mut call = extract_tool_call(record, &body);
            let can_attach = pending_agent
                .as_ref()
                .is_some_and(|step| step.observation_results.is_empty());
            if !can_attach {
                flush_pending_agent(&mut pending_agent, &mut steps);
            }
            let step =
                pending_agent.get_or_insert_with(|| PendingAgentStep::new(timestamp.clone()));
            if step.timestamp.is_none() {
                step.timestamp = timestamp;
            }
            if step.model_name.is_none() {
                step.model_name = extract_model_name(record);
            }
            if !step.tool_call_ids.insert(call.tool_call_id.clone()) {
                let unique = allocate_unique_id(call.tool_call_id, &mut step.tool_call_ids);
                call.tool_call_id = unique;
            }
            step.tool_calls.push(call);
            continue;
        }

        if is_tool_output_record(record) {
            let candidate_call_id = extract_source_call_id(record);
            let output_text = extract_tool_output_text(record, &body);
            if let Some(step) = pending_agent.as_mut() {
                let (source_call_id, extra) = match candidate_call_id {
                    Some(id) if step.tool_call_ids.contains(&id) => (Some(id), None),
                    Some(id) => {
                        let mut extra = Map::new();
                        extra.insert("unmatched_source_call_id".to_owned(), Value::String(id));
                        (None, Some(extra))
                    }
                    None => (None, None),
                };
                step.observation_results.push(ObservationResult {
                    source_call_id,
                    content: Some(MessageContent::Text(output_text)),
                    subagent_trajectory_ref: None,
                    extra,
                });
            } else {
                let extra = candidate_call_id.map(|id| {
                    let mut map = Map::new();
                    map.insert("unmatched_source_call_id".to_owned(), Value::String(id));
                    map
                });
                let next_id = next_step_id(&steps);
                steps.push(Step {
                    step_id: next_id,
                    timestamp,
                    source: StepSource::System,
                    model_name: None,
                    reasoning_effort: None,
                    message: MessageContent::Text(String::new()),
                    reasoning_content: None,
                    tool_calls: None,
                    observation: Some(Observation {
                        results: vec![ObservationResult {
                            source_call_id: None,
                            content: Some(MessageContent::Text(output_text)),
                            subagent_trajectory_ref: None,
                            extra,
                        }],
                    }),
                    metrics: None,
                    is_copied_context: None,
                    llm_call_count: None,
                    extra: None,
                });
            }
            continue;
        }

        match classify_message_source(record) {
            StepSource::System => {
                flush_pending_agent(&mut pending_agent, &mut steps);
                let next_id = next_step_id(&steps);
                steps.push(Step {
                    step_id: next_id,
                    timestamp,
                    source: StepSource::System,
                    model_name: None,
                    reasoning_effort: None,
                    message: MessageContent::Text(body),
                    reasoning_content: None,
                    tool_calls: None,
                    observation: None,
                    metrics: None,
                    is_copied_context: None,
                    llm_call_count: None,
                    extra: None,
                });
            }
            StepSource::User => {
                flush_pending_agent(&mut pending_agent, &mut steps);
                let next_id = next_step_id(&steps);
                steps.push(Step {
                    step_id: next_id,
                    timestamp,
                    source: StepSource::User,
                    model_name: None,
                    reasoning_effort: None,
                    message: MessageContent::Text(body),
                    reasoning_content: None,
                    tool_calls: None,
                    observation: None,
                    metrics: None,
                    is_copied_context: None,
                    llm_call_count: None,
                    extra: None,
                });
            }
            StepSource::Agent => {
                let reasoning = extract_reasoning_content(record);
                let model = extract_model_name(record);
                let metrics = extract_step_metrics(record);
                let must_flush = pending_agent.as_ref().is_some_and(|step| {
                    !step.message.is_empty()
                        || !step.tool_calls.is_empty()
                        || !step.observation_results.is_empty()
                        || (step.reasoning_content.is_some() && reasoning.is_some())
                });
                if must_flush {
                    flush_pending_agent(&mut pending_agent, &mut steps);
                }
                let step =
                    pending_agent.get_or_insert_with(|| PendingAgentStep::new(timestamp.clone()));
                if step.timestamp.is_none() {
                    step.timestamp = timestamp;
                }
                if step.model_name.is_none() {
                    step.model_name = model;
                }
                if step.reasoning_content.is_none() {
                    step.reasoning_content = reasoning;
                }
                if step.metrics.is_none() {
                    step.metrics = metrics;
                }
                if !body.is_empty() {
                    if !step.message.is_empty() {
                        step.message.push_str("\n\n");
                    }
                    step.message.push_str(&body);
                }
            }
        }
    }

    flush_pending_agent(&mut pending_agent, &mut steps);
    if steps.is_empty() {
        return Err(AtifExportError::EmptySession {
            session_id: session.ctx_session_id,
        });
    }

    for (index, step) in steps.iter_mut().enumerate() {
        step.step_id = u64::try_from(index).unwrap_or(0).saturating_add(1);
    }

    let final_metrics = compute_final_metrics(&steps);
    let mut agent_extra = Map::new();
    agent_extra.insert(
        "ctx_provider".to_owned(),
        Value::String(session.provider.as_str().to_owned()),
    );
    if !session.source_format.is_empty() && session.source_format != ATIF_SOURCE_FORMAT {
        agent_extra.insert(
            "ctx_source_format".to_owned(),
            Value::String(session.source_format.clone()),
        );
    }
    if let Some(cwd) = detected_cwd {
        agent_extra.insert("cwd".to_owned(), Value::String(cwd));
    }

    let mut root_extra = Map::new();
    root_extra.insert(
        "ctx_session_id".to_owned(),
        Value::String(session.ctx_session_id.to_string()),
    );

    let session_id = session
        .provider_session_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| session.ctx_session_id.to_string());
    let trajectory_id = embedded_trajectory_id.or_else(|| Some(session.ctx_session_id.to_string()));

    Ok(Trajectory {
        schema_version: SchemaVersion::V1_8,
        session_id: Some(session_id),
        trajectory_id,
        agent: Agent {
            name: session.provider.as_str().to_owned(),
            version: detected_agent_version.unwrap_or_else(|| "unknown".to_owned()),
            model_name: detected_model,
            tool_definitions: None,
            extra: Some(agent_extra),
        },
        steps,
        notes: None,
        final_metrics,
        continued_trajectory_ref: None,
        extra: Some(root_extra),
        subagent_trajectories: None,
    })
}

fn flush_pending_agent(pending: &mut Option<PendingAgentStep>, steps: &mut Vec<Step>) {
    if let Some(step) = pending.take() {
        let step_id = next_step_id(steps);
        steps.push(step.into_step(step_id));
    }
}

fn next_step_id(steps: &[Step]) -> u64 {
    u64::try_from(steps.len()).unwrap_or(0).saturating_add(1)
}

fn is_tool_call_record(record: &CoreRecord) -> bool {
    record.event_type == "tool_call"
        || record
            .content
            .activity
            .as_ref()
            .is_some_and(|activity| activity.invocation.is_some())
}

fn is_tool_output_record(record: &CoreRecord) -> bool {
    record.event_type == "tool_output"
        || record.role.as_deref() == Some("tool")
        || record
            .content
            .activity
            .as_ref()
            .is_some_and(|activity| activity.result.is_some())
}

fn classify_message_source(record: &CoreRecord) -> StepSource {
    match record.role.as_deref() {
        Some("system") | Some("developer") => StepSource::System,
        Some("user") => StepSource::User,
        Some("assistant") | Some("agent") | Some("model") => StepSource::Agent,
        _ => match record.event_type.as_str() {
            "system" | "status" => StepSource::System,
            "user_message" => StepSource::User,
            _ => StepSource::Agent,
        },
    }
}

fn record_body_text(record: &CoreRecord) -> String {
    if let Some(body) = record.content.normalized_body.as_deref() {
        return body.to_owned();
    }
    if let Some(structured) = record.content.structured_content.as_ref() {
        if let Some(text) = structured.as_str() {
            return text.to_owned();
        }
        if let Ok(rendered) = serde_json::to_string(structured) {
            return rendered;
        }
    }
    String::new()
}

fn extract_tool_call(record: &CoreRecord, body: &str) -> ToolCall {
    let activity = record.content.activity.as_ref();
    let invocation = activity.and_then(|act| act.invocation.as_ref());

    let tool_call_id = activity
        .and_then(|act| act.provider_call_id.as_ref())
        .and_then(typed_key_to_call_id)
        .or_else(|| {
            structured_str(
                record.content.structured_content.as_ref(),
                &["tool_call_id", "call_id", "id"],
            )
            .map(str::to_owned)
        })
        .unwrap_or_else(|| format!("call_{}", record.event_sequence));

    let function_name = invocation
        .map(|inv| inv.tool.as_str())
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            structured_str(
                record.content.structured_content.as_ref(),
                &["function_name", "name", "tool"],
            )
        })
        .map(str::to_owned)
        .unwrap_or_else(|| "tool".to_owned());

    let arguments = match invocation.map(|inv| &inv.arguments) {
        Some(ActivityJsonCapture::Present { value }) => value_to_arguments_map(value),
        _ => extract_structured_arguments(record.content.structured_content.as_ref())
            .unwrap_or_else(|| {
                let mut map = Map::new();
                if !body.is_empty() {
                    map.insert("raw".to_owned(), Value::String(body.to_owned()));
                }
                map
            }),
    };

    ToolCall {
        tool_call_id,
        function_name,
        arguments,
        extra: None,
    }
}

fn value_to_arguments_map(value: &Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map.clone(),
        Value::String(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Object(map)) => map,
            _ => {
                let mut map = Map::new();
                map.insert("input".to_owned(), Value::String(raw.clone()));
                map
            }
        },
        Value::Null => Map::new(),
        other => {
            let mut map = Map::new();
            map.insert("value".to_owned(), other.clone());
            map
        }
    }
}

fn extract_structured_arguments(value: Option<&Value>) -> Option<Map<String, Value>> {
    let object = value?.as_object()?;
    for key in ["arguments", "input", "args", "parameters"] {
        if let Some(found) = object.get(key) {
            return Some(value_to_arguments_map(found));
        }
    }
    None
}

fn extract_source_call_id(record: &CoreRecord) -> Option<String> {
    record
        .content
        .activity
        .as_ref()
        .and_then(|act| act.provider_call_id.as_ref())
        .and_then(typed_key_to_call_id)
        .or_else(|| {
            structured_str(
                record.content.structured_content.as_ref(),
                &["source_call_id", "tool_call_id", "call_id"],
            )
            .map(str::to_owned)
        })
}

fn typed_key_to_call_id(key: &TypedKey) -> Option<String> {
    match key {
        TypedKey::Utf8(value) if !value.trim().is_empty() => Some(value.clone()),
        TypedKey::I64(value) => Some(value.to_string()),
        TypedKey::U64(value) => Some(value.to_string()),
        _ => None,
    }
}

fn extract_tool_output_text(record: &CoreRecord, body: &str) -> String {
    if !body.is_empty() {
        return body.to_owned();
    }
    if let Some(ActivityJsonCapture::Present { value }) = record
        .content
        .activity
        .as_ref()
        .and_then(|act| act.result.as_ref())
        .map(|res| &res.structured_content)
    {
        if let Some(text) = value.as_str() {
            return text.to_owned();
        }
        if let Ok(rendered) = serde_json::to_string(value) {
            return rendered;
        }
    }
    String::new()
}

fn extract_cwd_from_record(record: &CoreRecord) -> Option<String> {
    record
        .content
        .activity
        .as_ref()?
        .facts
        .iter()
        .find(|fact| fact.kind == LiteralFactKind::SessionCwd && !fact.value.trim().is_empty())
        .map(|fact| fact.value.clone())
}

fn extract_model_name(record: &CoreRecord) -> Option<String> {
    let value = record.content.structured_content.as_ref()?;
    structured_str(Some(value), &["model_name", "model", "modelName"])
        .or_else(|| structured_str(value.get("message"), &["model", "model_name"]))
        .or_else(|| structured_str(value.get("payload"), &["model", "model_name"]))
        .map(str::to_owned)
}

fn extract_agent_version(record: &CoreRecord) -> Option<String> {
    let value = record.content.structured_content.as_ref()?;
    structured_str(Some(value), &["cli_version", "agent_version", "version"]).map(str::to_owned)
}

fn extract_reasoning_content(record: &CoreRecord) -> Option<String> {
    let value = record.content.structured_content.as_ref()?;
    structured_str(Some(value), &["reasoning_content", "thinking", "reasoning"]).map(str::to_owned)
}

fn extract_step_metrics(record: &CoreRecord) -> Option<Metrics> {
    let value = record.content.structured_content.as_ref()?;
    let usage = value
        .get("usage")
        .or_else(|| value.get("metrics"))
        .or_else(|| value.get("message").and_then(|msg| msg.get("usage")))?
        .as_object()?;
    let cached_tokens = usage
        .get("cached_tokens")
        .or_else(|| usage.get("cache_read_input_tokens"))
        .and_then(Value::as_u64);
    let cache_creation_tokens = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64);
    // Per ATIF RFC 0001, `prompt_tokens` includes ALL input tokens (both cached
    // and non-cached). OpenAI-style `prompt_tokens` already includes cached
    // tokens, whereas Anthropic-style `input_tokens` excludes cache reads and
    // cache creation tokens.
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .map(|input| {
                    input
                        .saturating_add(cached_tokens.unwrap_or(0))
                        .saturating_add(cache_creation_tokens.unwrap_or(0))
                })
        });
    let completion_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64);
    let cost_usd = usage
        .get("cost_usd")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite());
    let extra = cache_creation_tokens.map(|tokens| {
        let mut map = Map::new();
        map.insert(
            "cache_creation_input_tokens".to_owned(),
            Value::Number(tokens.into()),
        );
        map
    });
    if prompt_tokens.is_none()
        && completion_tokens.is_none()
        && cached_tokens.is_none()
        && cost_usd.is_none()
        && extra.is_none()
    {
        return None;
    }
    Some(Metrics {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cost_usd,
        prompt_token_ids: None,
        completion_token_ids: None,
        logprobs: None,
        extra,
    })
}

fn compute_final_metrics(steps: &[Step]) -> Option<FinalMetrics> {
    let mut prompt = 0_u64;
    let mut completion = 0_u64;
    let mut cached = 0_u64;
    let mut cost = 0.0_f64;
    let mut has_prompt = false;
    let mut has_completion = false;
    let mut has_cached = false;
    let mut has_cost = false;

    for step in steps {
        if let Some(metrics) = step.metrics.as_ref() {
            if let Some(value) = metrics.prompt_tokens {
                prompt = prompt.saturating_add(value);
                has_prompt = true;
            }
            if let Some(value) = metrics.completion_tokens {
                completion = completion.saturating_add(value);
                has_completion = true;
            }
            if let Some(value) = metrics.cached_tokens {
                cached = cached.saturating_add(value);
                has_cached = true;
            }
            if let Some(value) = metrics.cost_usd {
                if value.is_finite() {
                    cost += value;
                    has_cost = true;
                }
            }
        }
    }

    if !has_prompt && !has_completion && !has_cached && !has_cost {
        return None;
    }

    Some(FinalMetrics {
        total_prompt_tokens: has_prompt.then_some(prompt),
        total_completion_tokens: has_completion.then_some(completion),
        total_cached_tokens: has_cached.then_some(cached),
        total_cost_usd: (has_cost && cost.is_finite()).then_some(cost),
        total_steps: u64::try_from(steps.len()).ok(),
        extra: None,
    })
}

fn structured_str<'a>(value: Option<&'a Value>, keys: &[&str]) -> Option<&'a str> {
    let object = value?.as_object()?;
    for key in keys {
        if let Some(text) = object.get(*key).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn unix_ms_to_iso8601(unix_ms: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(unix_ms)
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}
