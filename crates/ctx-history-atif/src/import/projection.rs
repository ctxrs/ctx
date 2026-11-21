//! Projects validated ATIF trajectories and embedded subagents into `CoreRecord`s.

use std::collections::HashSet;

use ctx_history_core::{
    admit_optional_provider_call_id, admit_provider_declared_fact, derive_event_id,
    derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, AgentScope, CoreActivity, CoreContent, CoreContentPolicyStatus,
    CoreRecord, CoreRecordError, EventIdentityInput, EventRole, EventType, LiteralFactKind,
    NativeItemKey, NativeSessionKey, ProjectionContractError, ProviderDeclaredFact,
    ProviderNativeSessionRelationship, SessionIdentityInput, SourceKey, StableEntityId, TypedKey,
    CORE_ACTIVITY_REVISION, CORE_CONTENT_POLICY_REVISION, CORE_NORMALIZATION_REVISION,
    CORE_RECORD_VERSION, MAX_CORE_CONTENT_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::schema::{
    parse_iso8601_timestamp, Agent, FinalMetrics, MessageContent, Metrics, ObservationResult,
    ReasoningEffort, SchemaVersion, Step, StepSource, ToolCall, Trajectory,
    MAX_SUBAGENT_NESTING_DEPTH,
};

use super::ATIF_PARSER_REVISION;

pub const ATIF_STORED_METADATA_VERSION: u32 = 1;

const ATIF_LOGICAL_SESSION_KIND: &str = "atif-trajectory";
const ATIF_NATIVE_SESSION_NAMESPACE: &str = "atif.trajectory";
const ATIF_LOGICAL_EVENT_KIND: &str = "atif-step-event";
const ATIF_NATIVE_EVENT_NAMESPACE: &str = "atif.event";

#[derive(Debug, Error)]
pub enum AtifProjectionError {
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("ATIF subagent nesting depth exceeded maximum {MAX_SUBAGENT_NESTING_DEPTH}")]
    MaxNestingDepthExceeded,
}

/// Stored metadata envelope written to `CoreRecord.content.structured_content`
/// so ATIF-originated sessions round-trip losslessly back to ATIF v1.8.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredAtifEnvelope {
    pub atif: StoredAtifRecord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredAtifRecord {
    pub format_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory: Option<StoredAtifTrajectoryHeader>,
    pub step_id: u64,
    pub step_source: StepSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_timestamp: Option<String>,
    pub kind: StoredAtifRecordKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredAtifTrajectoryHeader {
    pub schema_version: SchemaVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    pub agent: Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<FinalMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_trajectory_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_embedded_subagent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_subagent_index: Option<usize>,
}

/// Step-level metadata carried on the first tool call or observation record
/// when a step's `message` is an empty string and no `StepMessage` event is emitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredAtifEmptyMessageStep {
    pub message: MessageContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_copied_context: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_call_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record_kind", rename_all = "snake_case")]
pub enum StoredAtifRecordKind {
    StepMessage {
        message: MessageContent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<ReasoningEffort>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metrics: Option<Metrics>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_copied_context: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        llm_call_count: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extra: Option<Map<String, Value>>,
    },
    ToolCall {
        tool_call_index: usize,
        tool_call: ToolCall,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_metadata: Option<StoredAtifEmptyMessageStep>,
    },
    ObservationResult {
        result_index: usize,
        result: ObservationResult,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_metadata: Option<StoredAtifEmptyMessageStep>,
    },
}

/// Optional lineage linking an external ATIF trajectory file to its parent/root
/// trajectory within the same catalog source.
#[derive(Debug, Clone)]
pub struct AtifLineageLink {
    pub parent_session_id: StableEntityId,
    pub root_session_id: StableEntityId,
    pub relationship: ProviderNativeSessionRelationship,
    pub agent_scope: AgentScope,
    pub inherited_session_id: Option<String>,
}

/// Input bundle for projecting one validated ATIF trajectory (and all of its
/// embedded `subagent_trajectories`) into `CoreRecord`s.
#[derive(Debug, Clone)]
pub struct AtifProjectionInput<'a> {
    pub source: &'a SourceKey,
    pub document_key: &'a str,
    pub trajectory: &'a Trajectory,
    pub lineage: Option<&'a AtifLineageLink>,
}

