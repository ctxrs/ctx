use chrono::Duration;
use ctx_history_core::CaptureProvider;
use serde_json::Value;

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, CapturedSqliteValue, NativePosition,
    SourceObservation,
};
use crate::provider::file_touches::{
    visit_provider_file_touches_from_raw_value, ProviderFileTouchSourceContext,
    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::normalization::provider_line_from_index;
use crate::{CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result};

use super::event::{forgecode_for_each_metric_file_touch, forgecode_timestamp};
use super::normalization::{forgecode_capture, forgecode_event, ForgeCodeCaptureContext};
use super::source::{
    decode_forgecode_conversation, decode_forgecode_position, ForgeCodeConversationRow,
};
use super::{
    FORGECODE_RECORD_KIND, FORGECODE_REJECTED_RECORD_KIND, FORGECODE_SQLITE_SOURCE_FORMAT,
};

pub(super) struct ForgeCodeCapturedBatchProjector {
    pub(super) context: ProviderAdapterContext,
    pub(super) raw_source_path: String,
    pub(super) user_version: i64,
    pub(super) schema_fingerprint: String,
}

impl CapturedBatchProjector for ForgeCodeCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "ForgeCode projector requires SQLite logical values",
            ));
        };
        match record.record_kind().as_str() {
            FORGECODE_RECORD_KIND => {
                let row =
                    decode_forgecode_conversation(values).map_err(ProviderProjectionFatal::new)?;
                forgecode_project_row(
                    &row,
                    &self.raw_source_path,
                    self.user_version,
                    &self.schema_fingerprint,
                    &self.context,
                    output,
                )
            }
            FORGECODE_REJECTED_RECORD_KIND => {
                let [CapturedSqliteValue::Integer(rowid), CapturedSqliteValue::Text(reason)] =
                    values.as_slice()
                else {
                    return Err(ProviderProjectionFatal::system_invariant(
                        "ForgeCode rejected conversation has an invalid value shape",
                    ));
                };
                output.reject_record(
                    provider_line_from_index((*rowid).max(0) as u64),
                    reason.clone(),
                );
                Ok(())
            }
            _ => Err(ProviderProjectionFatal::system_invariant(
                "ForgeCode projector received an unexpected record kind",
            )),
        }
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if decode_forgecode_position(position)?.is_some() {
            return Err(CaptureError::InvalidPayload(
                "ForgeCode initial cursor candidate is not at the SQLite source start".to_owned(),
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

fn forgecode_project_row(
    row: &ForgeCodeConversationRow,
    raw_source_path: &str,
    user_version: i64,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
    output: &mut dyn ProviderProjectionOutput,
) -> ProviderProjectionResult<()> {
    let row_line = provider_line_from_index(row.rowid.max(0) as u64);
    let started_at = forgecode_timestamp(Some(&row.created_at), context.imported_at);
    let ended_at = row
        .updated_at
        .as_deref()
        .map(|raw| forgecode_timestamp(Some(raw), started_at));
    let context_value = match row.context.as_deref().filter(|raw| !raw.trim().is_empty()) {
        Some(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(value) => Some(value),
            Err(err) => {
                output.reject_record(
                    row_line,
                    format!(
                        "invalid JSON in ForgeCode conversations.context {}: {err}",
                        row.conversation_id
                    ),
                );
                None
            }
        },
        None => None,
    };
    let metrics_value = match row.metrics.as_deref().filter(|raw| !raw.trim().is_empty()) {
        Some(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(value) => Some(value),
            Err(err) => {
                output.reject_record(
                    row_line,
                    format!(
                        "invalid JSON in ForgeCode conversations.metrics {}: {err}",
                        row.conversation_id
                    ),
                );
                None
            }
        },
        None => None,
    };
    let source_root = context.source_root_display();

    let mut emitted_events = false;
    if let Some(messages) = context_value
        .as_ref()
        .and_then(|value| value.get("messages"))
        .and_then(Value::as_array)
    {
        for (index, entry) in messages.iter().enumerate() {
            let provider_event_index = (index as u64).saturating_add(1);
            let occurred_at =
                started_at + Duration::milliseconds(i64::try_from(index).unwrap_or(i64::MAX));
            let event = forgecode_event(row, entry, provider_event_index, occurred_at);
            let line = provider_line_from_index(provider_event_index);
            output.use_explicit_file_touches();
            let mut capture = Some(forgecode_capture(
                row,
                ForgeCodeCaptureContext {
                    started_at,
                    ended_at,
                    raw_source_path,
                    user_version,
                    schema_fingerprint,
                    context_value: context_value.as_ref(),
                    metrics_value: metrics_value.as_ref(),
                    event: Some(event.clone()),
                },
                context,
            ));
            let touch_outcome = visit_provider_file_touches_from_raw_value(
                ProviderFileTouchSourceContext::new(
                    CaptureProvider::ForgeCode,
                    &row.conversation_id,
                    FORGECODE_SQLITE_SOURCE_FORMAT,
                    Some(raw_source_path),
                    source_root.as_deref(),
                ),
                entry,
                &event,
                line,
                |(touch_line, touch)| {
                    let captures = capture
                        .take()
                        .map(|capture| vec![(line, capture)])
                        .unwrap_or_default();
                    output.emit_normalization(ProviderNormalizationResult {
                        captures,
                        files_touched: vec![(touch_line, touch)],
                        ..ProviderNormalizationResult::default()
                    })
                },
            )?;
            if touch_outcome.limit_exceeded() {
                output.reject_record(line, PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned());
            }
            if touch_outcome.emitted() == 0 {
                let capture = capture.take().ok_or_else(|| {
                    ProviderProjectionFatal::system_invariant(
                        "ForgeCode touch-free event lost its pending capture",
                    )
                })?;
                output.emit_normalization(ProviderNormalizationResult {
                    captures: vec![(line, capture)],
                    ..ProviderNormalizationResult::default()
                })?;
            }
            emitted_events = true;
        }
    }

    if !emitted_events {
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(
                row_line,
                forgecode_capture(
                    row,
                    ForgeCodeCaptureContext {
                        started_at,
                        ended_at,
                        raw_source_path,
                        user_version,
                        schema_fingerprint,
                        context_value: context_value.as_ref(),
                        metrics_value: metrics_value.as_ref(),
                        event: None,
                    },
                    context,
                ),
            )],
            ..ProviderNormalizationResult::default()
        })?;
    }

    if let Some(metrics) = metrics_value.as_ref() {
        let metric_touch_limit_exceeded = forgecode_for_each_metric_file_touch(
            row,
            metrics,
            raw_source_path,
            ended_at.unwrap_or(started_at),
            |(line, mut touch)| {
                touch.source_root = source_root.clone();
                output.emit_normalization(ProviderNormalizationResult {
                    files_touched: vec![(line, touch)],
                    ..ProviderNormalizationResult::default()
                })
            },
        )?;
        if metric_touch_limit_exceeded {
            output.reject_record(row_line, PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned());
        }
    }
    Ok(())
}
