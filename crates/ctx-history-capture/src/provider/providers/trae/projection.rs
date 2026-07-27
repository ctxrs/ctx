use ctx_history_core::EventRole;
use serde::de::IgnoredAny;
use serde_json::{json, Value};

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
};
use crate::complete_content::CompleteContentBodyDigest;
use crate::provider::importer::{
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::provider_role;
use crate::{CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result};

use super::event::{trae_capture, trae_event_from_owned_message, TraeCaptureInput, TraeEventInput};
use super::json_stream::{
    trae_session_selection, trae_stream_session, TraeJsonArrayValues, TraeJsonContainerValues,
    TraeSessionSelection, TraeStreamSession,
};
use super::sqlite::{
    decode_trae_chat_row_locator, decode_trae_position, trae_line_base, trae_rejection_values,
};
use super::{
    TRAE_CHAT_KEYS, TRAE_CHAT_VALUE_RECORD_KIND, TRAE_FRONTIER_RECORD_KIND,
    TRAE_INVALID_VALUE_RECORD_KIND,
};

pub(super) struct TraeCapturedBatchProjector {
    pub(super) context: ProviderAdapterContext,
    pub(super) workspace_id: String,
    pub(super) workspace_folder: Option<String>,
    pub(super) workspace_ordinal: usize,
    #[cfg(test)]
    pub(super) projected_chat_values: usize,
}

impl CapturedBatchProjector for TraeCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        match record.record_kind().as_str() {
            TRAE_CHAT_VALUE_RECORD_KIND => {
                let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
                    return Err(ProviderProjectionFatal::system_invariant(
                        "Trae chat value record requires native bytes",
                    ));
                };
                let key_index = decode_trae_chat_row_locator(record.locator())
                    .map_err(ProviderProjectionFatal::new)?;
                let chat_key = TRAE_CHAT_KEYS.get(usize::from(key_index)).ok_or_else(|| {
                    ProviderProjectionFatal::system_invariant(
                        "Trae chat value locator key is out of range",
                    )
                })?;
                if let Err(error) = serde_json::from_slice::<IgnoredAny>(bytes) {
                    let line = usize::try_from(record.ordinal())
                        .unwrap_or(usize::MAX)
                        .saturating_add(1);
                    output.reject_record(
                        line,
                        format!("Trae ItemTable key `{chat_key}` contains invalid JSON: {error}"),
                    );
                    return Ok(());
                }
                #[cfg(test)]
                {
                    self.projected_chat_values = self.projected_chat_values.saturating_add(1);
                }
                let selection = match trae_session_selection(bytes, chat_key) {
                    Ok(selection) => selection,
                    Err(error) => {
                        let line = usize::try_from(record.ordinal())
                            .unwrap_or(usize::MAX)
                            .saturating_add(1);
                        output.reject_record(
                            line,
                            format!(
                                "Trae ItemTable key `{chat_key}` contains invalid JSON: {error}"
                            ),
                        );
                        return Ok(());
                    }
                };
                self.project_chat_value(bytes, chat_key, key_index, selection, output)
            }
            TRAE_INVALID_VALUE_RECORD_KIND => {
                let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
                    return Err(ProviderProjectionFatal::system_invariant(
                        "Trae invalid value record requires SQLite logical values",
                    ));
                };
                let (line, chat_key, value_type) = trae_rejection_values(values, "invalid value")
                    .map_err(ProviderProjectionFatal::new)?;
                output.reject_record(
                    line.saturating_add(1),
                    format!(
                        "Trae ItemTable key `{chat_key}` has unsupported SQLite type `{value_type}`"
                    ),
                );
                Ok(())
            }
            TRAE_FRONTIER_RECORD_KIND if matches!(record.payload(), CapturedRecordPayload::NativeBytes(bytes) if bytes.is_empty()) => {
                Ok(())
            }
            _ => Err(ProviderProjectionFatal::system_invariant(
                "Trae projector received an unexpected record kind",
            )),
        }
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if decode_trae_position(position)?.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Trae initial cursor candidate is not at the SQLite source start".to_owned(),
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&())?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

