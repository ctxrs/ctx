use chrono::{DateTime, NaiveDateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_json_text, NativeEventDraft, NativeSessionDraft,
};
use crate::{ProviderAdapterContext, ProviderNormalizationResult, SHELLEY_SQLITE_SOURCE_FORMAT};

use super::relationships::{
    shelley_event_index, shelley_event_role, shelley_event_type, shelley_message_body,
    shelley_message_complete_text, shelley_message_text, ShelleyConversationRow, ShelleyMessageRow,
};

pub(super) fn shelley_message_normalization(
    message: ShelleyMessageRow,
    conversation: &ShelleyConversationRow,
    raw_source_path: &str,
    user_version: i64,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
    parent_bearing: bool,
) -> crate::Result<ProviderNormalizationResult> {
    let mut result = ProviderNormalizationResult::default();
    let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
    let ended_at = conversation
        .updated_at
        .as_deref()
        .map(|timestamp| shelley_timestamp(Some(timestamp), context.imported_at));
    let occurred_at = shelley_timestamp(message.created_at.as_deref(), started_at);
    let mut event = shelley_complete_event(&message, conversation, occurred_at);
    let needs_locator = matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) || (event.event_type == EventType::Message
        && event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            == Some(true));
    if needs_locator {
        let complete_text = shelley_message_complete_text(&message)
            .unwrap_or_else(|| format!("Shelley {} message", message.entry_type));
        crate::complete_content::sqlite::attach_shelley_content_locator(
            &mut event,
            &message,
            conversation,
            parent_bearing,
            &complete_text,
        )?;
    }
    result.captures.push((
        message.rowid.max(0) as usize,
        shelley_capture(
            ShelleyCaptureDraft {
                conversation,
                started_at,
                ended_at,
                raw_source_path,
                user_version,
                schema_fingerprint,
                event: Some(event),
            },
            context,
        ),
    ));
    Ok(result)
}

pub(crate) fn shelley_complete_event(
    message: &ShelleyMessageRow,
    conversation: &ShelleyConversationRow,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let body = shelley_message_body(message);
    let text = shelley_message_text(message, &body)
        .unwrap_or_else(|| format!("Shelley {} message", message.entry_type));
    let event_type = shelley_event_type(message, &body);
    let role = shelley_event_role(&message.entry_type);
    native_event(NativeEventDraft {
        provider: CaptureProvider::Shelley,
        source_format: SHELLEY_SQLITE_SOURCE_FORMAT,
        provider_session_id: conversation.conversation_id.clone(),
        provider_event_index: shelley_event_index(message),
        provider_event_hash: Some(message.message_id.clone()),
        cursor: format!(
            "conversation:{}:sequence:{}:message:{}",
            message.conversation_id, message.sequence_id, message.message_id
        ),
        event_type,
        role,
        occurred_at,
        text,
        body,
        metadata: json!({
            "source": "shelley_messages",
            "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
            "message_id": message.message_id,
            "conversation_id": message.conversation_id,
            "sequence_id": message.sequence_id,
            "rowid": message.rowid,
            "message_type": message.entry_type,
            "generation": message.generation,
            "excluded_from_context": message.excluded_from_context,
            "usage": message.usage_data.as_deref().map(provider_json_text),
            "llm_api_url": message.llm_api_url,
            "model_name": message.model_name,
            "forked_from_message_id": message.forked_from_message_id,
        }),
    })
}

pub(super) fn shelley_empty_conversation_normalization(
    conversation: &ShelleyConversationRow,
    raw_source_path: &str,
    user_version: i64,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
) -> ProviderNormalizationResult {
    let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
    let ended_at = conversation
        .updated_at
        .as_deref()
        .map(|timestamp| shelley_timestamp(Some(timestamp), context.imported_at));
    ProviderNormalizationResult {
        captures: vec![(
            0,
            shelley_capture(
                ShelleyCaptureDraft {
                    conversation,
                    started_at,
                    ended_at,
                    raw_source_path,
                    user_version,
                    schema_fingerprint,
                    event: None,
                },
                context,
            ),
        )],
        ..ProviderNormalizationResult::default()
    }
}

struct ShelleyCaptureDraft<'a> {
    conversation: &'a ShelleyConversationRow,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    raw_source_path: &'a str,
    user_version: i64,
    schema_fingerprint: &'a str,
    event: Option<ProviderEventEnvelope>,
}

fn shelley_capture(
    draft: ShelleyCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let ShelleyCaptureDraft {
        conversation,
        started_at,
        ended_at,
        raw_source_path,
        user_version,
        schema_fingerprint,
        event,
    } = draft;
    let is_subagent = conversation.parent_conversation_id.is_some() || !conversation.user_initiated;
    let conversation_options = conversation
        .conversation_options
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    let tags = conversation
        .tags
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    let queued_messages = conversation
        .queued_messages
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Shelley,
            source_format: SHELLEY_SQLITE_SOURCE_FORMAT,
            provider_session_id: conversation.conversation_id.clone(),
            parent_provider_session_id: conversation.parent_conversation_id.clone(),
            root_provider_session_id: conversation.parent_conversation_id.clone(),
            external_agent_id: None,
            agent_type: if is_subagent {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
            is_primary: !is_subagent,
            started_at,
            ended_at,
            cwd: conversation.cwd.clone(),
            fidelity: Fidelity::Imported,
            raw_source_path: raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": SHELLEY_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "source_path": raw_source_path,
            }),
            session_metadata: json!({
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "conversation_id": conversation.conversation_id,
                "slug": conversation.slug,
                "title": conversation.slug,
                "user_initiated": conversation.user_initiated,
                "archived": conversation.archived,
                "parent_conversation_id": conversation.parent_conversation_id,
                "model": conversation.model,
                "conversation_options": conversation_options,
                "current_generation": conversation.current_generation,
                "agent_working": conversation.agent_working,
                "tags": tags,
                "is_draft": conversation.is_draft,
                "draft": conversation.draft,
                "queued_messages": queued_messages,
            }),
        },
        context,
        event,
    )
}

fn shelley_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return fallback;
    };
    parse_rfc3339_utc(raw)
        .or_else(|| {
            NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        })
        .unwrap_or(fallback)
}
