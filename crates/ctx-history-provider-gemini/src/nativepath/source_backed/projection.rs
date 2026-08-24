use ctx_history_core::{
    derive_event_id, derive_native_session_id, AgentScope, CaptureProvider, CoreRecord,
    EventIdentityInput, NativeItemKey, ProviderNativeSessionRelationship, SourceKey,
    StableEntityId, TypedKey, MAX_CORE_CONTENT_BYTES,
};
use ctx_history_jsonl::{fit_jsonl_activity, selected_content_fits, JsonlActivityObservedBytes};

use super::{
    GeminiSourceBackedError, GeminiSourceBackedResult, GEMINI_LOGICAL_EVENT_KIND,
    GEMINI_LOGICAL_SESSION_KIND, GEMINI_NATIVE_EVENT_NAMESPACE, GEMINI_NATIVE_SESSION_NAMESPACE,
    GEMINI_SOURCE_ANCHOR_NAMESPACE, GEMINI_SOURCE_SCHEMA_VARIANT,
};
use crate::{GeminiError, GEMINI_CLI_SOURCE_FORMAT};

pub(super) struct GeminiProjectedContent {
    pub(super) annotation: ctx_history_core::CoreRecordAnnotation,
}

pub(super) fn project_event(
    source: &SourceKey,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    parser_revision: &str,
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
        .ok_or({
            GeminiSourceBackedError::Gemini(GeminiError::SystemInvariant(
                "Gemini event sequence overflowed",
            ))
        })?;
    let body = lexical_body(&event);
    if body.is_empty() {
        return Err(GeminiError::InvalidPayload(
            "Gemini source-backed event has no lexical body".to_owned(),
        )
        .into());
    }
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        event_sequence,
        event.event_type.as_str(),
        parser_revision,
        body,
    )?;
    if let Some(parent_session_id) = parent_session_id {
        record.parent_session_id = Some(parent_session_id);
        if session.agent_scope == AgentScope::Subagent {
            record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
        }
    }
    record.provider_session_id = Some(session.native_session_id.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = event
        .occurred_at
        .or(session.started_at)
        .map(|timestamp| timestamp.timestamp_millis());
    record.role = Some(event.role.as_str().to_owned());
    record.agent_scope = Some(session.agent_scope);
    let mut structured_content = content.annotation.structured_content;
    let mut activity = content.annotation.activity;
    if !selected_content_fits(
        record
            .content
            .normalized_body
            .as_deref()
            .unwrap_or_default(),
        structured_content.as_ref(),
        activity.as_ref(),
        ctx_history_core::MAX_CORE_CONTENT_BYTES,
    ) {
        structured_content = None;
    }
    fit_jsonl_activity(
        record
            .content
            .normalized_body
            .as_deref()
            .unwrap_or_default(),
        structured_content.as_ref(),
        &mut activity,
        JsonlActivityObservedBytes::infer_from_present(),
        ctx_history_core::MAX_CORE_CONTENT_BYTES,
    );
    record.content.structured_content = structured_content;
    record.content.activity = activity;
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
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
        super::super::dto::GeminiEventBody::ToolResult {
            result: Some(serde_json::Value::String(text)),
            ..
        } if text.len() <= MAX_CORE_CONTENT_BYTES => text.clone(),
        super::super::dto::GeminiEventBody::ToolResult { .. } => "Gemini tool result".to_owned(),
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

pub(super) fn gemini_source_key(
    session: &super::super::GeminiSession,
) -> GeminiSourceBackedResult<SourceKey> {
    let anchor = TypedKey::composite(vec![
        TypedKey::utf8(&session.native_session_id)?,
        exact_optional_text(session.native_start_time.as_deref())?,
        exact_optional_text(session.project_hash.as_deref())?,
        exact_optional_text(session.native_kind.as_deref())?,
    ])?;
    Ok(SourceKey::derive_provider_native(
        CaptureProvider::Gemini.as_str(),
        GEMINI_CLI_SOURCE_FORMAT,
        GEMINI_SOURCE_SCHEMA_VARIANT,
        super::GEMINI_SOURCE_IDENTITY_VERSION,
        GEMINI_SOURCE_ANCHOR_NAMESPACE,
        anchor,
    )?)
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn gemini_legacy_v1_source_key(
    native_session_id: &str,
) -> GeminiSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive_provider_native(
        CaptureProvider::Gemini.as_str(),
        GEMINI_CLI_SOURCE_FORMAT,
        GEMINI_SOURCE_SCHEMA_VARIANT,
        1,
        GEMINI_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?)
}

fn exact_optional_text(value: Option<&str>) -> GeminiSourceBackedResult<TypedKey> {
    value
        .map(TypedKey::utf8)
        .transpose()
        .map(|value| value.unwrap_or(TypedKey::Null))
        .map_err(Into::into)
}

pub(super) fn gemini_session_id(
    source: &SourceKey,
    native_session_id: &str,
) -> GeminiSourceBackedResult<StableEntityId> {
    Ok(derive_native_session_id(
        source,
        GEMINI_LOGICAL_SESSION_KIND,
        GEMINI_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?)
}
