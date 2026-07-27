use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Fidelity, ProviderCaptureEnvelope, ProviderSourceTrust,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
use crate::provider::normalization::{native_provider_capture, NativeSessionDraft};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportSummary, ProviderNormalizationResult,
    Result, KIMI_CODE_CLI_SOURCE_FORMAT,
};

use super::event::{kimi_event, kimi_record_timestamp};
use super::source::KimiWireSessionState;
use super::{kimi_admission_scope_revision, KIMI_WIRE_RECORD_KIND};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct KimiParserCheckpoint {
    // Replay keeps only derived identity/timing state and counters. Native state, prompts, titles,
    // and event payloads are re-read from the revalidated source observation instead.
    pub(super) auxiliary_revision: u64,
    pub(super) admission_scope_revision: String,
    started_at: Option<DateTime<Utc>>,
    pub(super) next_ordinal: u64,
    pub(super) emitted_session: bool,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
}

pub(super) struct KimiCapturedBatchProjector {
    context: ProviderAdapterContext,
    session: KimiWireSessionState,
    pub(super) next_ordinal: u64,
    emitted_session: bool,
    accepted_events: u64,
    accepted_file_touches: u64,
    complete_content_binding: crate::complete_content::jsonl::ExactJsonlSourceBinding,
}

impl KimiCapturedBatchProjector {
    pub(super) fn fresh(
        context: ProviderAdapterContext,
        session: KimiWireSessionState,
        complete_content_binding: crate::complete_content::jsonl::ExactJsonlSourceBinding,
    ) -> Self {
        Self {
            context,
            session,
            next_ordinal: 0,
            emitted_session: false,
            accepted_events: 0,
            accepted_file_touches: 0,
            complete_content_binding,
        }
    }

    pub(super) fn resume(
        context: ProviderAdapterContext,
        mut session: KimiWireSessionState,
        cursor: &CertifiedProviderCursor,
        complete_content_binding: crate::complete_content::jsonl::ExactJsonlSourceBinding,
    ) -> Result<Self> {
        let checkpoint: KimiParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        session.started_at = checkpoint.started_at.or(session.started_at);
        Ok(Self {
            context,
            session,
            next_ordinal: checkpoint.next_ordinal,
            emitted_session: checkpoint.emitted_session,
            accepted_events: checkpoint.accepted_events,
            accepted_file_touches: checkpoint.accepted_file_touches,
            complete_content_binding,
        })
    }

    fn advance_to(&mut self, ordinal: u64) -> Result<usize> {
        if ordinal < self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Kimi captured record ordinal moved backwards",
            ));
        }
        self.next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Kimi captured record ordinal overflowed",
        ))?;
        usize::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Kimi captured record ordinal exceeds platform limits",
            ))
    }

    fn emit_session_if_needed(
        &mut self,
        line_number: usize,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if self.emitted_session {
            return Ok(());
        }
        emit_projected_normalization_units(
            output,
            ProviderNormalizationResult {
                captures: vec![(
                    line_number,
                    kimi_batched_capture(&self.session, &self.context, None)
                        .map_err(ProviderProjectionFatal::new)?,
                )],
                ..ProviderNormalizationResult::default()
            },
        )?;
        self.emitted_session = true;
        Ok(())
    }

    pub(super) fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::from(self.emitted_session);
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("Kimi replay event count exceeds platform limits")
        })?;
        let skipped_file_touches = usize::try_from(self.accepted_file_touches).map_err(|_| {
            CaptureError::SystemInvariant("Kimi replay file-touch count exceeds platform limits")
        })?;
        let skipped = skipped_sessions
            .checked_add(skipped_events)
            .and_then(|count| count.checked_add(skipped_file_touches))
            .ok_or(CaptureError::SystemInvariant(
                "Kimi replay summary count overflowed",
            ))?;
        Ok(ProviderImportSummary {
            skipped,
            skipped_sessions,
            skipped_events,
            accepted_content_records: skipped_events.saturating_add(skipped_file_touches),
            ..ProviderImportSummary::default()
        })
    }

    fn certified_cursor(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
        checkpoint: KimiParserCheckpoint,
    ) -> Result<CertifiedProviderCursor> {
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&checkpoint)?,
        )
    }
}

