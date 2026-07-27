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
use crate::provider::providers::native_jsonl::native_jsonl_timestamp;
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportSummary, ProviderNormalizationResult,
    Result, MISTRAL_VIBE_SOURCE_FORMAT,
};

use super::schema::{
    mistral_vibe_capture, mistral_vibe_event, mistral_vibe_metadata_pointer_string,
    mistral_vibe_metadata_string, mistral_vibe_metadata_timestamp, MistralVibeCaptureDraft,
};
use super::source::MistralVibeSessionSource;
use super::MISTRAL_VIBE_RECORD_KIND;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MistralVibeParserCheckpoint {
    pub(super) metadata_revision: String,
    pub(super) metadata_failure_reported: bool,
    pub(super) next_ordinal: u64,
    pub(super) accepted_captures: u64,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
    pub(super) rejected_records: u64,
}

pub(super) struct MistralVibeCapturedBatchProjector {
    context: ProviderAdapterContext,
    source: MistralVibeSessionSource,
    metadata: Value,
    pub(super) metadata_revision: String,
    metadata_failure: Option<String>,
    metadata_failure_reported: bool,
    pub(super) next_ordinal: u64,
    accepted_captures: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    last_accepted_ordinal_in_batch: Option<u64>,
    complete_content_binding: crate::complete_content::jsonl::ExactJsonlSourceBinding,
}

impl MistralVibeCapturedBatchProjector {
    pub(super) fn fresh(
        context: ProviderAdapterContext,
        source: MistralVibeSessionSource,
        metadata: Value,
        metadata_revision: String,
        metadata_failure: Option<String>,
        complete_content_binding: crate::complete_content::jsonl::ExactJsonlSourceBinding,
    ) -> Self {
        Self {
            context,
            source,
            metadata,
            metadata_revision,
            metadata_failure,
            metadata_failure_reported: false,
            next_ordinal: 0,
            accepted_captures: 0,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejected_records: 0,
            last_accepted_ordinal_in_batch: None,
            complete_content_binding,
        }
    }

    pub(super) fn resume(
        context: ProviderAdapterContext,
        source: MistralVibeSessionSource,
        metadata: Value,
        metadata_failure: Option<String>,
        cursor: &CertifiedProviderCursor,
        complete_content_binding: crate::complete_content::jsonl::ExactJsonlSourceBinding,
    ) -> Result<Self> {
        let checkpoint: MistralVibeParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        Ok(Self {
            context,
            source,
            metadata,
            metadata_revision: checkpoint.metadata_revision,
            metadata_failure,
            metadata_failure_reported: checkpoint.metadata_failure_reported,
            next_ordinal: checkpoint.next_ordinal,
            accepted_captures: checkpoint.accepted_captures,
            accepted_events: checkpoint.accepted_events,
            accepted_file_touches: checkpoint.accepted_file_touches,
            rejected_records: checkpoint.rejected_records.max(cursor.rejected_records()),
            last_accepted_ordinal_in_batch: None,
            complete_content_binding,
        })
    }

