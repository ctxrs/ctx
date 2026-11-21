//! Lossless ATIF-originated and native ctx session export to ATIF v1.8.

mod native;

use std::collections::{BTreeMap, HashSet};

use ctx_history_core::{CaptureProvider, CoreRecord};
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    import::{
        StoredAtifEmptyMessageStep, StoredAtifEnvelope, StoredAtifRecord, StoredAtifRecordKind,
        StoredAtifTrajectoryHeader,
    },
    schema::{
        AtifValidationError, MessageContent, Metrics, Observation, ObservationResult,
        ReasoningEffort, SchemaVersion, Step, StepSource, SubagentTrajectoryRef, ToolCall,
        Trajectory, MAX_SUBAGENT_NESTING_DEPTH,
    },
};

use native::export_native_trajectory;

/// Errors produced when exporting ctx sessions to ATIF v1.8.
#[derive(Debug, Error)]
pub enum AtifExportError {
    #[error("session {session_id} has no exportable events")]
    EmptySession { session_id: Uuid },
    #[error("ATIF subagent nesting depth exceeded maximum {MAX_SUBAGENT_NESTING_DEPTH}")]
    MaxNestingDepthExceeded,
    #[error(transparent)]
    Validation(#[from] AtifValidationError),
    #[error("failed to serialize ATIF JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Input session tree supplied by CLI/archive export callers.
#[derive(Debug, Clone)]
pub struct AtifExportSession {
    pub ctx_session_id: Uuid,
    pub provider: CaptureProvider,
    pub provider_session_id: Option<String>,
    pub source_format: String,
    pub records: Vec<CoreRecord>,
    pub children: Vec<AtifExportSession>,
}

/// Options controlling ATIF v1.8 trajectory export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtifExportOptions {
    pub include_subagents: bool,
}

impl Default for AtifExportOptions {
    fn default() -> Self {
        Self {
            include_subagents: true,
        }
    }
}

/// Exports one ctx session (and optional recursive child sessions) into a
/// strictly validated ATIF v1.8 [`Trajectory`].
pub fn export_session_to_trajectory(
    session: &AtifExportSession,
    options: AtifExportOptions,
) -> Result<Trajectory, AtifExportError> {
    let mut trajectory = build_trajectory_recursive(session, options, None, 0)?;
    trajectory.schema_version = SchemaVersion::V1_8;
    trajectory.validate()?;
    Ok(trajectory)
}

/// Validates and serializes a [`Trajectory`] to pretty-printed ATIF JSON with a
/// trailing newline.
pub fn export_trajectory_json_pretty(trajectory: &Trajectory) -> Result<String, AtifExportError> {
    trajectory.validate()?;
    let mut json = serde_json::to_string_pretty(trajectory)?;
    json.push('\n');
    Ok(json)
}

/// Builds a deterministic, filesystem-safe `.json` filename for batch archive
/// exports (e.g. `codex_<id>.atif.json`).
pub fn safe_trajectory_filename(provider: CaptureProvider, ctx_session_id: Uuid) -> String {
    let provider_slug: String = provider
        .as_str()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{provider_slug}_{ctx_session_id}.atif.json")
}

fn build_trajectory_recursive(
    session: &AtifExportSession,
    options: AtifExportOptions,
    embedded_trajectory_id: Option<String>,
    depth: usize,
) -> Result<Trajectory, AtifExportError> {
    if depth > MAX_SUBAGENT_NESTING_DEPTH {
        return Err(AtifExportError::MaxNestingDepthExceeded);
    }
    if session.records.is_empty() {
        return Err(AtifExportError::EmptySession {
            session_id: session.ctx_session_id,
        });
    }

    let mut ordered_records: Vec<&CoreRecord> = session.records.iter().collect();
    ordered_records.sort_by(|left, right| {
        left.event_sequence
            .cmp(&right.event_sequence)
            .then_with(|| left.occurred_at_unix_ms.cmp(&right.occurred_at_unix_ms))
            .then_with(|| left.event_id.as_uuid().cmp(&right.event_id.as_uuid()))
    });

    let mut trajectory = if let Some(reconstructed) =
        try_reconstruct_atif_trajectory(session, &ordered_records, embedded_trajectory_id.clone())
    {
        reconstructed
    } else {
        export_native_trajectory(session, &ordered_records, embedded_trajectory_id)?
    };

    if options.include_subagents && !session.children.is_empty() {
        let mut ordered_children: Vec<&AtifExportSession> = session.children.iter().collect();
        ordered_children.sort_by_key(|child| embedded_subagent_index(child).unwrap_or(usize::MAX));
        let mut used_ids = HashSet::new();
        let mut embedded = Vec::with_capacity(ordered_children.len());
        for child in ordered_children {
            if child.records.is_empty() {
                continue;
            }
            let preferred_id = preferred_subagent_trajectory_id(child);
            let unique_id = allocate_unique_id(preferred_id, &mut used_ids);
            let child_traj =
                build_trajectory_recursive(child, options, Some(unique_id.clone()), depth + 1)?;
            embedded.push(child_traj);
        }
        if !embedded.is_empty() {
            ensure_parent_references_embedded_subagents(&mut trajectory, &embedded);
            trajectory.subagent_trajectories = Some(embedded);
        }
    }

    Ok(trajectory)
}

fn embedded_subagent_index(child: &AtifExportSession) -> Option<usize> {
    for record in &child.records {
        if let Some(envelope) = extract_stored_envelope(record) {
            if let Some(header) = envelope.atif.trajectory {
                if let Some(index) = header.embedded_subagent_index {
                    return Some(index);
                }
            }
        }
    }
    None
}

fn preferred_subagent_trajectory_id(child: &AtifExportSession) -> String {
    for record in &child.records {
        if let Some(envelope) = extract_stored_envelope(record) {
            if let Some(header) = envelope.atif.trajectory {
                if let Some(tid) = header.trajectory_id.filter(|v| !v.trim().is_empty()) {
                    return tid;
                }
            }
        }
    }
    child
        .provider_session_id
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| child.ctx_session_id.to_string())
}

