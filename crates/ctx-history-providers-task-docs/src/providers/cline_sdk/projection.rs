use std::collections::HashMap;

use chrono::{DateTime, TimeZone, Utc};
use ctx_history_core::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, AgentScope, CaptureProvider, CoreActivity, CoreRecord, CoreRecordError,
    EventIdentityInput, EventRole, EventType, LiteralFactKind, NativeItemKey, NativeSessionKey,
    ProjectionContractError, ProviderNativeSessionRelationship, SessionIdentityInput,
    SourceAnchorScope, SourceKey, StableEntityId, SubrecordSelector, TypedKey,
    CORE_ACTIVITY_REVISION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::source::{validate_json_bounds, ClineSdkError, SessionLeaf, SessionMetadata};
use crate::CLINE_SDK_SOURCE_FORMAT;

const SOURCE_NAMESPACE: &str = "cline.sdk.session";
const SESSION_NAMESPACE: &str = "cline.sdk.session";
const MESSAGE_NAMESPACE: &str = "cline.sdk.message";
const LOGICAL_SESSION_KIND: &str = "cline-sdk-session";
const LOGICAL_EVENT_KIND: &str = "cline-sdk-event";
const SCHEMA_VARIANT: &str = "cline-sdk-session-store-v1";
pub(super) const PARSER_REVISION: &str = "cline-sdk-source-backed-v1";
const MAX_MESSAGES: usize = 65_536;
const MAX_BLOCKS_PER_MESSAGE: usize = 4_096;
const EVENT_STRIDE: u64 = MAX_BLOCKS_PER_MESSAGE as u64 + 1;