    fn line_number(&mut self, ordinal: u64) -> Result<usize> {
        if ordinal < self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Mistral Vibe captured record ordinal moved backwards",
            ));
        }
        self.next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Mistral Vibe captured record ordinal overflowed",
        ))?;
        usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Mistral Vibe captured record ordinal exceeds platform limits",
            ))
    }

    fn reject_record(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        line_number: usize,
        reason: String,
    ) -> ProviderProjectionResult<()> {
        self.rejected_records = self.rejected_records.checked_add(1).ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("Mistral Vibe rejection count overflowed")
        })?;
        output.reject_record(line_number, reason);
        Ok(())
    }

    fn accept(
        &mut self,
        normalization: ProviderNormalizationResult,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let captures = u64::try_from(normalization.captures.len())
            .map_err(|_| CaptureError::SystemInvariant("Mistral Vibe capture count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        let events = u64::try_from(
            normalization
                .captures
                .iter()
                .filter(|(_, capture)| capture.event.is_some())
                .count(),
        )
        .map_err(|_| CaptureError::SystemInvariant("Mistral Vibe event count exceeds u64"))
        .map_err(ProviderProjectionFatal::new)?;
        let file_touches = u64::try_from(normalization.files_touched.len())
            .map_err(|_| CaptureError::SystemInvariant("Mistral Vibe file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_captures = self
            .accepted_captures
            .checked_add(captures)
            .ok_or(CaptureError::SystemInvariant(
                "Mistral Vibe capture count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_events = self
            .accepted_events
            .checked_add(events)
            .ok_or(CaptureError::SystemInvariant(
                "Mistral Vibe event count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touches)
            .ok_or(CaptureError::SystemInvariant(
                "Mistral Vibe file-touch count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        emit_projected_normalization_units(output, normalization)
    }

    pub(super) fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::from(self.accepted_captures != 0);
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("Mistral Vibe replay event count exceeds platform limits")
        })?;
        let skipped_file_touches = usize::try_from(self.accepted_file_touches).map_err(|_| {
            CaptureError::SystemInvariant(
                "Mistral Vibe replay file-touch count exceeds platform limits",
            )
        })?;
        let skipped = skipped_sessions
            .checked_add(skipped_events)
            .and_then(|value| value.checked_add(skipped_file_touches))
            .ok_or(CaptureError::SystemInvariant(
                "Mistral Vibe replay summary count overflowed",
            ))?;
        let failed = usize::try_from(self.rejected_records).map_err(|_| {
            CaptureError::SystemInvariant(
                "Mistral Vibe replay rejection count exceeds platform limits",
            )
        })?;
        Ok(ProviderImportSummary {
            skipped,
            failed,
            skipped_sessions,
            skipped_events,
            accepted_content_records: skipped_events.saturating_add(skipped_file_touches),
            ..ProviderImportSummary::default()
        })
    }
}

impl CapturedBatchProjector for MistralVibeCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != MISTRAL_VIBE_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Mistral Vibe projector received an unexpected record kind",
            ));
        }
        let line_number = self
            .line_number(record.ordinal())
            .map_err(ProviderProjectionFatal::new)?;
        if !self.metadata_failure_reported {
            if let Some(error) = self.metadata_failure.clone() {
                self.reject_record(output, line_number, error)?;
            }
            self.metadata_failure_reported = true;
        }
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Mistral Vibe projector requires native JSONL bytes",
            ));
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                return self.reject_record(
                    output,
                    line_number,
                    format!(
                        "malformed JSONL in {}: {error}",
                        self.source.messages_path.display()
                    ),
                );
            }
        };
        let provider_session_id = mistral_vibe_metadata_string(&self.metadata, "session_id")
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Mistral Vibe bounded metadata lost its session id",
                )
            })?;
        let started_at = mistral_vibe_metadata_timestamp(&self.metadata, "start_time")
            .unwrap_or(self.context.imported_at);
        let occurred_at = native_jsonl_timestamp(&value).unwrap_or(started_at);
        let started_at = started_at.min(occurred_at);
        let parent_provider_session_id =
            mistral_vibe_metadata_string(&self.metadata, "parent_session_id");
        let agent_type = if parent_provider_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        };
        let mut event = mistral_vibe_event(
            &provider_session_id,
            line_number,
            &value,
            occurred_at,
            &self.source.messages_path,
            &self.metadata,
        );
        crate::complete_content::jsonl::attach_exact_jsonl_complete_content_locator(
            &mut event,
            CaptureProvider::MistralVibe,
            MISTRAL_VIBE_SOURCE_FORMAT,
            &value,
            record,
            line_number,
            &self.complete_content_binding,
        )
        .map_err(ProviderProjectionFatal::new)?;
        if let Some((content, native_record_id)) =
            crate::complete_content::jsonl::result_content_and_id(
                CaptureProvider::MistralVibe,
                MISTRAL_VIBE_SOURCE_FORMAT,
                &value,
                line_number,
            )
        {
            crate::complete_content::jsonl::attach_exact_jsonl_result_content_locator(
                &mut event,
                CaptureProvider::MistralVibe,
                MISTRAL_VIBE_SOURCE_FORMAT,
                &content,
                &native_record_id,
                record,
                &self.complete_content_binding,
            )
            .map_err(ProviderProjectionFatal::new)?;
        }
        let raw_source_path = self.source.messages_path.display().to_string();
        let source_root = self.context.source_root_display();
        output.use_explicit_file_touches();
        self.accept(
            ProviderNormalizationResult {
                captures: vec![(
                    line_number,
                    mistral_vibe_capture(
                        MistralVibeCaptureDraft {
                            provider_session_id: provider_session_id.clone(),
                            parent_provider_session_id,
                            agent_type,
                            role_hint: if agent_type == AgentType::Primary {
                                "primary".to_owned()
                            } else {
                                "subagent".to_owned()
                            },
                            is_primary: agent_type == AgentType::Primary,
                            started_at,
                            ended_at: mistral_vibe_metadata_timestamp(&self.metadata, "end_time"),
                            cwd: mistral_vibe_metadata_pointer_string(
                                &self.metadata,
                                &["/environment/working_directory"],
                            ),
                            metadata: &self.metadata,
                            source: &self.source,
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
                CaptureProvider::MistralVibe,
                &provider_session_id,
                MISTRAL_VIBE_SOURCE_FORMAT,
                Some(raw_source_path.as_str()),
                source_root.as_deref(),
            ),
            &value,
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
            .map_err(|_| CaptureError::SystemInvariant("Mistral Vibe file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touch_count)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Mistral Vibe file-touch count overflowed",
                )
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
                    "Mistral Vibe final metadata record is outside the captured batch",
                )
            })?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Mistral Vibe final metadata requires native JSONL bytes",
            ));
        };
        let value = serde_json::from_slice::<Value>(bytes).map_err(|_| {
            ProviderProjectionFatal::system_invariant(
                "Mistral Vibe accepted final metadata record is not valid JSON",
            )
        })?;
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Mistral Vibe final metadata ordinal exceeds platform limits",
                )
            })?;
        let provider_session_id = mistral_vibe_metadata_string(&self.metadata, "session_id")
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Mistral Vibe bounded metadata lost its session id",
                )
            })?;
        let started_at = mistral_vibe_metadata_timestamp(&self.metadata, "start_time")
            .unwrap_or(self.context.imported_at);
        let occurred_at = native_jsonl_timestamp(&value).unwrap_or(started_at);
        let started_at = started_at.min(occurred_at);
        let parent_provider_session_id =
            mistral_vibe_metadata_string(&self.metadata, "parent_session_id");
        let agent_type = if parent_provider_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        };
        Ok(Some((
            line_number,
            mistral_vibe_capture(
                MistralVibeCaptureDraft {
                    provider_session_id,
                    parent_provider_session_id,
                    agent_type,
                    role_hint: if agent_type == AgentType::Primary {
                        "primary".to_owned()
                    } else {
                        "subagent".to_owned()
                    },
                    is_primary: agent_type == AgentType::Primary,
                    started_at,
                    ended_at: mistral_vibe_metadata_timestamp(&self.metadata, "end_time"),
                    cwd: mistral_vibe_metadata_pointer_string(
                        &self.metadata,
                        &["/environment/working_directory"],
                    ),
                    metadata: &self.metadata,
                    source: &self.source,
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
            BoundedParserCheckpoint::from_serializable(&MistralVibeParserCheckpoint {
                metadata_revision: self.metadata_revision.clone(),
                metadata_failure_reported: false,
                next_ordinal: 0,
                accepted_captures: 0,
                accepted_events: 0,
                accepted_file_touches: 0,
                rejected_records: 0,
            })?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_ordinal = batch
            .records()
            .last()
            .and_then(|record| record.ordinal().checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Mistral Vibe captured batch did not have a next ordinal",
            ))?;
        if self.next_ordinal > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Mistral Vibe projector advanced beyond the captured batch",
            ));
        }
        CertifiedProviderCursor::new(
            batch.source().source_revision(),
            batch.source().capture_revision(),
            batch.source().policy_revision(),
            batch.range_end().clone(),
            BoundedParserCheckpoint::from_serializable(&MistralVibeParserCheckpoint {
                metadata_revision: self.metadata_revision.clone(),
                metadata_failure_reported: self.metadata_failure_reported,
                next_ordinal,
                accepted_captures: self.accepted_captures,
                accepted_events: self.accepted_events,
                accepted_file_touches: self.accepted_file_touches,
                rejected_records: self.rejected_records,
            })?,
        )
        .map(CapturedBatchCursorFinish::Advance)
    }
}