/// Derives the deterministic `provider_session_id` and `StableEntityId` for a
/// root or external ATIF trajectory document.
pub fn derive_atif_document_session_id(
    source: &SourceKey,
    document_key: &str,
    trajectory: &Trajectory,
) -> Result<(String, StableEntityId), ProjectionContractError> {
    let provider_session_id = trajectory
        .trajectory_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            trajectory
                .session_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| format!("atif:{document_key}"));
    let native_key = NativeSessionKey::composite(
        ATIF_NATIVE_SESSION_NAMESPACE,
        vec![
            TypedKey::utf8(document_key)?,
            TypedKey::utf8(&provider_session_id)?,
        ],
    )?;
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: ATIF_LOGICAL_SESSION_KIND,
        native_session_key: &native_key,
    })?;
    Ok((provider_session_id, session_id))
}

fn derive_embedded_subagent_session_id(
    source: &SourceKey,
    document_key: &str,
    parent_provider_session_id: &str,
    subagent_path: &[&str],
    sub_trajectory_id: &str,
    used_provider_session_ids: &mut HashSet<String>,
) -> Result<(String, StableEntityId), ProjectionContractError> {
    let provider_session_id = if used_provider_session_ids.insert(sub_trajectory_id.to_owned()) {
        sub_trajectory_id.to_owned()
    } else {
        let disambiguated = format!("{parent_provider_session_id}::subagent::{sub_trajectory_id}");
        used_provider_session_ids.insert(disambiguated.clone());
        disambiguated
    };
    let mut key_parts = Vec::with_capacity(subagent_path.len() + 2);
    key_parts.push(TypedKey::utf8(document_key)?);
    key_parts.push(TypedKey::utf8("embedded")?);
    for segment in subagent_path {
        key_parts.push(TypedKey::utf8(*segment)?);
    }
    let native_key = NativeSessionKey::composite(ATIF_NATIVE_SESSION_NAMESPACE, key_parts)?;
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: ATIF_LOGICAL_SESSION_KIND,
        native_session_key: &native_key,
    })?;
    Ok((provider_session_id, session_id))
}

/// Projects one validated ATIF trajectory and any embedded `subagent_trajectories`
/// into ordered, validated `CoreRecord`s.
pub fn project_trajectory_to_core_records(
    input: &AtifProjectionInput<'_>,
) -> Result<Vec<CoreRecord>, AtifProjectionError> {
    let (provider_session_id, session_id) =
        derive_atif_document_session_id(input.source, input.document_key, input.trajectory)?;
    let mut used_provider_session_ids = HashSet::new();
    used_provider_session_ids.insert(provider_session_id.clone());

    let (parent_session_id, root_session_id, relationship, agent_scope, inherited_session_id) =
        match input.lineage {
            Some(link) => (
                Some(link.parent_session_id),
                Some(link.root_session_id),
                Some(link.relationship),
                Some(link.agent_scope),
                link.inherited_session_id.clone(),
            ),
            None => (
                None,
                None,
                Some(ProviderNativeSessionRelationship::Root),
                Some(AgentScope::Primary),
                None,
            ),
        };
    let effective_root_id = root_session_id.unwrap_or(session_id);

    let mut records = Vec::new();
    let ctx = TrajectoryEmitContext {
        source: input.source,
        document_key: input.document_key,
        session_id,
        provider_session_id: &provider_session_id,
        parent_session_id,
        root_session_id,
        session_relationship: relationship,
        agent_scope,
        inherited_session_id: inherited_session_id.as_deref(),
        is_embedded_subagent: false,
        embedded_subagent_index: None,
    };
    emit_single_trajectory(input.trajectory, &ctx, &mut records)?;

    let effective_run_session_id = input
        .trajectory
        .session_id
        .as_deref()
        .or(inherited_session_id.as_deref());
    let mut path_segments = Vec::new();
    emit_embedded_subagents(
        input.source,
        input.document_key,
        input.trajectory,
        session_id,
        &provider_session_id,
        effective_root_id,
        effective_run_session_id,
        &mut path_segments,
        &mut used_provider_session_ids,
        0,
        &mut records,
    )?;

    Ok(records)
}

