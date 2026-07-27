use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, CaptureProvider, ProviderCaptureEnvelope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
};
use crate::provider::file_touches::{
    visit_provider_file_touches_from_raw_value, ProviderFileTouchSourceContext,
    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    emit_projected_normalization_units, BoundedParserCheckpoint, CapturedBatchCursorFinish,
    CapturedBatchProjector, CertifiedProviderCursor, ProviderProjectionFatal,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportFailure, ProviderImportSummary,
    ProviderNormalizationResult, Result, MAX_PROVIDER_JSONL_LINE_BYTES, MUX_SOURCE_FORMAT,
};

use super::metadata::{bounded_mux_failure, bounded_mux_id, MuxBoundedSessionMetadata};
use super::normalization::{
    mux_capture, mux_event, mux_history_sequence, mux_message_model, mux_message_timestamp_opt,
    mux_partial_event_index, MuxCaptureDraft, MuxMessageRow,
};
use super::source::MuxSessionSource;
use super::{MUX_CHAT_RECORD_KIND, MUX_PARTIAL_RECORD_KIND};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MuxBoundedFailure {
    line: usize,
    error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MuxParserCheckpoint {
    pub(super) provider_session_id: String,
    pub(super) metadata_revision: String,
    pub(super) next_ordinal: u64,
    pub(super) accepted_captures: u64,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
    pub(super) rejected_records: u64,
    pub(super) metadata_failure_reported: bool,
    first_failure: Option<MuxBoundedFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MuxCapturedStreamKind {
    Chat,
    Partial,
}

impl MuxCapturedStreamKind {
    fn record_kind(self) -> &'static str {
        match self {
            Self::Chat => MUX_CHAT_RECORD_KIND,
            Self::Partial => MUX_PARTIAL_RECORD_KIND,
        }
    }

    fn is_partial(self) -> bool {
        self == Self::Partial
    }
}

pub(super) struct MuxCapturedBatchProjector {
    context: ProviderAdapterContext,
    source: MuxSessionSource,
    source_path: PathBuf,
    stream_kind: MuxCapturedStreamKind,
    session: MuxBoundedSessionMetadata,
    pub(super) certified_metadata_revision: String,
    pub(super) next_ordinal: u64,
    accepted_captures: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    metadata_failure_reported: bool,
    first_failure: Option<MuxBoundedFailure>,
    last_accepted_ordinal_in_batch: Option<u64>,
}

impl MuxCapturedBatchProjector {
    pub(super) fn fresh(
        context: ProviderAdapterContext,
        source: MuxSessionSource,
        source_path: PathBuf,
        stream_kind: MuxCapturedStreamKind,
        session: MuxBoundedSessionMetadata,
    ) -> Self {
        let certified_metadata_revision = session.metadata_revision.clone();
        Self {
            context,
            source,
            source_path,
            stream_kind,
            session,
            certified_metadata_revision,
            next_ordinal: 0,
            accepted_captures: 0,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejected_records: 0,
            metadata_failure_reported: false,
            first_failure: None,
            last_accepted_ordinal_in_batch: None,
        }
    }

    pub(super) fn resume(
        context: ProviderAdapterContext,
        source: MuxSessionSource,
        source_path: PathBuf,
        stream_kind: MuxCapturedStreamKind,
        mut session: MuxBoundedSessionMetadata,
        cursor: &CertifiedProviderCursor,
    ) -> Result<Self> {
        let checkpoint: MuxParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        session.provider_session_id = checkpoint.provider_session_id;
        Ok(Self {
            context,
            source,
            source_path,
            stream_kind,
            session,
            certified_metadata_revision: checkpoint.metadata_revision,
            next_ordinal: checkpoint.next_ordinal,
            accepted_captures: checkpoint.accepted_captures,
            accepted_events: checkpoint.accepted_events,
            accepted_file_touches: checkpoint.accepted_file_touches,
            rejected_records: checkpoint.rejected_records.max(cursor.rejected_records()),
            metadata_failure_reported: checkpoint.metadata_failure_reported,
            first_failure: checkpoint.first_failure,
            last_accepted_ordinal_in_batch: None,
        })
    }

    fn line_number(&mut self, ordinal: u64) -> Result<usize> {
        if ordinal != self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Mux captured record ordinal is not contiguous",
            ));
        }
        self.next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Mux captured record ordinal overflowed",
        ))?;
        usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Mux captured record ordinal exceeds platform limits",
            ))
    }

    fn reject_record(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        line_number: usize,
        reason: String,
    ) -> ProviderProjectionResult<()> {
        self.rejected_records = self.rejected_records.checked_add(1).ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("Mux rejection count overflowed")
        })?;
        let reason = bounded_mux_failure(reason);
        if self.first_failure.is_none() {
            self.first_failure = Some(MuxBoundedFailure {
                line: line_number,
                error: reason.clone(),
            });
        }
        output.reject_record(line_number, reason);
        Ok(())
    }

    fn accept(
        &mut self,
        normalization: ProviderNormalizationResult,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let captures = u64::try_from(normalization.captures.len())
            .map_err(|_| CaptureError::SystemInvariant("Mux capture count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        let events = u64::try_from(
            normalization
                .captures
                .iter()
                .filter(|(_, capture)| capture.event.is_some())
                .count(),
        )
        .map_err(|_| CaptureError::SystemInvariant("Mux event count exceeds u64"))
        .map_err(ProviderProjectionFatal::new)?;
        let file_touches = u64::try_from(normalization.files_touched.len())
            .map_err(|_| CaptureError::SystemInvariant("Mux file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_captures = self
            .accepted_captures
            .checked_add(captures)
            .ok_or(CaptureError::SystemInvariant(
                "Mux capture count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_events = self
            .accepted_events
            .checked_add(events)
            .ok_or(CaptureError::SystemInvariant("Mux event count overflowed"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touches)
            .ok_or(CaptureError::SystemInvariant(
                "Mux file-touch count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        emit_projected_normalization_units(output, normalization)
    }

    pub(super) fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::from(self.accepted_captures != 0);
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("Mux replay event count exceeds platform limits")
        })?;
        let skipped_file_touches = usize::try_from(self.accepted_file_touches).map_err(|_| {
            CaptureError::SystemInvariant("Mux replay file-touch count exceeds platform limits")
        })?;
        let skipped = skipped_sessions
            .checked_add(skipped_events)
            .and_then(|value| value.checked_add(skipped_file_touches))
            .ok_or(CaptureError::SystemInvariant(
                "Mux replay summary count overflowed",
            ))?;
        let failed = usize::try_from(self.rejected_records).map_err(|_| {
            CaptureError::SystemInvariant("Mux replay rejection count exceeds platform limits")
        })?;
        let failures = self
            .first_failure
            .iter()
            .map(|failure| ProviderImportFailure {
                line: failure.line,
                error: failure.error.clone(),
            })
            .collect();
        Ok(ProviderImportSummary {
            skipped,
            failed,
            skipped_sessions,
            skipped_events,
            accepted_content_records: skipped_events.saturating_add(skipped_file_touches),
            failures,
            ..ProviderImportSummary::default()
        })
    }
}

impl CapturedBatchProjector for MuxCapturedBatchProjector {
    fn project_structural_rejection(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != self.stream_kind.record_kind() {
            return Err(ProviderProjectionFatal::system_invariant(
                "Mux projector received an unexpected structural-rejection record kind",
            ));
        }
        let line_number = self
            .line_number(record.ordinal())
            .map_err(ProviderProjectionFatal::new)?;
        if !self.metadata_failure_reported {
            if let Some(error) = self.session.metadata_failure.clone() {
                self.reject_record(output, line_number, error)?;
            }
            self.metadata_failure_reported = true;
        }
        let CapturedRecordPayload::StructuralRejection { observed_bytes, .. } = record.payload()
        else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Mux structural-rejection hook requires a structural rejection record",
            ));
        };
        self.reject_record(
            output,
            line_number,
            format!(
                "provider record exceeds the {} byte limit (observed {observed_bytes} bytes)",
                MAX_PROVIDER_JSONL_LINE_BYTES
            ),
        )
    }

    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != self.stream_kind.record_kind() {
            return Err(ProviderProjectionFatal::system_invariant(
                "Mux projector received an unexpected record kind",
            ));
        }
        let line_number = self
            .line_number(record.ordinal())
            .map_err(ProviderProjectionFatal::new)?;
        if !self.metadata_failure_reported {
            if let Some(error) = self.session.metadata_failure.clone() {
                self.reject_record(output, line_number, error)?;
            }
            self.metadata_failure_reported = true;
        }
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Mux projector requires native JSON bytes",
            ));
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                return self.reject_record(
                    output,
                    line_number,
                    format!(
                        "{} must contain a JSON object",
                        self.stream_kind.record_kind()
                    ),
                );
            }
            Err(error) => {
                return self.reject_record(
                    output,
                    line_number,
                    format!("malformed Mux JSON record: {error}"),
                );
            }
        };
        if let Some(provider_session_id) = value
            .get("workspaceId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        {
            self.session.provider_session_id = bounded_mux_id(
                provider_session_id.to_owned(),
                &self.source_path,
                "workspace id",
            )
            .map_err(ProviderProjectionFatal::new)?;
        }
        let started_at = self
            .session
            .started_at
            .parse::<DateTime<Utc>>()
            .map_err(|_| {
                ProviderProjectionFatal::system_invariant(
                    "Mux checkpoint contains an invalid start time",
                )
            })?;
        let occurred_at = mux_message_timestamp_opt(&value).unwrap_or(started_at);
        let event_index = match self.stream_kind {
            MuxCapturedStreamKind::Chat => record.ordinal(),
            MuxCapturedStreamKind::Partial => mux_partial_event_index(bytes),
        };
        let row = MuxMessageRow {
            line_number,
            source_path: self.source_path.clone(),
            value,
            is_partial: self.stream_kind.is_partial(),
        };
        let model = self
            .session
            .model
            .clone()
            .or_else(|| mux_message_model(&row.value));
        let projected = mux_event(
            &self.session.provider_session_id,
            event_index,
            &row,
            occurred_at,
            model.as_deref(),
        );
        let mut event = projected.event;
        crate::complete_content::jsonl::attach_mux_verified_content_locator(
            &mut event,
            projected.result_content_ref.as_ref(),
            &row.value,
            record,
            line_number,
            self.stream_kind.is_partial(),
        )
        .map_err(ProviderProjectionFatal::new)?;
        let raw_source_path = self.source_path.display().to_string();
        let source_root = self.context.source_root_display();
        let agent_type = if self.session.parent_provider_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        };
        let message_count = mux_history_sequence(&row.value)
            .and_then(|sequence| u64::try_from(sequence).ok())
            .and_then(|sequence| sequence.checked_add(1))
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(line_number);
        output.use_explicit_file_touches();
        self.accept(
            ProviderNormalizationResult {
                captures: vec![(
                    line_number,
                    mux_capture(
                        MuxCaptureDraft {
                            provider_session_id: self.session.provider_session_id.clone(),
                            parent_provider_session_id: self
                                .session
                                .parent_provider_session_id
                                .clone(),
                            root_provider_session_id: self.session.root_provider_session_id.clone(),
                            agent_type,
                            role_hint: if agent_type == AgentType::Primary {
                                "primary".to_owned()
                            } else {
                                "subagent".to_owned()
                            },
                            is_primary: agent_type == AgentType::Primary,
                            started_at,
                            ended_at: Some(occurred_at),
                            cwd: self.session.cwd.clone(),
                            model,
                            metadata: &self.session.metadata,
                            message_count,
                            source: &self.source,
                            raw_source_path: &self.source_path,
                            event: Some(event.clone()),
                        },
                        &self.context,
                    ),
                )],
                ..ProviderNormalizationResult::default()
            },
            output,
        )?;
        let file_touch_outcome = visit_provider_file_touches_from_raw_value(
            ProviderFileTouchSourceContext::new(
                CaptureProvider::Mux,
                &self.session.provider_session_id,
                MUX_SOURCE_FORMAT,
                Some(raw_source_path.as_str()),
                source_root.as_deref(),
            ),
            &row.value,
            &event,
            line_number,
            |file_touch| {
                output.emit_normalization(ProviderNormalizationResult {
                    files_touched: vec![file_touch],
                    ..ProviderNormalizationResult::default()
                })
            },
        )?;
        if file_touch_outcome.limit_exceeded() {
            self.reject_record(
                output,
                line_number,
                PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
            )?;
        }
        let file_touch_count = u64::try_from(file_touch_outcome.emitted())
            .map_err(|_| CaptureError::SystemInvariant("Mux file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touch_count)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant("Mux file-touch count overflowed")
            })?;
        self.last_accepted_ordinal_in_batch = Some(record.ordinal());
        Ok(())
    }

    fn final_metadata_capture(
        &mut self,
        batch: &CapturedBatch,
    ) -> ProviderProjectionResult<Option<(usize, ProviderCaptureEnvelope)>> {
        let Some(ordinal) = self.last_accepted_ordinal_in_batch.take() else {
            return Ok(None);
        };
        let record = batch
            .records()
            .iter()
            .find(|record| record.ordinal() == ordinal)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Mux final metadata record is outside the captured batch",
                )
            })?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Mux final metadata requires native JSON bytes",
            ));
        };
        let value = serde_json::from_slice::<Value>(bytes)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Mux accepted final metadata record is not a JSON object",
                )
            })?;
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Mux final metadata ordinal exceeds platform limits",
                )
            })?;
        let started_at = self
            .session
            .started_at
            .parse::<DateTime<Utc>>()
            .map_err(|_| {
                ProviderProjectionFatal::system_invariant(
                    "Mux checkpoint contains an invalid start time",
                )
            })?;
        let occurred_at = mux_message_timestamp_opt(&value).unwrap_or(started_at);
        let row = MuxMessageRow {
            line_number,
            source_path: self.source_path.clone(),
            value,
            is_partial: self.stream_kind.is_partial(),
        };
        let model = self
            .session
            .model
            .clone()
            .or_else(|| mux_message_model(&row.value));
        let message_count = mux_history_sequence(&row.value)
            .and_then(|sequence| u64::try_from(sequence).ok())
            .and_then(|sequence| sequence.checked_add(1))
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(line_number);
        let agent_type = if self.session.parent_provider_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        };
        Ok(Some((
            line_number,
            mux_capture(
                MuxCaptureDraft {
                    provider_session_id: self.session.provider_session_id.clone(),
                    parent_provider_session_id: self.session.parent_provider_session_id.clone(),
                    root_provider_session_id: self.session.root_provider_session_id.clone(),
                    agent_type,
                    role_hint: if agent_type == AgentType::Primary {
                        "primary".to_owned()
                    } else {
                        "subagent".to_owned()
                    },
                    is_primary: agent_type == AgentType::Primary,
                    started_at,
                    ended_at: Some(occurred_at),
                    cwd: self.session.cwd.clone(),
                    model,
                    metadata: &self.session.metadata,
                    message_count,
                    source: &self.source,
                    raw_source_path: &self.source_path,
                    event: None,
                },
                &self.context,
            ),
        )))
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&MuxParserCheckpoint {
                provider_session_id: self.session.provider_session_id.clone(),
                metadata_revision: self.session.metadata_revision.clone(),
                next_ordinal: 0,
                accepted_captures: 0,
                accepted_events: 0,
                accepted_file_touches: 0,
                rejected_records: 0,
                metadata_failure_reported: false,
                first_failure: None,
            })?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_ordinal = batch
            .records()
            .last()
            .and_then(|record| record.ordinal().checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Mux captured batch did not have a next ordinal",
            ))?;
        if self.next_ordinal > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Mux projector advanced beyond the captured batch",
            ));
        }
        CertifiedProviderCursor::new(
            batch.source().source_revision(),
            batch.source().capture_revision(),
            batch.source().policy_revision(),
            batch.range_end().clone(),
            BoundedParserCheckpoint::from_serializable(&MuxParserCheckpoint {
                provider_session_id: self.session.provider_session_id.clone(),
                metadata_revision: self.session.metadata_revision.clone(),
                next_ordinal,
                accepted_captures: self.accepted_captures,
                accepted_events: self.accepted_events,
                accepted_file_touches: self.accepted_file_touches,
                rejected_records: self.rejected_records,
                metadata_failure_reported: self.metadata_failure_reported,
                first_failure: self.first_failure.clone(),
            })?,
        )
        .map(CapturedBatchCursorFinish::Advance)
    }
}