pub(super) fn allocate_unique_id(base: String, used: &mut HashSet<String>) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    let mut suffix = 2_u64;
    loop {
        let candidate = format!("{base}-{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        suffix = suffix.saturating_add(1);
    }
}

fn ensure_parent_references_embedded_subagents(parent: &mut Trajectory, embedded: &[Trajectory]) {
    let mut already_referenced: HashSet<String> = HashSet::new();
    for step in &mut parent.steps {
        if let Some(obs) = step.observation.as_mut() {
            for result in &mut obs.results {
                if let Some(refs) = result.subagent_trajectory_ref.as_mut() {
                    for sub_ref in refs {
                        if let Some(tid) = sub_ref.trajectory_id.as_deref() {
                            already_referenced.insert(tid.to_owned());
                        }
                    }
                }
            }
        }
    }

    let missing: Vec<SubagentTrajectoryRef> = embedded
        .iter()
        .filter_map(|child| {
            let tid = child.trajectory_id.as_deref()?;
            if already_referenced.contains(tid) {
                return None;
            }
            Some(SubagentTrajectoryRef {
                trajectory_id: Some(tid.to_owned()),
                session_id: child.session_id.clone(),
                trajectory_path: None,
                extra: None,
            })
        })
        .collect();

    if missing.is_empty() {
        return;
    }

    // Attach missing embedded subagent references to the last agent or system
    // step if one exists, otherwise append a system delegation step.
    if let Some(step) = parent
        .steps
        .iter_mut()
        .rev()
        .find(|step| matches!(step.source, StepSource::Agent | StepSource::System))
    {
        let obs = step.observation.get_or_insert_with(|| Observation {
            results: Vec::new(),
        });
        obs.results.push(ObservationResult {
            source_call_id: None,
            content: None,
            subagent_trajectory_ref: Some(missing),
            extra: None,
        });
    } else {
        let next_step_id = u64::try_from(parent.steps.len())
            .unwrap_or(0)
            .saturating_add(1);
        parent.steps.push(Step {
            step_id: next_step_id,
            timestamp: None,
            source: StepSource::System,
            model_name: None,
            reasoning_effort: None,
            message: MessageContent::Text("Delegated subagent sessions".to_owned()),
            reasoning_content: None,
            tool_calls: None,
            observation: Some(Observation {
                results: vec![ObservationResult {
                    source_call_id: None,
                    content: None,
                    subagent_trajectory_ref: Some(missing),
                    extra: None,
                }],
            }),
            metrics: None,
            is_copied_context: None,
            llm_call_count: None,
            extra: None,
        });
    }
}

fn extract_stored_envelope(record: &CoreRecord) -> Option<StoredAtifEnvelope> {
    let value = record.content.structured_content.as_ref()?;
    value.get("atif")?;
    serde_json::from_value(value.clone()).ok()
}

struct ReconstructedStep {
    source: StepSource,
    timestamp: Option<String>,
    message: MessageContent,
    model_name: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    reasoning_content: Option<String>,
    metrics: Option<Metrics>,
    is_copied_context: Option<bool>,
    llm_call_count: Option<u64>,
    extra: Option<Map<String, Value>>,
    tool_calls: BTreeMap<usize, ToolCall>,
    observation_results: BTreeMap<usize, ObservationResult>,
}

impl ReconstructedStep {
    fn new(source: StepSource, timestamp: Option<String>) -> Self {
        Self {
            source,
            timestamp,
            message: MessageContent::Text(String::new()),
            model_name: None,
            reasoning_effort: None,
            reasoning_content: None,
            metrics: None,
            is_copied_context: None,
            llm_call_count: None,
            extra: None,
            tool_calls: BTreeMap::new(),
            observation_results: BTreeMap::new(),
        }
    }

