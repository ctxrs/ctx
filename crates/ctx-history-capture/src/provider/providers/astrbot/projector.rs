use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativePosition,
    SourceObservation,
};
use crate::provider::importer::{
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_json_text, provider_line_from_index,
    provider_nonnegative_i64_to_u64, provider_timestamp_millis, provider_value_text,
    NativeEventDraft, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    ASTRBOT_SQLITE_SOURCE_FORMAT,
};

use super::codec::{
    astrbot_captured_record_line, astrbot_checkpoint_id, astrbot_item_id, astrbot_item_text,
    astrbot_provider_session_id, astrbot_role, decode_astrbot_conversation, decode_astrbot_locator,
    decode_astrbot_platform_message, decode_astrbot_position, AstrBotConversationRow,
    AstrBotParserCheckpoint, AstrBotPhase, AstrBotPlatformMessageLink, AstrBotPlatformMessageRow,
    ASTRBOT_CONVERSATION_ORDER_VIOLATION_RECORD_KIND, ASTRBOT_CONVERSATION_RECORD_KIND,
    ASTRBOT_PLATFORM_MESSAGE_ORDER_VIOLATION_RECORD_KIND, ASTRBOT_PLATFORM_MESSAGE_RECORD_KIND,
};

pub(super) struct AstrBotCapturedBatchProjector {
    pub(super) context: ProviderAdapterContext,
    pub(super) raw_source_path: String,
    pub(super) user_version: i64,
    pub(super) schema_fingerprint: String,
    pub(super) selected_conversation: Option<String>,
    pub(super) parser_checkpoint: AstrBotParserCheckpoint,
}

impl CapturedBatchProjector for AstrBotCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "AstrBot projector requires SQLite logical values",
            ));
        };
        match record.record_kind().as_str() {
            ASTRBOT_CONVERSATION_ORDER_VIOLATION_RECORD_KIND => self.project_order_violation(
                record,
                values,
                AstrBotPhase::Conversations,
                "conversations",
                output,
            ),
            ASTRBOT_PLATFORM_MESSAGE_ORDER_VIOLATION_RECORD_KIND => self.project_order_violation(
                record,
                values,
                AstrBotPhase::PlatformMessages,
                "platform_message_history",
                output,
            ),
            ASTRBOT_CONVERSATION_RECORD_KIND => {
                let conversation =
                    decode_astrbot_conversation(values).map_err(ProviderProjectionFatal::new)?;
                decode_astrbot_locator(record.locator(), AstrBotPhase::Conversations)
                    .map_err(ProviderProjectionFatal::new)?;
                self.project_conversation(&conversation, output)
            }
            ASTRBOT_PLATFORM_MESSAGE_RECORD_KIND => {
                let (message, link) = decode_astrbot_platform_message(values)
                    .map_err(ProviderProjectionFatal::new)?;
                self.project_platform_message(
                    &message,
                    link.as_ref(),
                    astrbot_captured_record_line(record)?,
                    output,
                )
            }
            _ => Err(ProviderProjectionFatal::system_invariant(
                "AstrBot projector received an unexpected record kind",
            )),
        }
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if decode_astrbot_position(position)?.is_some() {
            return Err(CaptureError::InvalidPayload(
                "AstrBot initial cursor candidate is not at the SQLite source start".to_owned(),
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&AstrBotParserCheckpoint::empty())?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&self.parser_checkpoint)?,
            )?,
        ))
    }
}

impl AstrBotCapturedBatchProjector {
    fn project_order_violation(
        &self,
        record: &CapturedRecord,
        values: &[CapturedSqliteValue],
        phase: AstrBotPhase,
        table: &str,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if !values.is_empty() {
            return Err(ProviderProjectionFatal::system_invariant(
                "AstrBot order-violation marker retained unexpected SQLite values",
            ));
        }
        decode_astrbot_locator(record.locator(), phase).map_err(ProviderProjectionFatal::new)?;
        output.reject_record(
            astrbot_captured_record_line(record)?,
            format!("AstrBot {table} rows are not in legacy timestamp/id order by physical rowid"),
        );
        Ok(())
    }

