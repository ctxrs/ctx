//! Complete ATIF v1.0-v1.8 validation rules matching RFC 0001 and Harbor.

use std::{collections::HashSet, fmt};

use super::{
    parse_iso8601_timestamp, ContentPart, MessageContent, ReasoningEffort, Step, StepSource,
    Trajectory,
};

/// Maximum nesting depth for recursive `subagent_trajectories` validation.
pub const MAX_SUBAGENT_NESTING_DEPTH: usize = 32;

/// Aggregated ATIF schema validation failure collecting every violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtifValidationError {
    pub errors: Vec<String>,
}

impl AtifValidationError {
    pub fn single(message: impl Into<String>) -> Self {
        Self {
            errors: vec![message.into()],
        }
    }
}

impl fmt::Display for AtifValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.errors.len() == 1 {
            write!(formatter, "ATIF validation failed: {}", self.errors[0])
        } else {
            write!(
                formatter,
                "ATIF validation failed with {} errors: {}",
                self.errors.len(),
                self.errors.join("; ")
            )
        }
    }
}

impl std::error::Error for AtifValidationError {}

pub(super) fn validate_trajectory(trajectory: &Trajectory) -> Result<(), AtifValidationError> {
    let mut errors = Vec::new();
    validate_trajectory_at(trajectory, "trajectory", false, 0, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AtifValidationError { errors })
    }
}

fn validate_trajectory_at(
    trajectory: &Trajectory,
    prefix: &str,
    is_embedded: bool,
    depth: usize,
    errors: &mut Vec<String>,
) {
    if depth > MAX_SUBAGENT_NESTING_DEPTH {
        errors.push(format!(
            "{prefix}: subagent_trajectories nesting depth exceeds maximum {MAX_SUBAGENT_NESTING_DEPTH}"
        ));
        return;
    }

    if !is_embedded && trajectory.schema_version.requires_root_session_id() {
        match trajectory.session_id.as_deref() {
            Some(id) if !id.trim().is_empty() => {}
            _ => errors.push(format!(
                "{prefix}.session_id: required for schema_version {}",
                trajectory.schema_version.as_str()
            )),
        }
    }

    if let Some(session_id) = trajectory.session_id.as_deref() {
        if session_id.trim().is_empty() {
            errors.push(format!(
                "{prefix}.session_id: must not be empty when present"
            ));
        }
    }

    if let Some(trajectory_id) = trajectory.trajectory_id.as_deref() {
        if trajectory_id.trim().is_empty() {
            errors.push(format!(
                "{prefix}.trajectory_id: must not be empty when present"
            ));
        }
    }

    if trajectory.agent.name.trim().is_empty() {
        errors.push(format!("{prefix}.agent.name: must not be empty"));
    }

    if let Some(final_metrics) = trajectory.final_metrics.as_ref() {
        if let Some(cost) = final_metrics.total_cost_usd {
            if !cost.is_finite() {
                errors.push(format!(
                    "{prefix}.final_metrics.total_cost_usd: must be a finite number"
                ));
            }
        }
    }

    if trajectory.steps.is_empty() {
        errors.push(format!("{prefix}.steps: must contain at least one step"));
    } else {
        for (index, step) in trajectory.steps.iter().enumerate() {
            let expected_step_id = u64::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .unwrap_or(u64::MAX);
            let step_prefix = format!("{prefix}.steps[{index}]");
            if step.step_id != expected_step_id {
                errors.push(format!(
                    "{step_prefix}.step_id: expected {expected_step_id} (sequential from 1), got {}",
                    step.step_id
                ));
            }
            validate_step(step, &step_prefix, errors);
        }
    }

    if let Some(subagents) = trajectory.subagent_trajectories.as_ref() {
        let mut seen_trajectory_ids = HashSet::with_capacity(subagents.len());
        for (index, subagent) in subagents.iter().enumerate() {
            let sub_prefix = format!("{prefix}.subagent_trajectories[{index}]");
            match subagent.trajectory_id.as_deref() {
                Some(id) if !id.trim().is_empty() => {
                    if !seen_trajectory_ids.insert(id.to_owned()) {
                        errors.push(format!(
                            "{sub_prefix}.trajectory_id: `{id}` is not unique within subagent_trajectories"
                        ));
                    }
                }
                _ => {
                    errors.push(format!(
                        "{sub_prefix}.trajectory_id: required and non-empty for embedded subagents"
                    ));
                }
            }
            validate_trajectory_at(subagent, &sub_prefix, true, depth + 1, errors);
        }
    }
}