impl TraeCapturedBatchProjector {
    fn project_chat_value(
        &mut self,
        bytes: &[u8],
        chat_key: &str,
        key_index: u16,
        selection: Option<TraeSessionSelection>,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let Some(selection) = selection else {
            return Ok(());
        };
        let record_digest = CompleteContentBodyDigest::from_bytes(bytes);
        match selection {
            TraeSessionSelection::CnMessages(messages) => self.project_stream_session(
                bytes,
                chat_key,
                key_index,
                0,
                &record_digest,
                TraeStreamSession {
                    native_session_id: "trae-cn-input-history".to_owned(),
                    metadata_preview: json!({
                        "id": "trae-cn-input-history",
                        "title": "Trae CN input history",
                    }),
                    explicit_started_at: None,
                    explicit_ended_at: None,
                    explicit_title: Some("Trae CN input history".to_owned()),
                    messages,
                },
                output,
            ),
            TraeSessionSelection::Sessions(container) => {
                let mut sessions = TraeJsonContainerValues::new(bytes, container)
                    .map_err(ProviderProjectionFatal::new)?;
                let mut session_index = 0_usize;
                while let Some(range) = sessions
                    .next_range()
                    .map_err(ProviderProjectionFatal::new)?
                {
                    if let Some(session) = trae_stream_session(bytes, range, session_index)
                        .map_err(ProviderProjectionFatal::new)?
                    {
                        self.project_stream_session(
                            bytes,
                            chat_key,
                            key_index,
                            session_index,
                            &record_digest,
                            session,
                            output,
                        )?;
                    }
                    session_index = session_index.saturating_add(1);
                }
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn project_stream_session(
        &mut self,
        bytes: &[u8],
        chat_key: &str,
        key_index: u16,
        session_index: usize,
        record_digest: &CompleteContentBodyDigest,
        session: TraeStreamSession,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let provider_session_id = format!("{}/{}", self.workspace_id, session.native_session_id);
        let raw_source_path = self
            .context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string())
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Trae captured source path is unavailable",
                )
            })?;
        let line_base = trae_line_base(
            self.workspace_ordinal,
            usize::from(key_index),
            session_index,
        );
        let mut messages = TraeJsonArrayValues::new(bytes, session.messages.clone())
            .map_err(ProviderProjectionFatal::new)?;
        let mut message_index = 0_usize;
        let mut first_event_at = None;
        let mut last_event_at = None;
        let mut first_title = None;
        let mut first_user_title = None;
        let mut last_line = None;
        while let Some(range) = messages
            .next_range()
            .map_err(ProviderProjectionFatal::new)?
        {
            let message: Value = serde_json::from_slice(&bytes[range])
                .map_err(|error| ProviderProjectionFatal::new(CaptureError::Json(error)))?;
            let Some(event) = trae_event_from_owned_message(
                &provider_session_id,
                &self.workspace_id,
                chat_key,
                message,
                message_index,
                self.context.imported_at,
                line_base,
            ) else {
                message_index = message_index.saturating_add(1);
                continue;
            };
            first_event_at.get_or_insert(event.occurred_at);
            last_event_at = Some(event.occurred_at);
            let generated = event
                .text
                .replace('\n', " ")
                .chars()
                .take(50)
                .collect::<String>();
            if !generated.trim().is_empty() {
                first_title.get_or_insert_with(|| generated.clone());
                if provider_role(event.role.as_deref()) == EventRole::User {
                    first_user_title.get_or_insert(generated);
                }
            }
            let started_at = session
                .explicit_started_at
                .or(first_event_at)
                .unwrap_or(self.context.imported_at);
            let ended_at = session.explicit_ended_at.or(last_event_at);
            let title = session
                .explicit_title
                .clone()
                .or_else(|| first_user_title.clone())
                .or_else(|| first_title.clone());
            last_line = Some(event.line_number);
            output.emit_normalization(ProviderNormalizationResult {
                captures: vec![(
                    event.line_number,
                    trae_capture(TraeCaptureInput {
                        provider_session_id: &provider_session_id,
                        native_session_id: &session.native_session_id,
                        workspace_id: &self.workspace_id,
                        workspace_folder: self.workspace_folder.as_deref(),
                        raw_source_path: &raw_source_path,
                        chat_key,
                        session: &session.metadata_preview,
                        context: &self.context,
                        started_at,
                        ended_at,
                        title,
                        event,
                        complete_content_locator: Some(
                            super::trae_complete_message_locator(
                                key_index,
                                session_index,
                                message_index,
                            )
                            .map_err(ProviderProjectionFatal::new)?,
                        ),
                        complete_content_record_digest: Some(record_digest.clone()),
                    })
                    .map_err(ProviderProjectionFatal::new)?,
                )],
                ..ProviderNormalizationResult::default()
            })?;
            message_index = message_index.saturating_add(1);
        }
        let Some(line) = last_line else {
            return Ok(());
        };
        let started_at = session
            .explicit_started_at
            .or(first_event_at)
            .unwrap_or(self.context.imported_at);
        let ended_at = session.explicit_ended_at.or(last_event_at);
        let title = session.explicit_title.or(first_user_title).or(first_title);
        let mut refresh = trae_capture(TraeCaptureInput {
            provider_session_id: &provider_session_id,
            native_session_id: &session.native_session_id,
            workspace_id: &self.workspace_id,
            workspace_folder: self.workspace_folder.as_deref(),
            raw_source_path: &raw_source_path,
            chat_key,
            session: &session.metadata_preview,
            context: &self.context,
            started_at,
            ended_at,
            title,
            event: TraeEventInput {
                line_number: line,
                provider_event_index: 0,
                native_message_id: "ctx-final-metadata".to_owned(),
                role: None,
                occurred_at: ended_at.unwrap_or(started_at),
                text: String::new(),
                raw_message: Value::Null,
            },
            complete_content_locator: None,
            complete_content_record_digest: None,
        })
        .map_err(ProviderProjectionFatal::new)?;
        refresh.event = None;
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(line, refresh)],
            ..ProviderNormalizationResult::default()
        })
    }
}