#[derive(Debug, Error)]
pub(super) enum ProjectionError {
    #[error(transparent)]
    Source(#[from] ClineSdkError),
    #[error(transparent)]
    Identity(#[from] ProjectionContractError),
    #[error(transparent)]
    Core(#[from] CoreRecordError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid Cline messages artifact: {0}")]
    Invalid(String),
    #[error("Cline message coordinate overflowed")]
    CoordinateOverflow,
}

pub(super) type Result<T> = std::result::Result<T, ProjectionError>;

pub(super) struct ProjectedDocument {
    pub(super) records: Vec<CoreRecord>,
    pub(super) rejected: u64,
    pub(super) ignored: u64,
}

#[cfg(test)]
pub(super) fn cline_source_key(provider_session_id: &str) -> Result<SourceKey> {
    cline_source_key_scoped(provider_session_id, SourceAnchorScope::Unqualified)
}

pub(super) fn cline_source_key_scoped(
    provider_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<SourceKey> {
    Ok(SourceKey::derive_provider_native_scoped(
        CaptureProvider::Cline.as_str(),
        CLINE_SDK_SOURCE_FORMAT,
        SCHEMA_VARIANT,
        1,
        SOURCE_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
        source_anchor_scope,
    )?)
}

pub(super) fn cline_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> Result<StableEntityId> {
    let native =
        NativeSessionKey::native_id(SESSION_NAMESPACE, TypedKey::utf8(provider_session_id)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native,
    })?)
}

fn provider_session_identity(
    provider_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<StableEntityId> {
    let source = cline_source_key_scoped(provider_session_id, source_anchor_scope)?;
    cline_session_id(&source, provider_session_id)
}

pub(super) fn owns_cline_source(source: &SourceKey) -> bool {
    source.provider() == CaptureProvider::Cline.as_str()
        && source.source_format() == CLINE_SDK_SOURCE_FORMAT
        && source.schema_variant() == SCHEMA_VARIANT
        && source.provider_identity_version() == 1
}

pub(super) fn project_messages(
    leaf: &SessionLeaf,
    source: &SourceKey,
    session_id: StableEntityId,
    source_anchor_scope: SourceAnchorScope,
    source_revision: [u8; 32],
    bytes: &[u8],
) -> Result<ProjectedDocument> {
    let document: Value = serde_json::from_slice(bytes)?;
    validate_json_bounds(&document, 0, &mut 0)?;
    if document.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(ProjectionError::Invalid("expected version 1".into()));
    }
    if document.get("sessionId").and_then(Value::as_str) != Some(&leaf.provider_session_id) {
        return Err(ProjectionError::Invalid(
            "sessionId does not match the catalog identity".into(),
        ));
    }
    let messages = document
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| ProjectionError::Invalid("messages must be an array".into()))?;
    if messages.len() > MAX_MESSAGES {
        return Err(ProjectionError::Invalid(format!(
            "messages exceeds the {MAX_MESSAGES} entry limit"
        )));
    }

    let parent_provider_session_id = leaf
        .metadata
        .parent_session_id
        .clone()
        .or_else(|| exact_string(document.pointer("/origin/parentThreadId")));
    let parent_session_id = parent_provider_session_id
        .as_deref()
        .map(|provider_session_id| {
            provider_session_identity(provider_session_id, source_anchor_scope)
        })
        .transpose()?;
    let agent = document.get("agent").and_then(Value::as_str);
    let agent_scope = explicit_agent_scope(&leaf.metadata, agent);
    let relationship = explicit_relationship(&leaf.metadata, parent_session_id, agent_scope);
    let fallback_timestamp = document
        .get("updated_at")
        .and_then(timestamp_value)
        .or_else(|| parse_timestamp_text(leaf.metadata.updated_at.as_deref()))
        .or_else(|| parse_timestamp_text(leaf.metadata.started_at.as_deref()));

    let mut records = Vec::new();
    let mut ignored = 0_u64;
    let mut rejected = u64::from(leaf.metadata.malformed_manifest);
    if let Some(system_prompt) = document.get("system_prompt") {
        if let Some(body) = lexical_value(system_prompt).filter(|body| !body.trim().is_empty()) {
            records.push(core_record(
                RecordContext {
                    leaf,
                    source,
                    session_id,
                    source_revision,
                    parent_provider_session_id: parent_provider_session_id.as_deref(),
                    parent_session_id,
                    relationship,
                    agent_scope,
                    session_agent: agent,
                },
                RecordProjection {
                    native_item_key: NativeItemKey::native_id(
                        MESSAGE_NAMESPACE,
                        TypedKey::utf8("system-prompt")?,
                    )?,
                    selector: None,
                    native_event_id: TypedKey::utf8("system-prompt")?,
                    event_sequence: 0,
                    occurred_at: fallback_timestamp,
                    event_type: EventType::Notice,
                    role: EventRole::System,
                    body,
                    message: system_prompt,
                    block: system_prompt,
                    activity: None,
                },
            )?);
        } else {
            ignored = checked_add(ignored, 1)?;
        }
    }

    let mut message_occurrences = HashMap::<String, u64>::new();
    let mut digest_occurrences = HashMap::<[u8; 32], u64>::new();
    for (message_index, message) in messages.iter().enumerate() {
        let Some(message_object) = message.as_object() else {
            rejected = checked_add(rejected, 1)?;
            continue;
        };
        let (native_item_key, message_coordinate) =
            message_native_identity(message, &mut message_occurrences, &mut digest_occurrences)?;
        let role = message_object
            .get("role")
            .and_then(Value::as_str)
            .map(event_role)
            .unwrap_or(EventRole::Unknown);
        let occurred_at = message_object
            .get("ts")
            .and_then(timestamp_value)
            .or_else(|| message_object.get("timestamp").and_then(timestamp_value))
            .or(fallback_timestamp);
        let Some(content) = message_object.get("content") else {
            rejected = checked_add(rejected, 1)?;
            continue;
        };
        let blocks = message_blocks(content)?;
        if blocks.len() > MAX_BLOCKS_PER_MESSAGE {
            return Err(ProjectionError::Invalid(format!(
                "one message exceeds the {MAX_BLOCKS_PER_MESSAGE} block limit"
            )));
        }
        let mut kind_occurrences = HashMap::<String, u64>::new();
        let mut retained_from_message = 0_u64;
        for (block_index, block) in blocks.into_iter().enumerate() {
            let Some(projected) = project_block(block, role) else {
                ignored = checked_add(ignored, 1)?;
                continue;
            };
            if projected.body.trim().is_empty() {
                ignored = checked_add(ignored, 1)?;
                continue;
            }
            let occurrence = kind_occurrences
                .entry(projected.kind.to_owned())
                .or_default();
            let selector = if let Some(native_id) = projected.native_selector_id.as_deref() {
                SubrecordSelector::native_id(
                    format!("cline.sdk.{}", projected.kind),
                    TypedKey::utf8(native_id)?,
                )?
            } else {
                SubrecordSelector::composite(
                    "cline.sdk.block",
                    vec![TypedKey::utf8(projected.kind)?, TypedKey::U64(*occurrence)],
                )?
            };
            let sequence = u64::try_from(message_index + 1)
                .ok()
                .and_then(|value| value.checked_mul(EVENT_STRIDE))
                .and_then(|value| value.checked_add(u64::try_from(block_index).ok()?))
                .ok_or(ProjectionError::CoordinateOverflow)?;
            let native_event_id = TypedKey::composite(vec![
                message_coordinate.clone(),
                TypedKey::utf8(projected.kind)?,
                TypedKey::U64(*occurrence),
            ])?;
            records.push(core_record(
                RecordContext {
                    leaf,
                    source,
                    session_id,
                    source_revision,
                    parent_provider_session_id: parent_provider_session_id.as_deref(),
                    parent_session_id,
                    relationship,
                    agent_scope,
                    session_agent: agent,
                },
                RecordProjection {
                    native_item_key: native_item_key.clone(),
                    selector: Some(selector),
                    native_event_id,
                    event_sequence: sequence,
                    occurred_at,
                    event_type: projected.event_type,
                    role: projected.role,
                    body: projected.body,
                    message,
                    block,
                    activity: projected.activity,
                },
            )?);
            *occurrence = occurrence
                .checked_add(1)
                .ok_or(ProjectionError::CoordinateOverflow)?;
            retained_from_message = checked_add(retained_from_message, 1)?;
        }
        if retained_from_message == 0 {
            rejected = checked_add(rejected, 1)?;
        }
    }
    Ok(ProjectedDocument {
        records,
        rejected,
        ignored,
    })
}

#[derive(Clone, Copy)]
struct RecordContext<'a> {
    leaf: &'a SessionLeaf,
    source: &'a SourceKey,
    session_id: StableEntityId,
    source_revision: [u8; 32],
    parent_provider_session_id: Option<&'a str>,
    parent_session_id: Option<StableEntityId>,
    relationship: Option<ProviderNativeSessionRelationship>,
    agent_scope: Option<AgentScope>,
    session_agent: Option<&'a str>,
}