impl CapturedBatchProjector for KimiCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != KIMI_WIRE_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Kimi projector received an unexpected record kind",
            ));
        }
        let line_number = self
            .advance_to(record.ordinal())
            .map_err(ProviderProjectionFatal::new)?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Kimi projector requires native JSONL bytes",
            ));
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                output.reject_record(line_number, format!("malformed JSONL: {error}"));
                return Ok(());
            }
        };
        let record_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if self.session.started_at.is_none() {
            self.session.started_at = if record_type == "metadata" {
                value
                    .get("created_at")
                    .and_then(Value::as_i64)
                    .and_then(DateTime::<Utc>::from_timestamp_millis)
            } else {
                kimi_record_timestamp(&value, self.context.imported_at)
            };
        }
        output.use_explicit_file_touches();
        self.emit_session_if_needed(line_number, output)?;
        if record_type == "metadata" {
            return Ok(());
        }

        let occurred_at = kimi_record_timestamp(
            &value,
            self.session.started_at.unwrap_or(self.context.imported_at),
        )
        .unwrap_or(self.context.imported_at);
        let path = self.context.source_path.as_deref().ok_or_else(|| {
            ProviderProjectionFatal::system_invariant(
                "Kimi captured import requires its actual wire source path",
            )
        })?;
        let mut event = kimi_event(
            &self.session.provider_session_id,
            line_number,
            &value,
            occurred_at,
            path,
        );
        crate::complete_content::jsonl::attach_exact_jsonl_complete_content_locator(
            &mut event,
            CaptureProvider::KimiCodeCli,
            KIMI_CODE_CLI_SOURCE_FORMAT,
            &value,
            record,
            line_number,
            &self.complete_content_binding,
        )
        .map_err(ProviderProjectionFatal::new)?;
        if let Some((content, native_record_id)) =
            crate::complete_content::jsonl::result_content_and_id(
                CaptureProvider::KimiCodeCli,
                KIMI_CODE_CLI_SOURCE_FORMAT,
                &value,
                line_number,
            )
        {
            crate::complete_content::jsonl::attach_exact_jsonl_result_content_locator(
                &mut event,
                CaptureProvider::KimiCodeCli,
                KIMI_CODE_CLI_SOURCE_FORMAT,
                &content,
                &native_record_id,
                record,
                &self.complete_content_binding,
            )
            .map_err(ProviderProjectionFatal::new)?;
        }
        let raw_source_path = path.to_string_lossy();
        let source_root = self.context.source_root_display();
        emit_projected_normalization_units(
            output,
            ProviderNormalizationResult {
                captures: vec![(
                    line_number,
                    kimi_batched_capture(&self.session, &self.context, Some(event.clone()))
                        .map_err(ProviderProjectionFatal::new)?,
                )],
                ..ProviderNormalizationResult::default()
            },
        )?;
        let file_touch_outcome = visit_provider_file_touches_from_raw_value(
            ProviderFileTouchSourceContext::new(
                CaptureProvider::KimiCodeCli,
                &self.session.provider_session_id,
                KIMI_CODE_CLI_SOURCE_FORMAT,
                Some(raw_source_path.as_ref()),
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
            output.reject_record(line_number, PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned());
        }
        let file_touch_count = u64::try_from(file_touch_outcome.emitted())
            .map_err(|_| {
                CaptureError::SystemInvariant("Kimi projected file-touch count exceeds u64")
            })
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_events = self
            .accepted_events
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Kimi projected event count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touch_count)
            .ok_or(CaptureError::SystemInvariant(
                "Kimi projected file-touch count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        self.certified_cursor(
            source,
            position,
            KimiParserCheckpoint {
                auxiliary_revision: self.session.auxiliary_revision,
                admission_scope_revision: kimi_admission_scope_revision(&self.context),
                started_at: self.session.started_at,
                next_ordinal: 0,
                emitted_session: false,
                accepted_events: 0,
                accepted_file_touches: 0,
            },
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_ordinal = batch
            .records()
            .last()
            .and_then(|record| record.ordinal().checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Kimi captured batch did not have a next ordinal",
            ))?;
        if self.next_ordinal > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Kimi projector advanced beyond the captured batch",
            ));
        }
        Ok(CapturedBatchCursorFinish::Advance(self.certified_cursor(
            batch.source(),
            batch.range_end(),
            KimiParserCheckpoint {
                auxiliary_revision: self.session.auxiliary_revision,
                admission_scope_revision: kimi_admission_scope_revision(&self.context),
                started_at: self.session.started_at,
                next_ordinal,
                emitted_session: self.emitted_session,
                accepted_events: self.accepted_events,
                accepted_file_touches: self.accepted_file_touches,
            },
        )?))
    }
}

fn kimi_batched_capture(
    session: &KimiWireSessionState,
    context: &ProviderAdapterContext,
    event: Option<ctx_history_core::ProviderEventEnvelope>,
) -> Result<ProviderCaptureEnvelope> {
    let path = context
        .source_path
        .as_deref()
        .map(|path| path.display().to_string())
        .ok_or(CaptureError::SystemInvariant(
            "Kimi captured import requires its actual wire source path",
        ))?;
    Ok(native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::KimiCodeCli,
            source_format: KIMI_CODE_CLI_SOURCE_FORMAT,
            provider_session_id: session.provider_session_id.clone(),
            parent_provider_session_id: session.parent_provider_session_id.clone(),
            root_provider_session_id: session.root_provider_session_id.clone(),
            external_agent_id: Some(session.agent_id.clone()),
            agent_type: if session.is_primary {
                AgentType::Primary
            } else {
                AgentType::Subagent
            },
            role_hint: Some(if session.is_primary {
                "main".to_owned()
            } else {
                "subagent".to_owned()
            }),
            is_primary: session.is_primary,
            started_at: session.started_at.unwrap_or(context.imported_at),
            ended_at: session.ended_at,
            cwd: session.cwd.clone(),
            fidelity: Fidelity::Imported,
            raw_source_path: path.clone(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": KIMI_CODE_CLI_SOURCE_FORMAT,
                "source_path": path,
                "session_index": session.index_metadata.clone(),
            }),
            session_metadata: json!({
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "agent_id": session.agent_id.clone(),
                "state": session.state_metadata.clone(),
                "agent_state": session.agent_state_metadata.clone(),
                "title": session.title.clone(),
                "last_prompt": session.last_prompt.clone(),
                "archived": session.archived,
            }),
        },
        context,
        event,
    ))
}