#[allow(clippy::too_many_arguments)]
fn emit_embedded_subagents<'a>(
    source: &SourceKey,
    document_key: &str,
    parent: &'a Trajectory,
    parent_session_id: StableEntityId,
    parent_provider_session_id: &str,
    root_session_id: StableEntityId,
    parent_run_session_id: Option<&'a str>,
    path_segments: &mut Vec<&'a str>,
    used_provider_session_ids: &mut HashSet<String>,
    depth: usize,
    out: &mut Vec<CoreRecord>,
) -> Result<(), AtifProjectionError> {
    let Some(subagents) = parent.subagent_trajectories.as_ref() else {
        return Ok(());
    };
    if depth >= MAX_SUBAGENT_NESTING_DEPTH {
        return Err(AtifProjectionError::MaxNestingDepthExceeded);
    }
    for (sub_index, subagent) in subagents.iter().enumerate() {
        let sub_tid = subagent
            .trajectory_id
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or("unnamed-subagent");
        path_segments.push(sub_tid);
        let (sub_provider_session_id, sub_session_id) = derive_embedded_subagent_session_id(
            source,
            document_key,
            parent_provider_session_id,
            path_segments,
            sub_tid,
            used_provider_session_ids,
        )?;
        let inherited = subagent
            .session_id
            .is_none()
            .then_some(parent_run_session_id)
            .flatten();
        let ctx = TrajectoryEmitContext {
            source,
            document_key,
            session_id: sub_session_id,
            provider_session_id: &sub_provider_session_id,
            parent_session_id: Some(parent_session_id),
            root_session_id: Some(root_session_id),
            session_relationship: Some(ProviderNativeSessionRelationship::Delegated),
            agent_scope: Some(AgentScope::Subagent),
            inherited_session_id: inherited,
            is_embedded_subagent: true,
            embedded_subagent_index: Some(sub_index),
        };
        emit_single_trajectory(subagent, &ctx, out)?;
        let next_run_session_id = subagent.session_id.as_deref().or(parent_run_session_id);
        emit_embedded_subagents(
            source,
            document_key,
            subagent,
            sub_session_id,
            &sub_provider_session_id,
            root_session_id,
            next_run_session_id,
            path_segments,
            used_provider_session_ids,
            depth + 1,
            out,
        )?;
        path_segments.pop();
    }
    Ok(())
}

struct TrajectoryEmitContext<'a> {
    source: &'a SourceKey,
    document_key: &'a str,
    session_id: StableEntityId,
    provider_session_id: &'a str,
    parent_session_id: Option<StableEntityId>,
    root_session_id: Option<StableEntityId>,
    session_relationship: Option<ProviderNativeSessionRelationship>,
    agent_scope: Option<AgentScope>,
    inherited_session_id: Option<&'a str>,
    is_embedded_subagent: bool,
    embedded_subagent_index: Option<usize>,
}

