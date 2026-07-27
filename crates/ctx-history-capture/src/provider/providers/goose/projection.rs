use ctx_history_core::CaptureProvider;

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
};
use crate::provider::file_touches::{
    visit_provider_file_touches_from_raw_value, ProviderFileTouchSourceContext,
    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ExistingSessionEventOutcome, ProviderProjectionFatal,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::provider_line_from_index;
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
};

use super::normalization::{goose_message_normalization, goose_session_normalization};
use super::position::decode_goose_position;
use super::schema::{
    decode_goose_message_record, decode_goose_session, GooseSessionRow, GOOSE_MESSAGE_RECORD_KIND,
    GOOSE_SESSION_RECORD_KIND,
};

pub(super) struct GooseCapturedBatchProjector {
    context: ProviderAdapterContext,
    raw_source_path: String,
    user_version: i64,
    schema_version: Option<i64>,
    schema_fingerprint: String,
}

impl GooseCapturedBatchProjector {
    pub(super) fn new(
        context: ProviderAdapterContext,
        raw_source_path: String,
        user_version: i64,
        schema_version: Option<i64>,
        schema_fingerprint: String,
    ) -> Self {
        Self {
            context,
            raw_source_path,
            user_version,
            schema_version,
            schema_fingerprint,
        }
    }
}

impl CapturedBatchProjector for GooseCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Goose projector requires SQLite logical values",
            ));
        };
        match record.record_kind().as_str() {
            GOOSE_MESSAGE_RECORD_KIND => {
                let (parent_rowid, message) =
                    decode_goose_message_record(values).map_err(ProviderProjectionFatal::new)?;
                let session =
                    parent_rowid.map(|_| GooseSessionRow::event_reference(&message.session_id));
                let projection = match goose_message_normalization(
                    message,
                    session.as_ref(),
                    &self.raw_source_path,
                    self.user_version,
                    self.schema_version,
                    &self.schema_fingerprint,
                    &self.context,
                ) {
                    Ok(normalization) => normalization,
                    Err(rejection) => {
                        output.reject_record(rejection.line, rejection.reason);
                        return Ok(());
                    }
                };
                output.use_explicit_file_touches();
                let event_outcome = output.emit_existing_session_event(
                    provider_line_from_index(record.ordinal().saturating_add(1)),
                    projection.capture,
                )?;
                if event_outcome == ExistingSessionEventOutcome::Rejected {
                    return Ok(());
                }
                let source_root = self.context.source_root_display();
                let touch_outcome = visit_provider_file_touches_from_raw_value(
                    ProviderFileTouchSourceContext::new(
                        CaptureProvider::Goose,
                        &projection.provider_session_id,
                        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                        Some(self.raw_source_path.as_str()),
                        source_root.as_deref(),
                    ),
                    &projection.raw_content,
                    &projection.event,
                    projection.line,
                    |file_touch| {
                        output.emit_normalization(ProviderNormalizationResult {
                            files_touched: vec![file_touch],
                            ..ProviderNormalizationResult::default()
                        })
                    },
                )?;
                if touch_outcome.limit_exceeded() {
                    output.reject_record(
                        projection.line,
                        PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
                    );
                }
                Ok(())
            }
            GOOSE_SESSION_RECORD_KIND => {
                let session = decode_goose_session(values).map_err(ProviderProjectionFatal::new)?;
                output.emit_normalization(goose_session_normalization(
                    &session,
                    &self.raw_source_path,
                    self.user_version,
                    self.schema_version,
                    &self.schema_fingerprint,
                    &self.context,
                ))
            }
            _ => Err(ProviderProjectionFatal::system_invariant(
                "Goose projector received an unexpected record kind",
            )),
        }
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if decode_goose_position(position)?.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Goose initial cursor candidate is not at the SQLite source start".to_owned(),
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