struct RecordProjection<'a> {
    native_item_key: NativeItemKey,
    selector: Option<SubrecordSelector>,
    native_event_id: TypedKey,
    event_sequence: u64,
    occurred_at: Option<DateTime<Utc>>,
    event_type: EventType,
    role: EventRole,
    body: String,
    message: &'a Value,
    block: &'a Value,
    activity: Option<ProjectedActivity>,
}

fn core_record(context: RecordContext<'_>, projection: RecordProjection<'_>) -> Result<CoreRecord> {
    let event_id = derive_event_id(EventIdentityInput {
        source: context.source,
        session_id: context.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &projection.native_item_key,
        subrecord_selector: projection.selector.as_ref(),
    })?;
    let mut record = CoreRecord::new_selected(
        event_id,
        context.session_id,
        context.source.clone(),
        projection.event_sequence,
        projection.event_type.as_str(),
        PARSER_REVISION,
        projection.body,
    )?;
    record.parent_session_id = context.parent_session_id;
    record.session_relationship = context.relationship;
    record.agent_scope = context.agent_scope;
    record.provider_session_id = Some(context.leaf.provider_session_id.clone());
    record.native_event_id = Some(projection.native_event_id);
    record.occurred_at_unix_ms = projection.occurred_at.map(|value| value.timestamp_millis());
    record.role = Some(projection.role.as_str().to_owned());
    record.content.structured_content = Some(json!({
        "message": projection.message,
        "block": projection.block,
        "session": structured_session(
            context.leaf,
            context.session_agent,
            context.parent_provider_session_id,
        ),
        "source_revision": hex_digest(context.source_revision),
    }));

    let mut facts = Vec::new();
    if let Some(cwd) = context.leaf.metadata.cwd.clone() {
        if let Some(fact) =
            admit_provider_declared_fact(LiteralFactKind::SessionCwd, cwd, facts.len())
        {
            facts.push(fact);
        }
    }
    if let Some(workspace) = context.leaf.metadata.workspace_root.clone() {
        if let Some(fact) =
            admit_provider_declared_fact(LiteralFactKind::Workspace, workspace, facts.len())
        {
            facts.push(fact);
        }
    }
    let (provider_call_id, invocation, result) =
        projection.activity.map_or((None, None, None), |activity| {
            activity.into_core(projection.occurred_at)
        });
    if invocation.is_some() || result.is_some() || !facts.is_empty() {
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id,
            invocation,
            result,
            facts,
        });
    }
    record
        .content
        .omit_provider_declared_facts_if_aggregate_exceeds_limit()?;
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(record)
}

fn structured_session(
    leaf: &SessionLeaf,
    agent: Option<&str>,
    effective_parent_session_id: Option<&str>,
) -> Value {
    json!({
        "model": leaf.metadata.model,
        "provider": leaf.metadata.provider,
        "cwd": leaf.metadata.cwd,
        "workspace_root": leaf.metadata.workspace_root,
        "parent_session_id": leaf.metadata.parent_session_id,
        "effective_parent_session_id": effective_parent_session_id,
        "parent_agent_id": leaf.metadata.parent_agent_id,
        "agent_id": leaf.metadata.agent_id,
        "conversation_id": leaf.metadata.conversation_id,
        "is_subagent": leaf.metadata.is_subagent,
        "agent": agent,
        "index_row": leaf.metadata.index_row,
        "database_row": leaf.metadata.database_row,
        "manifest": leaf.metadata.manifest,
    })
}