    fn project_conversation(
        &self,
        conversation: &AstrBotConversationRow,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let conversation_row_id = match provider_nonnegative_i64_to_u64(
            conversation.row_id,
            "AstrBot conversation row id",
        ) {
            Ok(row_id) => row_id,
            Err(error) => {
                output.reject_record(0, error.to_string());
                return Ok(());
            }
        };
        let provider_session_id = astrbot_provider_session_id(conversation);
        let started_at =
            provider_timestamp_millis(conversation.created_at, self.context.imported_at);
        let ended_at = conversation
            .updated_at
            .map(|timestamp| provider_timestamp_millis(Some(timestamp), self.context.imported_at));
        let content = provider_json_text(&conversation.content);
        if let Value::Array(items) = &content {
            let mut emitted_event = false;
            for (index, item) in items.iter().enumerate() {
                if astrbot_checkpoint_id(item).is_some() {
                    continue;
                }
                let Some(text) = astrbot_item_text(item).filter(|text| !text.trim().is_empty())
                else {
                    continue;
                };
                let event = native_event(NativeEventDraft {
                    provider: CaptureProvider::AstrBot,
                    source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
                    provider_session_id: provider_session_id.clone(),
                    provider_event_index: index as u64,
                    provider_event_hash: astrbot_item_id(item)
                        .map(|id| format!("conversation:{id}")),
                    cursor: format!("conversation:{}:item:{index}", conversation.conversation_id),
                    event_type: EventType::Message,
                    role: astrbot_role(item),
                    occurred_at: started_at,
                    text,
                    body: item.clone(),
                    metadata: json!({
                        "source": "astrbot_conversations",
                        "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                        "conversation_id": conversation.conversation_id,
                        "inner_conversation_id": conversation.inner_conversation_id,
                        "item_index": index,
                    }),
                });
                output.emit_normalization(ProviderNormalizationResult {
                    captures: vec![(
                        index + 1,
                        astrbot_capture(
                            AstrBotCaptureDraft {
                                conversation,
                                provider_session_id: &provider_session_id,
                                started_at,
                                ended_at,
                                path: Path::new(&self.raw_source_path),
                                user_version: self.user_version,
                                schema_fingerprint: &self.schema_fingerprint,
                                selected_conversation: self.selected_conversation.as_deref(),
                                event: Some(event),
                            },
                            &self.context,
                        ),
                    )],
                    ..ProviderNormalizationResult::default()
                })?;
                emitted_event = true;
            }
            if !emitted_event {
                self.project_conversation_metadata(
                    conversation,
                    &provider_session_id,
                    started_at,
                    ended_at,
                    provider_line_from_index(conversation_row_id),
                    output,
                )?;
            }
            return Ok(());
        }

        let Some(text) = provider_value_text(&content).filter(|text| !text.trim().is_empty())
        else {
            return self.project_conversation_metadata(
                conversation,
                &provider_session_id,
                started_at,
                ended_at,
                provider_line_from_index(conversation_row_id),
                output,
            );
        };
        let line = provider_line_from_index(conversation_row_id);
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::AstrBot,
            source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
            provider_session_id: provider_session_id.clone(),
            provider_event_index: 0,
            provider_event_hash: Some(format!("conversation-row:{}", conversation.row_id)),
            cursor: format!("conversation:{}:content", conversation.conversation_id),
            event_type: EventType::Message,
            role: None,
            occurred_at: started_at,
            text,
            body: content,
            metadata: json!({
                "source": "astrbot_conversations",
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "conversation_id": conversation.conversation_id,
            }),
        });
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(
                line,
                astrbot_capture(
                    AstrBotCaptureDraft {
                        conversation,
                        provider_session_id: &provider_session_id,
                        started_at,
                        ended_at,
                        path: Path::new(&self.raw_source_path),
                        user_version: self.user_version,
                        schema_fingerprint: &self.schema_fingerprint,
                        selected_conversation: self.selected_conversation.as_deref(),
                        event: Some(event),
                    },
                    &self.context,
                ),
            )],
            ..ProviderNormalizationResult::default()
        })
    }

    fn project_conversation_metadata(
        &self,
        conversation: &AstrBotConversationRow,
        provider_session_id: &str,
        started_at: DateTime<Utc>,
        ended_at: Option<DateTime<Utc>>,
        line: usize,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(
                line,
                astrbot_capture(
                    AstrBotCaptureDraft {
                        conversation,
                        provider_session_id,
                        started_at,
                        ended_at,
                        path: Path::new(&self.raw_source_path),
                        user_version: self.user_version,
                        schema_fingerprint: &self.schema_fingerprint,
                        selected_conversation: self.selected_conversation.as_deref(),
                        event: None,
                    },
                    &self.context,
                ),
            )],
            ..ProviderNormalizationResult::default()
        })
    }

    fn project_platform_message(
        &self,
        message: &AstrBotPlatformMessageRow,
        link: Option<&AstrBotPlatformMessageLink>,
        record_line: usize,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let message_id =
            match provider_nonnegative_i64_to_u64(message.id, "AstrBot platform message id") {
                Ok(value) => value,
                Err(error) => {
                    output.reject_record(0, error.to_string());
                    return Ok(());
                }
            };
        let provider_session_id = link
            .map(|link| link.provider_session_id.clone())
            .unwrap_or_else(|| {
                format!(
                    "platform/{}/{}",
                    message.platform_id.as_deref().unwrap_or("unknown"),
                    message.user_id.as_deref().unwrap_or("unknown")
                )
            });
        let started_at = link
            .and_then(|link| link.parent_created_at)
            .map(|timestamp| provider_timestamp_millis(Some(timestamp), self.context.imported_at))
            .unwrap_or_else(|| {
                provider_timestamp_millis(message.created_at, self.context.imported_at)
            });
        let content = message
            .content
            .as_deref()
            .map(provider_json_text)
            .unwrap_or(Value::Null);
        let Some(text) = provider_value_text(&content).filter(|text| !text.trim().is_empty())
        else {
            return Ok(());
        };
        let role = if message.sender_id.as_deref() == message.user_id.as_deref() {
            Some(EventRole::User)
        } else {
            Some(EventRole::Assistant)
        };
        let event_index = 1_000_000u64.saturating_add(message_id);
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::AstrBot,
            source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
            provider_session_id: provider_session_id.clone(),
            provider_event_index: event_index,
            provider_event_hash: Some(format!("platform-message:{}", message.id)),
            cursor: format!("platform_message_history:id:{}", message.id),
            event_type: EventType::Message,
            role,
            occurred_at: provider_timestamp_millis(message.created_at, started_at),
            text,
            body: json!({
                "message_id": message.id,
                "platform_id": message.platform_id,
                "user_id": message.user_id,
                "sender_id": message.sender_id,
                "sender_name": message.sender_name,
                "content": content,
                "llm_checkpoint_id": message.llm_checkpoint_id,
            }),
            metadata: json!({
                "source": "astrbot_platform_message_history",
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "message_id": message.id,
            }),
        });
        if link.is_some() {
            let capture = native_provider_capture(
                NativeSessionDraft {
                    provider: CaptureProvider::AstrBot,
                    source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
                    provider_session_id,
                    parent_provider_session_id: None,
                    root_provider_session_id: None,
                    external_agent_id: message.platform_id.clone(),
                    agent_type: AgentType::Primary,
                    role_hint: Some("llm-context".to_owned()),
                    is_primary: true,
                    started_at,
                    ended_at: None,
                    cwd: None,
                    fidelity: Fidelity::Partial,
                    raw_source_path: self.raw_source_path.clone(),
                    trust: ProviderSourceTrust::ProviderNative,
                    source_metadata: json!({
                        "adapter": ASTRBOT_SQLITE_SOURCE_FORMAT,
                        "sqlite_user_version": self.user_version,
                        "schema_fingerprint": self.schema_fingerprint,
                        "support_level": "supported",
                    }),
                    session_metadata: json!({
                        "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                        "linked_checkpoint_id": message.llm_checkpoint_id,
                    }),
                },
                &self.context,
                Some(event),
            );
            return output
                .emit_existing_session_event(record_line, capture)
                .map(|_| ());
        }
        let capture = native_provider_capture(
            NativeSessionDraft {
                provider: CaptureProvider::AstrBot,
                source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
                provider_session_id: provider_session_id.clone(),
                parent_provider_session_id: None,
                root_provider_session_id: None,
                external_agent_id: message.platform_id.clone(),
                agent_type: AgentType::Primary,
                role_hint: Some("platform-history".to_owned()),
                is_primary: true,
                started_at,
                ended_at: None,
                cwd: None,
                fidelity: Fidelity::Partial,
                raw_source_path: self.raw_source_path.clone(),
                trust: ProviderSourceTrust::ProviderNative,
                source_metadata: json!({
                    "adapter": ASTRBOT_SQLITE_SOURCE_FORMAT,
                    "sqlite_user_version": self.user_version,
                    "schema_fingerprint": self.schema_fingerprint,
                    "support_level": "supported",
                }),
                session_metadata: json!({
                    "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                    "platform_id": message.platform_id,
                    "user_id": message.user_id,
                    "fidelity_gap": "platform history row was not linked to a conversations checkpoint",
                }),
            },
            &self.context,
            Some(event),
        );
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(event_index.min(usize::MAX as u64) as usize, capture)],
            ..ProviderNormalizationResult::default()
        })
    }
}

