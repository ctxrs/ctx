use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, EventIdentityInput,
    EventOrigin, NativeItemKey, NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey,
    StableEntityId, TypedKey,
};

use super::{
    GeminiSourceBackedError, GeminiSourceBackedResult, GEMINI_LOGICAL_EVENT_KIND,
    GEMINI_LOGICAL_SESSION_KIND, GEMINI_NATIVE_EVENT_NAMESPACE, GEMINI_NATIVE_SESSION_NAMESPACE,
    GEMINI_SOURCE_ANCHOR_NAMESPACE, GEMINI_SOURCE_BACKED_PARSER_REVISION,
    GEMINI_SOURCE_SCHEMA_VARIANT, MAX_GEMINI_LEXICAL_METADATA_CHARS,
};
use crate::{CaptureError, GEMINI_CLI_SOURCE_FORMAT};

pub(super) struct GeminiProjectedContent {
    pub(super) annotation: ctx_history_core::CoreRecordAnnotation,
    pub(super) discovery_exclusion: Option<ctx_history_core::CoreDiscoveryExclusion>,
}

pub(super) fn project_event(
    source: &SourceKey,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    session: &super::super::GeminiSession,
    event: super::super::GeminiRetainedEvent,
    content: GeminiProjectedContent,
) -> GeminiSourceBackedResult<CoreRecord> {
    let super::super::GeminiEventIdentity::NativeRecordId(native_event_id) = &event.identity;
    let event_id = gemini_event_id(source, session_id, &event)?;
    let native_event_id = TypedKey::utf8(native_event_id)?;
    let event_sequence = event
        .native_order
        .raw_ordinal
        .checked_mul(u64::from(u32::MAX) + 1)
        .and_then(|sequence| sequence.checked_add(u64::from(event.native_order.sub_ordinal)))
        .ok_or_else(|| {
            GeminiSourceBackedError::Capture(CaptureError::SystemInvariant(
                "Gemini event sequence overflowed",
            ))
        })?;
    let body = lexical_body(&event);
    if body.is_empty() {
        return Err(CaptureError::InvalidPayload(
            "Gemini source-backed event has no lexical body".to_owned(),
        )
        .into());
    }
    let is_primary =
        session.parent_native_session_id.is_none() && session.agent_type != AgentType::Subagent;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        event_sequence,
        event.event_type.as_str(),
        session.agent_type.as_str(),
        true,
        GEMINI_SOURCE_BACKED_PARSER_REVISION,
        body,
    )?;
    if let Some(parent_session_id) = parent_session_id {
        let kind = if is_primary {
            ctx_history_core::SessionRelationshipKind::RelatedUnknown
        } else {
            ctx_history_core::SessionRelationshipKind::Delegated
        };
        record.set_session_relationship(kind, Some(parent_session_id), root_session_id)?;
        if kind == ctx_history_core::SessionRelationshipKind::Delegated {
            record.event_origin = EventOrigin::UniqueToSession;
        }
    }
    record.provider_session_id = Some(session.native_session_id.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = event
        .occurred_at
        .or(session.started_at)
        .map(|timestamp| timestamp.timestamp_millis());
    record.role = Some(event.role.as_str().to_owned());
    record.cwd = session
        .cwd
        .as_deref()
        .map(|cwd| bounded_chars(cwd, MAX_GEMINI_LEXICAL_METADATA_CHARS));
    record.content.structured_content = content.annotation.structured_content;
    record.content.discovery_exclusion = content.discovery_exclusion;
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.metadata = content.annotation.metadata;
    record.repository_candidate_evidence = content.annotation.repository_candidate_evidence;
    record.repository_bindings = content.annotation.repository_bindings;
    record.repository_abstentions = content.annotation.repository_abstentions;
    record.repository_file_invocation_evidence =
        content.annotation.repository_file_invocation_evidence;
    record.repository_file_observations = content.annotation.repository_file_observations;
    record.repository_vcs_observations = content.annotation.repository_vcs_observations;
    record.validate_contract()?;
    Ok(record)
}

pub(super) fn gemini_event_id(
    source: &SourceKey,
    session_id: StableEntityId,
    event: &super::super::GeminiRetainedEvent,
) -> GeminiSourceBackedResult<StableEntityId> {
    let super::super::GeminiEventIdentity::NativeRecordId(native_event_id) = &event.identity;
    let native_item_key = NativeItemKey::native_id(
        GEMINI_NATIVE_EVENT_NAMESPACE,
        TypedKey::utf8(native_event_id)?,
    )?;
    Ok(derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: GEMINI_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?)
}

fn lexical_body(event: &super::super::GeminiRetainedEvent) -> String {
    if !event.searchable_text.is_empty() {
        return event.searchable_text.clone();
    }
    match &event.body {
        super::super::dto::GeminiEventBody::Message { text, .. } => text.clone(),
        super::super::dto::GeminiEventBody::ToolCall { .. } => "Gemini tool call".to_owned(),
        super::super::dto::GeminiEventBody::OutputDiagnostic {
            result,
            call_id,
            tool_name,
            outcome,
            exit_code,
            duration_ms,
            ..
        } => result
            .as_ref()
            .and_then(|value| match value {
                serde_json::Value::String(text) if !text.is_empty() => Some(text.clone()),
                serde_json::Value::String(_) => None,
                value => serde_json::to_string(value).ok(),
            })
            .unwrap_or_else(|| {
                format!(
                    "Gemini {} output {}{}{}{}",
                    tool_name.as_deref().unwrap_or("tool"),
                    outcome,
                    call_id
                        .as_deref()
                        .map(|call| format!(", call {call}"))
                        .unwrap_or_default(),
                    exit_code
                        .map(|code| format!(", exit code {code}"))
                        .unwrap_or_default(),
                    duration_ms
                        .map(|duration| format!(", duration {duration} ms"))
                        .unwrap_or_default(),
                )
            }),
        super::super::dto::GeminiEventBody::StateNotice { summary } => summary
            .as_deref()
            .filter(|summary| !summary.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "Gemini state update".to_owned()),
        super::super::dto::GeminiEventBody::RewindNotice {
            target_native_record_id,
        } => format!("Gemini rewind to {target_native_record_id}"),
    }
}

fn bounded_chars(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

pub(super) fn gemini_source_key(native_session_id: &str) -> GeminiSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        GEMINI_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Gemini.as_str(),
        GEMINI_CLI_SOURCE_FORMAT,
        GEMINI_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

pub(super) fn gemini_session_id(
    source: &SourceKey,
    native_session_id: &str,
) -> GeminiSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        GEMINI_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: GEMINI_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}