struct ProjectedBlock {
    kind: &'static str,
    event_type: EventType,
    role: EventRole,
    body: String,
    native_selector_id: Option<String>,
    activity: Option<ProjectedActivity>,
}

enum ProjectedActivity {
    Invocation {
        call_id: Option<String>,
        tool: Option<String>,
        input: Option<Value>,
    },
    Result {
        call_id: Option<String>,
        content: Value,
        is_error: Option<bool>,
    },
}

impl ProjectedActivity {
    fn into_core(
        self,
        occurred_at: Option<DateTime<Utc>>,
    ) -> (
        Option<TypedKey>,
        Option<ActivityInvocation>,
        Option<ActivityResult>,
    ) {
        match self {
            Self::Invocation {
                call_id,
                tool,
                input,
            } => {
                let provider_call_id = admit_optional_provider_call_id(call_id);
                let invocation = provider_call_id.as_ref().and_then(|_| {
                    admit_optional_metadata_text(tool).map(|tool| ActivityInvocation {
                        protocol: None,
                        server: None,
                        tool,
                        arguments: input.map_or(ActivityJsonCapture::Absent, |value| {
                            ActivityJsonCapture::Present { value }
                        }),
                        started_at_unix_ms: occurred_at.map(|value| value.timestamp_millis()),
                    })
                });
                (provider_call_id, invocation, None)
            }
            Self::Result {
                call_id,
                content,
                is_error,
            } => {
                let provider_call_id = admit_optional_provider_call_id(call_id);
                let result = provider_call_id.as_ref().map(|_| ActivityResult {
                    status: is_error.map(|value| value.to_string()),
                    completed_at_unix_ms: occurred_at.map(|value| value.timestamp_millis()),
                    duration_ns: None,
                    text: ActivityTextCapture::NormalizedBody,
                    structured_content: ActivityJsonCapture::Present { value: content },
                });
                (provider_call_id, None, result)
            }
        }
    }
}

fn project_block(block: &Value, message_role: EventRole) -> Option<ProjectedBlock> {
    if let Some(text) = block.as_str() {
        return Some(ProjectedBlock {
            kind: "text",
            event_type: EventType::Message,
            role: message_role,
            body: text.to_owned(),
            native_selector_id: None,
            activity: None,
        });
    }
    let kind = block.get("type")?.as_str()?;
    match kind {
        "text" | "thinking" => Some(ProjectedBlock {
            kind: if kind == "thinking" {
                "thinking"
            } else {
                "text"
            },
            event_type: EventType::Message,
            role: if kind == "thinking" {
                EventRole::Assistant
            } else {
                message_role
            },
            body: block
                .get(if kind == "thinking" {
                    "thinking"
                } else {
                    "text"
                })
                .and_then(Value::as_str)?
                .to_owned(),
            native_selector_id: None,
            activity: None,
        }),
        "tool_use" => {
            let call_id = exact_string(block.get("id"));
            let tool = exact_string(block.get("name"));
            let input = block.get("input").cloned();
            let body = match (&tool, &input) {
                (Some(tool), Some(input)) => {
                    format!("{tool} {}", lexical_value(input).unwrap_or_default())
                }
                (Some(tool), None) => tool.clone(),
                (None, Some(input)) => lexical_value(input).unwrap_or_else(|| "tool_call".into()),
                (None, None) => "tool_call".into(),
            };
            Some(ProjectedBlock {
                kind: "tool_use",
                event_type: EventType::ToolCall,
                role: EventRole::Assistant,
                body,
                native_selector_id: call_id.clone(),
                activity: Some(ProjectedActivity::Invocation {
                    call_id,
                    tool,
                    input,
                }),
            })
        }
        "tool_result" => {
            let call_id = exact_string(block.get("tool_use_id"));
            let content = block.get("content").cloned().unwrap_or(Value::Null);
            Some(ProjectedBlock {
                kind: "tool_result",
                event_type: EventType::ToolOutput,
                role: EventRole::Tool,
                body: lexical_value(&content).unwrap_or_else(|| "tool_result".into()),
                native_selector_id: call_id.clone(),
                activity: Some(ProjectedActivity::Result {
                    call_id,
                    content,
                    is_error: block.get("is_error").and_then(Value::as_bool),
                }),
            })
        }
        _ => None,
    }
}