pub(super) struct AstrBotCaptureDraft<'a> {
    pub(super) conversation: &'a AstrBotConversationRow,
    pub(super) provider_session_id: &'a str,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) path: &'a Path,
    pub(super) user_version: i64,
    pub(super) schema_fingerprint: &'a str,
    pub(super) selected_conversation: Option<&'a str>,
    pub(super) event: Option<ProviderEventEnvelope>,
}

pub(super) fn astrbot_capture(
    draft: AstrBotCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let AstrBotCaptureDraft {
        conversation,
        provider_session_id,
        started_at,
        ended_at,
        path,
        user_version,
        schema_fingerprint,
        selected_conversation,
        event,
    } = draft;
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::AstrBot,
            source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: conversation.platform_id.clone(),
            agent_type: AgentType::Primary,
            role_hint: Some("llm-context".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd: None,
            fidelity: Fidelity::Partial,
            raw_source_path: path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "support_level": "supported",
            }),
            session_metadata: json!({
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "conversation_id": conversation.conversation_id,
                "inner_conversation_id": conversation.inner_conversation_id,
                "platform_id": conversation.platform_id,
                "user_id": conversation.user_id,
                "title": conversation.title,
                "persona_id": conversation.persona_id,
                "token_usage": conversation.token_usage.as_deref().map(provider_json_text),
                "selected_conversation": selected_conversation,
                "fidelity_gap": "The AstrBot importer reads local LLM context plus available platform history from data_v4.db; platform-native chats may still be partial when upstream stores non-LLM replies on the IM platform",
            }),
        },
        context,
        event,
    )
}