fn emit_single_trajectory(
    trajectory: &Trajectory,
    ctx: &TrajectoryEmitContext<'_>,
    out: &mut Vec<CoreRecord>,
) -> Result<(), AtifProjectionError> {
    let mut header = Some(StoredAtifTrajectoryHeader {
        schema_version: trajectory.schema_version,
        session_id: trajectory.session_id.clone(),
        inherited_session_id: ctx.inherited_session_id.map(str::to_owned),
        trajectory_id: trajectory.trajectory_id.clone(),
        agent: trajectory.agent.clone(),
        notes: trajectory.notes.clone(),
        final_metrics: trajectory.final_metrics.clone(),
        continued_trajectory_ref: trajectory.continued_trajectory_ref.clone(),
        extra: trajectory.extra.clone(),
        is_embedded_subagent: ctx.is_embedded_subagent.then_some(true),
        embedded_subagent_index: ctx.embedded_subagent_index,
    });
    let session_cwd = extract_session_cwd(trajectory);
    let mut event_sequence = 0_u64;

    for step in &trajectory.steps {
        let occurred_at_unix_ms = step
            .timestamp
            .as_deref()
            .and_then(|raw| parse_iso8601_timestamp(raw).ok())
            .map(|ts| ts.timestamp_millis());

        let tool_calls = step.tool_calls.as_deref().unwrap_or(&[]);
        let obs_results = step
            .observation
            .as_ref()
            .map(|obs| obs.results.as_slice())
            .unwrap_or(&[]);

        let has_message_text = !step.message.is_empty();
        let has_reasoning = step
            .reasoning_content
            .as_deref()
            .is_some_and(|text| !text.is_empty());
        let emit_message_record =
            has_message_text || has_reasoning || (tool_calls.is_empty() && obs_results.is_empty());

        let mut pending_empty_step_meta =
            (!emit_message_record).then(|| StoredAtifEmptyMessageStep {
                message: step.message.clone(),
                model_name: step.model_name.clone(),
                reasoning_effort: step.reasoning_effort.clone(),
                reasoning_content: step.reasoning_content.clone(),
                metrics: step.metrics.clone(),
                is_copied_context: step.is_copied_context,
                llm_call_count: step.llm_call_count,
                extra: step.extra.clone(),
            });

        if emit_message_record {
            let body = build_step_message_body(step);
            let role = match step.source {
                StepSource::System => EventRole::System,
                StepSource::User => EventRole::User,
                StepSource::Agent => EventRole::Assistant,
            };
            let envelope = StoredAtifEnvelope {
                atif: StoredAtifRecord {
                    format_version: ATIF_STORED_METADATA_VERSION,
                    trajectory: header.take(),
                    step_id: step.step_id,
                    step_source: step.source,
                    step_timestamp: step.timestamp.clone(),
                    kind: StoredAtifRecordKind::StepMessage {
                        message: step.message.clone(),
                        model_name: step.model_name.clone(),
                        reasoning_effort: step.reasoning_effort.clone(),
                        reasoning_content: step.reasoning_content.clone(),
                        metrics: step.metrics.clone(),
                        is_copied_context: step.is_copied_context,
                        llm_call_count: step.llm_call_count,
                        extra: step.extra.clone(),
                    },
                },
            };
            let activity = session_cwd.as_deref().and_then(cwd_only_activity);
            let record = build_core_record(
                ctx,
                step.step_id,
                "message",
                0,
                event_sequence,
                occurred_at_unix_ms,
                EventType::Message,
                Some(role),
                body,
                serde_json::to_value(envelope)?,
                activity,
            )?;
            event_sequence = event_sequence.saturating_add(1);
            out.push(record);
        }

        for (tc_index, call) in tool_calls.iter().enumerate() {
            let args_value = Value::Object(call.arguments.clone());
            let args_json = serde_json::to_string(&args_value)?;
            let body = format!("{} {}", call.function_name, args_json);
            let envelope = StoredAtifEnvelope {
                atif: StoredAtifRecord {
                    format_version: ATIF_STORED_METADATA_VERSION,
                    trajectory: header.take(),
                    step_id: step.step_id,
                    step_source: step.source,
                    step_timestamp: step.timestamp.clone(),
                    kind: StoredAtifRecordKind::ToolCall {
                        tool_call_index: tc_index,
                        tool_call: call.clone(),
                        step_metadata: pending_empty_step_meta.take(),
                    },
                },
            };
            let mut facts = Vec::new();
            push_cwd_fact(&mut facts, session_cwd.as_deref());
            push_tool_call_facts(&mut facts, call);
            let activity = build_tool_call_activity(call, occurred_at_unix_ms, facts);
            let record = build_core_record(
                ctx,
                step.step_id,
                "tool_call",
                u64::try_from(tc_index).unwrap_or(u64::MAX),
                event_sequence,
                occurred_at_unix_ms,
                EventType::ToolCall,
                Some(EventRole::Assistant),
                body,
                serde_json::to_value(envelope)?,
                activity,
            )?;
            event_sequence = event_sequence.saturating_add(1);
            out.push(record);
        }

        for (res_index, result) in obs_results.iter().enumerate() {
            let body = build_observation_result_body(result);
            let role = if step.source == StepSource::System {
                EventRole::System
            } else {
                EventRole::Tool
            };
            let envelope = StoredAtifEnvelope {
                atif: StoredAtifRecord {
                    format_version: ATIF_STORED_METADATA_VERSION,
                    trajectory: header.take(),
                    step_id: step.step_id,
                    step_source: step.source,
                    step_timestamp: step.timestamp.clone(),
                    kind: StoredAtifRecordKind::ObservationResult {
                        result_index: res_index,
                        result: result.clone(),
                        step_metadata: pending_empty_step_meta.take(),
                    },
                },
            };
            let mut facts = Vec::new();
            push_cwd_fact(&mut facts, session_cwd.as_deref());
            let activity =
                build_observation_result_activity(result, &body, occurred_at_unix_ms, facts);
            let record = build_core_record(
                ctx,
                step.step_id,
                "observation_result",
                u64::try_from(res_index).unwrap_or(u64::MAX),
                event_sequence,
                occurred_at_unix_ms,
                EventType::ToolOutput,
                Some(role),
                body,
                serde_json::to_value(envelope)?,
                activity,
            )?;
            event_sequence = event_sequence.saturating_add(1);
            out.push(record);
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_core_record(
    ctx: &TrajectoryEmitContext<'_>,
    step_id: u64,
    sub_kind: &str,
    sub_index: u64,
    event_sequence: u64,
    occurred_at_unix_ms: Option<i64>,
    event_type: EventType,
    role: Option<EventRole>,
    normalized_body: String,
    structured_content: Value,
    activity: Option<CoreActivity>,
) -> Result<CoreRecord, AtifProjectionError> {
    let event_key = TypedKey::composite(vec![
        TypedKey::U64(step_id),
        TypedKey::utf8(sub_kind)?,
        TypedKey::U64(sub_index),
    ])?;
    let native_item_key = NativeItemKey::native_id(ATIF_NATIVE_EVENT_NAMESPACE, event_key)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: ctx.source,
        session_id: ctx.session_id,
        logical_item_kind: ATIF_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(ctx.document_key)?,
        TypedKey::utf8(ctx.provider_session_id)?,
        TypedKey::U64(step_id),
        TypedKey::utf8(sub_kind)?,
        TypedKey::U64(sub_index),
    ])?;

    let bounded_body = truncate_utf8_to_bytes(&normalized_body, MAX_CORE_CONTENT_BYTES / 2);
    let mut record = CoreRecord {
        record_version: CORE_RECORD_VERSION,
        event_id,
        session_id: ctx.session_id,
        parent_session_id: ctx.parent_session_id,
        root_session_id: ctx.root_session_id,
        session_relationship: ctx.session_relationship,
        event_copy: None,
        source: ctx.source.clone(),
        provider_session_id: Some(ctx.provider_session_id.to_owned()),
        native_event_id: Some(native_event_id),
        event_sequence,
        occurred_at_unix_ms,
        event_type: event_type.as_str().to_owned(),
        role: role.map(|value| value.as_str().to_owned()),
        agent_scope: ctx.agent_scope,
        parser_revision: ATIF_PARSER_REVISION.to_owned(),
        normalization_revision: CORE_NORMALIZATION_REVISION,
        content: CoreContent {
            policy_revision: CORE_CONTENT_POLICY_REVISION,
            policy_status: CoreContentPolicyStatus::Selected,
            normalized_body: Some(bounded_body),
            structured_content: Some(structured_content),
            discovery_exclusion: None,
            activity,
        },
    };
    let _ = record
        .content
        .omit_provider_declared_facts_if_aggregate_exceeds_limit()?;
    let _ = record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    if record.content.structured_content.is_none()
        && record.content.activity.is_none()
        && record
            .content
            .normalized_body
            .as_deref()
            .is_none_or(str::is_empty)
    {
        record.content.normalized_body = Some("[atif step]".to_owned());
    }
    record.validate_contract()?;
    Ok(record)
}

fn build_step_message_body(step: &Step) -> String {
    let message_text = step.message.to_searchable_text();
    match step.reasoning_content.as_deref() {
        Some(reasoning) if !reasoning.is_empty() && !message_text.is_empty() => {
            format!("{message_text}\n\n[reasoning]\n{reasoning}")
        }
        Some(reasoning) if !reasoning.is_empty() => reasoning.to_owned(),
        _ => message_text,
    }
}

fn build_observation_result_body(result: &ObservationResult) -> String {
    let mut parts = Vec::new();
    if let Some(content) = result.content.as_ref() {
        let text = content.to_searchable_text();
        if !text.is_empty() {
            parts.push(text);
        }
    }
    if let Some(refs) = result.subagent_trajectory_ref.as_ref() {
        for sub_ref in refs {
            let target = sub_ref
                .trajectory_id
                .as_deref()
                .or(sub_ref.trajectory_path.as_deref())
                .unwrap_or("unknown");
            parts.push(format!("[subagent: {target}]"));
        }
    }
    parts.join("\n")
}

fn build_tool_call_activity(
    call: &ToolCall,
    started_at_unix_ms: Option<i64>,
    facts: Vec<ProviderDeclaredFact>,
) -> Option<CoreActivity> {
    let call_id = admit_optional_provider_call_id(Some(call.tool_call_id.clone()));
    if let Some(provider_call_id) = call_id {
        let args_value = Value::Object(call.arguments.clone());
        let args_capture = match serde_json::to_vec(&args_value) {
            Ok(encoded) if encoded.len() <= MAX_CORE_CONTENT_BYTES / 4 => {
                ActivityJsonCapture::Present { value: args_value }
            }
            Ok(encoded) => ActivityJsonCapture::Omitted {
                reason: "exceeds_core_activity_argument_budget".to_owned(),
                observed_encoded_bytes: u64::try_from(encoded.len()).ok(),
            },
            Err(_) => ActivityJsonCapture::Unavailable,
        };
        Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: Some(provider_call_id),
            invocation: Some(ActivityInvocation {
                protocol: None,
                server: None,
                tool: call.function_name.clone(),
                arguments: args_capture,
                started_at_unix_ms,
            }),
            result: None,
            facts,
        })
    } else if !facts.is_empty() {
        Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: None,
            invocation: None,
            result: None,
            facts,
        })
    } else {
        None
    }
}