    fn apply_empty_step_meta(&mut self, meta: StoredAtifEmptyMessageStep) {
        self.message = meta.message;
        self.model_name = meta.model_name;
        self.reasoning_effort = meta.reasoning_effort;
        self.reasoning_content = meta.reasoning_content;
        self.metrics = meta.metrics;
        self.is_copied_context = meta.is_copied_context;
        self.llm_call_count = meta.llm_call_count;
        self.extra = meta.extra;
    }
}

fn try_reconstruct_atif_trajectory(
    session: &AtifExportSession,
    records: &[&CoreRecord],
    embedded_trajectory_id: Option<String>,
) -> Option<Trajectory> {
    let envelopes: Vec<StoredAtifRecord> = records
        .iter()
        .filter_map(|record| extract_stored_envelope(record).map(|env| env.atif))
        .collect();
    if envelopes.is_empty() || envelopes.len() != records.len() {
        return None;
    }

    let mut header: Option<StoredAtifTrajectoryHeader> = None;
    let mut steps_map: BTreeMap<u64, ReconstructedStep> = BTreeMap::new();

    for stored in envelopes {
        if header.is_none() {
            header = stored.trajectory;
        }
        let entry = steps_map.entry(stored.step_id).or_insert_with(|| {
            ReconstructedStep::new(stored.step_source, stored.step_timestamp.clone())
        });
        if entry.timestamp.is_none() {
            entry.timestamp = stored.step_timestamp;
        }
        match stored.kind {
            StoredAtifRecordKind::StepMessage {
                message,
                model_name,
                reasoning_effort,
                reasoning_content,
                metrics,
                is_copied_context,
                llm_call_count,
                extra,
            } => {
                entry.message = message;
                entry.model_name = model_name;
                entry.reasoning_effort = reasoning_effort;
                entry.reasoning_content = reasoning_content;
                entry.metrics = metrics;
                entry.is_copied_context = is_copied_context;
                entry.llm_call_count = llm_call_count;
                entry.extra = extra;
            }
            StoredAtifRecordKind::ToolCall {
                tool_call_index,
                tool_call,
                step_metadata,
            } => {
                if let Some(meta) = step_metadata {
                    entry.apply_empty_step_meta(meta);
                }
                entry.tool_calls.insert(tool_call_index, tool_call);
            }
            StoredAtifRecordKind::ObservationResult {
                result_index,
                result,
                step_metadata,
            } => {
                if let Some(meta) = step_metadata {
                    entry.apply_empty_step_meta(meta);
                }
                entry.observation_results.insert(result_index, result);
            }
        }
    }

    if steps_map.is_empty() {
        return None;
    }

    let mut steps = Vec::with_capacity(steps_map.len());
    for (index, (_, reconstructed)) in steps_map.into_iter().enumerate() {
        let step_id = u64::try_from(index).unwrap_or(0).saturating_add(1);
        let tool_calls = if reconstructed.tool_calls.is_empty() {
            None
        } else {
            Some(reconstructed.tool_calls.into_values().collect::<Vec<_>>())
        };
        let valid_call_ids: HashSet<String> = tool_calls
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|call| call.tool_call_id.clone())
            .collect();
        let observation = if reconstructed.observation_results.is_empty() {
            None
        } else {
            let results = reconstructed
                .observation_results
                .into_values()
                .map(|mut result| {
                    if let Some(source_call_id) = result.source_call_id.as_deref() {
                        if reconstructed.source != StepSource::Agent
                            || !valid_call_ids.contains(source_call_id)
                        {
                            result.source_call_id = None;
                        }
                    }
                    result
                })
                .collect();
            Some(Observation { results })
        };

        steps.push(Step {
            step_id,
            timestamp: reconstructed.timestamp,
            source: reconstructed.source,
            model_name: reconstructed.model_name,
            reasoning_effort: reconstructed.reasoning_effort,
            message: reconstructed.message,
            reasoning_content: reconstructed.reasoning_content,
            tool_calls,
            observation,
            metrics: reconstructed.metrics,
            is_copied_context: reconstructed.is_copied_context,
            llm_call_count: reconstructed.llm_call_count,
            extra: reconstructed.extra,
        });
    }

    let header = header?;
    let trajectory_id = embedded_trajectory_id.or(header.trajectory_id);
    let session_id = header.session_id.or_else(|| {
        if trajectory_id.is_some() {
            None
        } else {
            session
                .provider_session_id
                .clone()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| Some(session.ctx_session_id.to_string()))
        }
    });

    Some(Trajectory {
        schema_version: SchemaVersion::V1_8,
        session_id,
        trajectory_id,
        agent: header.agent,
        steps,
        notes: header.notes,
        final_metrics: header.final_metrics,
        continued_trajectory_ref: header.continued_trajectory_ref,
        extra: header.extra,
        subagent_trajectories: None,
    })
}
