use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
};
use crate::provider::importer::{
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::{CaptureError, ProviderAdapterContext, Result};

use super::normalization::{
    shelley_empty_conversation_normalization, shelley_message_normalization,
};
use super::relationships::{
    decode_shelley_conversation, decode_shelley_message, decode_shelley_message_child_record,
    decode_shelley_message_parent, ShelleyRelationshipState,
};
use super::row_stream::initial_shelley_position;
use super::{
    SHELLEY_CONVERSATION_RECORD_KIND, SHELLEY_MESSAGE_CHILD_RECORD_KIND,
    SHELLEY_MESSAGE_KEY_MARKER_KIND, SHELLEY_MESSAGE_KEY_REJECTION_KIND,
    SHELLEY_MESSAGE_RECORD_KIND, SHELLEY_NONEMPTY_CONVERSATION_RECORD_KIND,
    SHELLEY_OVERSIZE_SESSION_RECORD_KIND, SHELLEY_TERMINAL_MARKER_KIND,
};

pub(super) struct ShelleyCapturedBatchProjector {
    context: ProviderAdapterContext,
    raw_source_path: String,
    user_version: i64,
    schema_fingerprint: String,
    relationships: ShelleyRelationshipState,
}

impl ShelleyCapturedBatchProjector {
    pub(super) fn new(
        context: ProviderAdapterContext,
        raw_source_path: String,
        user_version: i64,
        schema_fingerprint: String,
    ) -> Self {
        Self {
            context,
            raw_source_path,
            user_version,
            schema_fingerprint,
            relationships: ShelleyRelationshipState::default(),
        }
    }
}

impl CapturedBatchProjector for ShelleyCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if matches!(
            record.record_kind().as_str(),
            SHELLEY_NONEMPTY_CONVERSATION_RECORD_KIND
                | SHELLEY_MESSAGE_KEY_MARKER_KIND
                | SHELLEY_TERMINAL_MARKER_KIND
        ) {
            return Ok(());
        }
        if record.record_kind().as_str() == SHELLEY_MESSAGE_KEY_REJECTION_KIND {
            output.reject_record(
                shelley_record_line(record),
                "Shelley message conversation_id must be text".to_owned(),
            );
            return Ok(());
        }
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Shelley projector requires SQLite logical values",
            ));
        };
        let normalization = match record.record_kind().as_str() {
            SHELLEY_MESSAGE_RECORD_KIND => {
                let conversation = match decode_shelley_message_parent(values) {
                    Ok(conversation) => conversation,
                    Err(error) => {
                        self.relationships.clear_active_conversation();
                        output.reject_record(shelley_record_line(record), error.to_string());
                        return Ok(());
                    }
                };
                self.relationships.replace_active_conversation(conversation);
                let message = match decode_shelley_message(values) {
                    Ok(message) => message,
                    Err(error) => {
                        output.reject_record(shelley_record_line(record), error.to_string());
                        return Ok(());
                    }
                };
                let same_conversation =
                    self.relationships
                        .active_conversation()
                        .is_some_and(|conversation| {
                            conversation.conversation_id == message.conversation_id
                        });
                if !same_conversation {
                    self.relationships.clear_active_conversation();
                    output.reject_record(
                        shelley_record_line(record),
                        "Shelley parent-bearing message references a different conversation"
                            .to_owned(),
                    );
                    return Ok(());
                }
                let conversation = self.relationships.active_conversation().ok_or_else(|| {
                    ProviderProjectionFatal::system_invariant(
                        "Shelley parent-bearing message did not populate its conversation cache",
                    )
                })?;
                shelley_message_normalization(
                    message,
                    conversation,
                    &self.raw_source_path,
                    self.user_version,
                    &self.schema_fingerprint,
                    &self.context,
                )
            }
            SHELLEY_MESSAGE_CHILD_RECORD_KIND => {
                let (message, has_conversation) = match decode_shelley_message_child_record(values)
                {
                    Ok(decoded) => decoded,
                    Err(error) => {
                        output.reject_record(shelley_record_line(record), error.to_string());
                        return Ok(());
                    }
                };
                if !has_conversation {
                    self.relationships.clear_active_conversation();
                    output.reject_record(
                        message.sequence_id.max(0) as usize,
                        format!(
                            "Shelley message {} references missing conversation {}",
                            message.message_id, message.conversation_id
                        ),
                    );
                    return Ok(());
                }
                let Some(conversation) = self
                    .relationships
                    .active_conversation()
                    .filter(|conversation| conversation.conversation_id == message.conversation_id)
                else {
                    output.reject_record(
                        shelley_record_line(record),
                        "Shelley child message is not preceded by its bounded conversation row"
                            .to_owned(),
                    );
                    return Ok(());
                };
                shelley_message_normalization(
                    message,
                    conversation,
                    &self.raw_source_path,
                    self.user_version,
                    &self.schema_fingerprint,
                    &self.context,
                )
            }
            SHELLEY_CONVERSATION_RECORD_KIND | SHELLEY_OVERSIZE_SESSION_RECORD_KIND => {
                let conversation = match decode_shelley_conversation(values) {
                    Ok(conversation) => conversation,
                    Err(error) => {
                        output.reject_record(shelley_record_line(record), error.to_string());
                        return Ok(());
                    }
                };
                shelley_empty_conversation_normalization(
                    &conversation,
                    &self.raw_source_path,
                    self.user_version,
                    &self.schema_fingerprint,
                    &self.context,
                )
            }
            _ => {
                return Err(ProviderProjectionFatal::system_invariant(
                    "Shelley projector received an unexpected record kind",
                ));
            }
        };
        output.emit_normalization(normalization)
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if *position != initial_shelley_position()? {
            return Err(CaptureError::InvalidPayload(
                "Shelley initial cursor candidate is not at the SQLite source start".to_owned(),
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

fn shelley_record_line(record: &CapturedRecord) -> usize {
    usize::try_from(record.ordinal())
        .unwrap_or(usize::MAX)
        .saturating_add(1)
}