fn validate_step(step: &Step, prefix: &str, errors: &mut Vec<String>) {
    if let Some(timestamp) = step.timestamp.as_deref() {
        if let Err(detail) = parse_iso8601_timestamp(timestamp) {
            errors.push(format!("{prefix}.timestamp: {detail}"));
        }
    }

    validate_message_content(&step.message, &format!("{prefix}.message"), errors);

    if step.source != StepSource::Agent {
        if step.model_name.is_some() {
            errors.push(format!(
                "{prefix}.model_name: only applicable when source is 'agent', got '{}'",
                step.source.as_str()
            ));
        }
        if step.reasoning_effort.is_some() {
            errors.push(format!(
                "{prefix}.reasoning_effort: only applicable when source is 'agent', got '{}'",
                step.source.as_str()
            ));
        }
        if step.reasoning_content.is_some() {
            errors.push(format!(
                "{prefix}.reasoning_content: only applicable when source is 'agent', got '{}'",
                step.source.as_str()
            ));
        }
        if step.tool_calls.is_some() {
            errors.push(format!(
                "{prefix}.tool_calls: only applicable when source is 'agent', got '{}'",
                step.source.as_str()
            ));
        }
        if step.metrics.is_some() {
            errors.push(format!(
                "{prefix}.metrics: only applicable when source is 'agent', got '{}'",
                step.source.as_str()
            ));
        }
    }

    if step.source == StepSource::Agent && step.llm_call_count == Some(0) {
        if step.metrics.is_some() {
            errors.push(format!(
                "{prefix}.metrics: must be absent when llm_call_count is 0 (deterministic dispatch)"
            ));
        }
        if step.reasoning_content.is_some() {
            errors.push(format!(
                "{prefix}.reasoning_content: must be absent when llm_call_count is 0 (deterministic dispatch)"
            ));
        }
    }

    if let Some(ReasoningEffort::Number(value)) = step.reasoning_effort.as_ref() {
        if !value.is_finite() {
            errors.push(format!(
                "{prefix}.reasoning_effort: numeric value must be finite"
            ));
        }
    }

    if let Some(metrics) = step.metrics.as_ref() {
        if let Some(cost) = metrics.cost_usd {
            if !cost.is_finite() {
                errors.push(format!(
                    "{prefix}.metrics.cost_usd: must be a finite number"
                ));
            }
        }
        if let Some(logprobs) = metrics.logprobs.as_ref() {
            for (index, value) in logprobs.iter().enumerate() {
                if !value.is_finite() {
                    errors.push(format!(
                        "{prefix}.metrics.logprobs[{index}]: must be a finite number"
                    ));
                }
            }
        }
    }

    let mut tool_call_ids = HashSet::new();
    if let Some(tool_calls) = step.tool_calls.as_ref() {
        for (index, call) in tool_calls.iter().enumerate() {
            if call.tool_call_id.trim().is_empty() {
                errors.push(format!(
                    "{prefix}.tool_calls[{index}].tool_call_id: must not be empty"
                ));
            }
            if call.function_name.trim().is_empty() {
                errors.push(format!(
                    "{prefix}.tool_calls[{index}].function_name: must not be empty"
                ));
            }
            tool_call_ids.insert(call.tool_call_id.as_str());
        }
    }

    if let Some(observation) = step.observation.as_ref() {
        for (res_idx, result) in observation.results.iter().enumerate() {
            let result_prefix = format!("{prefix}.observation.results[{res_idx}]");
            if let Some(source_call_id) = result.source_call_id.as_deref() {
                if step.source != StepSource::Agent {
                    errors.push(format!(
                        "{result_prefix}.source_call_id: not allowed when source is '{}' (only 'agent' steps may have tool_calls)",
                        step.source.as_str()
                    ));
                } else if !tool_call_ids.contains(source_call_id) {
                    errors.push(format!(
                        "{result_prefix}.source_call_id: references `{source_call_id}` which is not found in step {}'s tool_calls",
                        step.step_id
                    ));
                }
            }
            if let Some(content) = result.content.as_ref() {
                validate_message_content(content, &format!("{result_prefix}.content"), errors);
            }
            if let Some(refs) = result.subagent_trajectory_ref.as_ref() {
                for (ref_idx, sub_ref) in refs.iter().enumerate() {
                    let ref_prefix = format!("{result_prefix}.subagent_trajectory_ref[{ref_idx}]");
                    let has_trajectory_id = match sub_ref.trajectory_id.as_deref() {
                        Some(value) if !value.trim().is_empty() => true,
                        Some(_) => {
                            errors.push(format!(
                                "{ref_prefix}.trajectory_id: must not be empty when present"
                            ));
                            false
                        }
                        None => false,
                    };
                    let has_trajectory_path = match sub_ref.trajectory_path.as_deref() {
                        Some(value) if !value.trim().is_empty() => true,
                        Some(_) => {
                            errors.push(format!(
                                "{ref_prefix}.trajectory_path: must not be empty when present"
                            ));
                            false
                        }
                        None => false,
                    };
                    if !has_trajectory_id && !has_trajectory_path {
                        errors.push(format!(
                            "{ref_prefix}: SubagentTrajectoryRef must set `trajectory_id` or `trajectory_path` (`session_id` alone is not a resolution key)"
                        ));
                    }
                }
            }
        }
    }
}

fn validate_message_content(content: &MessageContent, prefix: &str, errors: &mut Vec<String>) {
    let MessageContent::Parts(parts) = content else {
        return;
    };
    for (index, part) in parts.iter().enumerate() {
        let part_prefix = format!("{prefix}[{index}]");
        match part {
            ContentPart::Text { .. } => {}
            ContentPart::Image { source } => {
                if source.path.trim().is_empty() {
                    errors.push(format!("{part_prefix}.source.path: must not be empty"));
                }
            }
            ContentPart::Audio { source } => {
                if source.path.trim().is_empty() {
                    errors.push(format!("{part_prefix}.source.path: must not be empty"));
                }
                if let Some(duration) = source.duration_sec {
                    if !duration.is_finite() || duration < 0.0 {
                        errors.push(format!(
                            "{part_prefix}.source.duration_sec: must be a finite number >= 0, got {duration}"
                        ));
                    }
                }
            }
        }
    }
}