fn message_blocks(content: &Value) -> Result<Vec<&Value>> {
    match content {
        Value::String(_) => Ok(vec![content]),
        Value::Array(values) => Ok(values.iter().collect()),
        _ => Err(ProjectionError::Invalid(
            "message content must be a string or array".into(),
        )),
    }
}

fn message_native_identity(
    message: &Value,
    id_occurrences: &mut HashMap<String, u64>,
    digest_occurrences: &mut HashMap<[u8; 32], u64>,
) -> Result<(NativeItemKey, TypedKey)> {
    if let Some(id) = exact_string(message.get("id")) {
        let occurrence = id_occurrences.entry(id.clone()).or_default();
        let coordinate =
            TypedKey::composite(vec![TypedKey::utf8(&id)?, TypedKey::U64(*occurrence)])?;
        let key = NativeItemKey::composite(
            MESSAGE_NAMESPACE,
            vec![TypedKey::utf8(&id)?, TypedKey::U64(*occurrence)],
        )?;
        *occurrence = occurrence
            .checked_add(1)
            .ok_or(ProjectionError::CoordinateOverflow)?;
        return Ok((key, coordinate));
    }
    let encoded = serde_json::to_vec(message)?;
    let digest: [u8; 32] = Sha256::digest(encoded).into();
    let occurrence = digest_occurrences.entry(digest).or_default();
    let coordinate = TypedKey::composite(vec![
        TypedKey::bytes(digest.to_vec())?,
        TypedKey::U64(*occurrence),
    ])?;
    let key = NativeItemKey::composite(
        MESSAGE_NAMESPACE,
        vec![
            TypedKey::bytes(digest.to_vec())?,
            TypedKey::U64(*occurrence),
        ],
    )?;
    *occurrence = occurrence
        .checked_add(1)
        .ok_or(ProjectionError::CoordinateOverflow)?;
    Ok((key, coordinate))
}

fn explicit_agent_scope(metadata: &SessionMetadata, agent: Option<&str>) -> Option<AgentScope> {
    metadata
        .is_subagent
        .map(|value| {
            if value {
                AgentScope::Subagent
            } else {
                AgentScope::Primary
            }
        })
        .or_else(|| match agent {
            Some("lead") => Some(AgentScope::Primary),
            Some("subagent" | "teammate") => Some(AgentScope::Subagent),
            _ if metadata.parent_agent_id.is_some() => Some(AgentScope::Subagent),
            _ => None,
        })
}

fn explicit_relationship(
    metadata: &SessionMetadata,
    parent_session_id: Option<StableEntityId>,
    agent_scope: Option<AgentScope>,
) -> Option<ProviderNativeSessionRelationship> {
    if parent_session_id.is_some() {
        Some(if metadata.fork_parent {
            ProviderNativeSessionRelationship::Forked
        } else {
            ProviderNativeSessionRelationship::Delegated
        })
    } else if agent_scope == Some(AgentScope::Primary) {
        Some(ProviderNativeSessionRelationship::Root)
    } else {
        None
    }
}

fn event_role(value: &str) -> EventRole {
    match value {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "system" => EventRole::System,
        "tool" => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

fn timestamp_value(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::Number(number) => number.as_i64().and_then(timestamp_integer),
        Value::String(value) => parse_timestamp_text(Some(value)),
        _ => None,
    }
}

fn timestamp_integer(value: i64) -> Option<DateTime<Utc>> {
    if value.unsigned_abs() >= 10_000_000_000 {
        Utc.timestamp_millis_opt(value).single()
    } else {
        Utc.timestamp_opt(value, 0).single()
    }
}

fn parse_timestamp_text(value: Option<&str>) -> Option<DateTime<Utc>> {
    let value = value?;
    value
        .parse::<i64>()
        .ok()
        .and_then(timestamp_integer)
        .or_else(|| {
            DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|value| value.with_timezone(&Utc))
        })
}

fn lexical_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Array(values) => {
            let text = values
                .iter()
                .filter_map(|value| {
                    value
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| value.as_str().map(str::to_owned))
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty())
                .then_some(text)
                .or_else(|| serde_json::to_string(value).ok())
        }
        Value::Null => None,
        _ => serde_json::to_string(value).ok(),
    }
}

fn exact_string(value: Option<&Value>) -> Option<String> {
    value?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn checked_add(left: u64, right: u64) -> Result<u64> {
    left.checked_add(right)
        .ok_or(ProjectionError::CoordinateOverflow)
}

fn hex_digest(value: [u8; 32]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}