fn build_observation_result_activity(
    result: &ObservationResult,
    body: &str,
    completed_at_unix_ms: Option<i64>,
    facts: Vec<ProviderDeclaredFact>,
) -> Option<CoreActivity> {
    let call_id = admit_optional_provider_call_id(result.source_call_id.clone());
    if let Some(provider_call_id) = call_id {
        let text_capture = if body.is_empty() {
            ActivityTextCapture::Absent
        } else if body.len() <= MAX_CORE_CONTENT_BYTES / 4 {
            ActivityTextCapture::NormalizedBody
        } else {
            ActivityTextCapture::Omitted {
                reason: "exceeds_core_activity_result_budget".to_owned(),
                observed_bytes: u64::try_from(body.len()).ok(),
            }
        };
        Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: Some(provider_call_id),
            invocation: None,
            result: Some(ActivityResult {
                status: None,
                completed_at_unix_ms,
                duration_ns: None,
                text: text_capture,
                structured_content: ActivityJsonCapture::Absent,
            }),
            facts,
        })
    } else if !facts.is_empty() {
        Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: None,
            invocation: None,
            result: None,
            facts,
        })
    } else {
        None
    }
}

fn cwd_only_activity(cwd: &str) -> Option<CoreActivity> {
    let fact = admit_provider_declared_fact(LiteralFactKind::SessionCwd, cwd.to_owned(), 0)?;
    Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: vec![fact],
    })
}

fn push_cwd_fact(facts: &mut Vec<ProviderDeclaredFact>, cwd: Option<&str>) {
    if let Some(cwd) = cwd {
        if let Some(fact) =
            admit_provider_declared_fact(LiteralFactKind::SessionCwd, cwd.to_owned(), facts.len())
        {
            facts.push(fact);
        }
    }
}

fn push_tool_call_facts(facts: &mut Vec<ProviderDeclaredFact>, call: &ToolCall) {
    for key in ["path", "file_path", "filepath", "filename"] {
        if let Some(value) = call.arguments.get(key).and_then(Value::as_str) {
            if let Some(fact) =
                admit_provider_declared_fact(LiteralFactKind::File, value.to_owned(), facts.len())
            {
                facts.push(fact);
            }
        }
    }
    for key in ["command", "cmd"] {
        if let Some(value) = call.arguments.get(key).and_then(Value::as_str) {
            if let Some(fact) = admit_provider_declared_fact(
                LiteralFactKind::Command,
                value.to_owned(),
                facts.len(),
            ) {
                facts.push(fact);
            }
        }
    }
}

fn extract_session_cwd(trajectory: &Trajectory) -> Option<String> {
    for map in [trajectory.extra.as_ref(), trajectory.agent.extra.as_ref()]
        .into_iter()
        .flatten()
    {
        for key in ["cwd", "working_directory", "workspace"] {
            if let Some(value) = map.get(key).and_then(Value::as_str) {
                if !value.trim().is_empty() {
                    return Some(value.to_owned());
                }
            }
        }
    }
    None
}

fn truncate_utf8_to_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
