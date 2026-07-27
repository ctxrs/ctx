use std::collections::BTreeMap;

use ctx_history_core::{CaptureProvider, EventType};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::captured_batch::jsonl::{jsonl_locator_range, jsonl_position_offset};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
};
use crate::provider::codex::events::{codex_session_line_timestamp, codex_value_is_tool_call};
use crate::provider::file_touches::{
    event_type_supports_structured_file_touches, visit_provider_file_touches_with_context,
    ProviderFileTouchEnvelopeContext, PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    emit_projected_normalization_units, BoundedParserCheckpoint, CapturedBatchCursorFinish,
    CapturedBatchProjector, CertifiedProviderCursor, ProviderProjectionFatal,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportSummary, ProviderNormalizationResult,
    Result, CODEX_SESSION_SOURCE_FORMAT,
};

use super::correlation::{CodexToolCallCheckpoint, CodexToolCorrelation};
use super::filter::should_parse_codex_session_line;
use super::header::{
    bounded_codex_header, codex_session_capture, codex_session_header, CodexSessionHeader,
};
use super::import::codex_jsonl_batch_error;
use super::resume::{codex_header_anchor, CodexHeaderAnchor};
use super::CODEX_RECORD_KIND;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexParserCheckpoint {
    #[serde(rename = "h", alias = "header_anchor")]
    pub(super) header_anchor: Option<CodexHeaderAnchor>,
    #[serde(rename = "c", alias = "call_contexts")]
    pub(super) call_contexts: BTreeMap<String, CodexToolCallCheckpoint>,
    #[serde(rename = "n", alias = "next_ordinal")]
    pub(super) next_ordinal: u64,
    #[serde(rename = "a", alias = "accepted_captures")]
    pub(super) accepted_captures: u64,
    #[serde(rename = "i", alias = "accepted_sessions", default)]
    pub(super) accepted_sessions: u64,
    #[serde(rename = "x", alias = "current_header_has_capture", default)]
    pub(super) current_header_has_capture: bool,
    #[serde(rename = "e", alias = "accepted_events")]
    pub(super) accepted_events: u64,
    #[serde(rename = "f", alias = "accepted_file_touches")]
    pub(super) accepted_file_touches: u64,
    #[serde(rename = "s", alias = "policy_skipped_events")]
    pub(super) policy_skipped_events: u64,
}
pub(super) struct CodexCapturedBatchProjector {
    pub(super) context: ProviderAdapterContext,
    pub(super) header: Option<CodexSessionHeader>,
    pub(super) header_anchor: Option<CodexHeaderAnchor>,
    pub(super) correlation: CodexToolCorrelation,
    pub(super) next_ordinal: u64,
    pub(super) accepted_captures: u64,
    pub(super) accepted_sessions: u64,
    pub(super) current_header_has_capture: bool,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
    pub(super) policy_skipped_events: u64,
    pub(super) replay_rejected_records: u64,
}

impl CodexCapturedBatchProjector {
    pub(super) fn fresh(context: ProviderAdapterContext) -> Self {
        Self {
            context,
            header: None,
            header_anchor: None,
            correlation: CodexToolCorrelation::fresh(),
            next_ordinal: 0,
            accepted_captures: 0,
            accepted_sessions: 0,
            current_header_has_capture: false,
            accepted_events: 0,
            accepted_file_touches: 0,
            policy_skipped_events: 0,
            replay_rejected_records: 0,
        }
    }

    pub(super) fn resume(
        context: ProviderAdapterContext,
        cursor: &CertifiedProviderCursor,
        header: Option<CodexSessionHeader>,
    ) -> Result<Self> {
        let checkpoint: CodexParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        if header.is_none()
            && (checkpoint.accepted_captures != 0
                || checkpoint.accepted_sessions != 0
                || checkpoint.current_header_has_capture
                || checkpoint.accepted_events != 0
                || checkpoint.accepted_file_touches != 0
                || !checkpoint.call_contexts.is_empty())
        {
            return Err(CaptureError::InvalidPayload(
                "headerless Codex checkpoint contains header-dependent parser state".to_owned(),
            ));
        }
        Ok(Self {
            context,
            header: header.map(bounded_codex_header).transpose()?,
            header_anchor: checkpoint.header_anchor,
            correlation: CodexToolCorrelation::from_checkpoint(checkpoint.call_contexts),
            next_ordinal: checkpoint.next_ordinal,
            accepted_captures: checkpoint.accepted_captures,
            accepted_sessions: checkpoint.accepted_sessions,
            current_header_has_capture: checkpoint.current_header_has_capture,
            accepted_events: checkpoint.accepted_events,
            accepted_file_touches: checkpoint.accepted_file_touches,
            policy_skipped_events: checkpoint.policy_skipped_events,
            replay_rejected_records: cursor.rejected_records(),
        })
    }

    fn line_number(&mut self, ordinal: u64) -> Result<usize> {
        if ordinal < self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Codex captured record ordinal moved backwards",
            ));
        }
        self.next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Codex captured record ordinal overflowed",
        ))?;
        usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Codex captured record ordinal exceeds platform limits",
            ))
    }

    pub(super) fn checkpoint(&self, next_ordinal: u64) -> CodexParserCheckpoint {
        CodexParserCheckpoint {
            header_anchor: self.header_anchor.clone(),
            call_contexts: self.correlation.checkpoint(),
            next_ordinal,
            accepted_captures: self.accepted_captures,
            accepted_sessions: self.accepted_sessions,
            current_header_has_capture: self.current_header_has_capture,
            accepted_events: self.accepted_events,
            accepted_file_touches: self.accepted_file_touches,
            policy_skipped_events: self.policy_skipped_events,
        }
    }

    fn accept(
        &mut self,
        mut normalization: ProviderNormalizationResult,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        // The first accepted capture establishes the exact source-scoped
        // session. Later event-only records must not rebuild and upsert that
        // identical source/session envelope. Keep the normalized capture for
        // validation, but persist only its event against the already-admitted
        // session.
        let establishes_current_session = !normalization.captures.is_empty();
        let emit_against_existing_session = self.current_header_has_capture
            && normalization.summary == ProviderImportSummary::default()
            && normalization.captures.len() == 1
            && normalization.files_touched.is_empty()
            && normalization.captures[0].1.event.is_some();
        let captures = u64::try_from(normalization.captures.len())
            .map_err(|_| CaptureError::SystemInvariant("Codex capture count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        let events = u64::try_from(
            normalization
                .captures
                .iter()
                .filter(|(_, capture)| capture.event.is_some())
                .count(),
        )
        .map_err(|_| CaptureError::SystemInvariant("Codex event count exceeds u64"))
        .map_err(ProviderProjectionFatal::new)?;
        let file_touches = u64::try_from(normalization.files_touched.len())
            .map_err(|_| CaptureError::SystemInvariant("Codex file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        let policy_skipped_events = u64::try_from(normalization.summary.skipped_events)
            .map_err(|_| CaptureError::SystemInvariant("Codex skipped event count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_captures = self
            .accepted_captures
            .checked_add(captures)
            .ok_or(CaptureError::SystemInvariant(
                "Codex capture count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_events = self
            .accepted_events
            .checked_add(events)
            .ok_or(CaptureError::SystemInvariant(
                "Codex event count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touches)
            .ok_or(CaptureError::SystemInvariant(
                "Codex file-touch count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.policy_skipped_events = self
            .policy_skipped_events
            .checked_add(policy_skipped_events)
            .ok_or(CaptureError::SystemInvariant(
                "Codex skipped event count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        if emit_against_existing_session {
            let (line_number, capture) = normalization.captures.pop().ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Codex existing-session projection lost its event capture",
                )
            })?;
            output.emit_existing_session_event(line_number, capture)?;
            return Ok(());
        }
        emit_projected_normalization_units(output, normalization)?;
        if establishes_current_session && !self.current_header_has_capture {
            self.accepted_sessions = self.accepted_sessions.checked_add(1).ok_or_else(|| {
                ProviderProjectionFatal::system_invariant("Codex accepted session count overflowed")
            })?;
            self.current_header_has_capture = true;
        }
        Ok(())
    }

    fn reject_record(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        line_number: usize,
        reason: String,
    ) -> ProviderProjectionResult<()> {
        output.reject_record(line_number, reason);
        Ok(())
    }

    pub(super) fn bound_call_contexts(&mut self) {
        self.correlation.bound_retained_contexts();
        while BoundedParserCheckpoint::from_serializable(&self.checkpoint(self.next_ordinal))
            .is_err()
        {
            if !self.correlation.drop_oldest() {
                break;
            }
        }
    }

    pub(super) fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::try_from(self.accepted_sessions).map_err(|_| {
            CaptureError::SystemInvariant("Codex replay session count exceeds platform limits")
        })?;
        let accepted_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("Codex replay event count exceeds platform limits")
        })?;
        let policy_skipped_events = usize::try_from(self.policy_skipped_events).map_err(|_| {
            CaptureError::SystemInvariant(
                "Codex replay skipped event count exceeds platform limits",
            )
        })?;
        let skipped_events = accepted_events.checked_add(policy_skipped_events).ok_or(
            CaptureError::SystemInvariant("Codex replay skipped event count overflowed"),
        )?;
        let skipped_file_touches = usize::try_from(self.accepted_file_touches).map_err(|_| {
            CaptureError::SystemInvariant("Codex replay file-touch count exceeds platform limits")
        })?;
        let skipped = skipped_sessions
            .checked_add(skipped_events)
            .and_then(|value| value.checked_add(skipped_file_touches))
            .ok_or(CaptureError::SystemInvariant(
                "Codex replay summary count overflowed",
            ))?;
        let failed = usize::try_from(self.replay_rejected_records).map_err(|_| {
            CaptureError::SystemInvariant("Codex replay rejection count exceeds platform limits")
        })?;
        Ok(ProviderImportSummary {
            skipped,
            failed,
            skipped_sessions,
            skipped_events,
            accepted_content_records: accepted_events.saturating_add(skipped_file_touches),
            ..ProviderImportSummary::default()
        })
    }
}

impl CapturedBatchProjector for CodexCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != CODEX_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Codex projector received an unexpected record kind",
            ));
        }
        let line_number = self
            .line_number(record.ordinal())
            .map_err(ProviderProjectionFatal::new)?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Codex projector requires native JSONL bytes",
            ));
        };
        if bytes.iter().all(u8::is_ascii_whitespace) || !should_parse_codex_session_line(bytes) {
            return Ok(());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                return self.reject_record(output, line_number, error.to_string());
            }
        };
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            return match codex_session_header(value) {
                Ok(header) => {
                    self.correlation.clear();
                    if self.header.as_ref().is_some_and(|owner| {
                        owner.root_session.is_some() || owner.parent_session.is_some()
                    }) {
                        // Modern Codex rollouts start with a lineage-bearing owning thread and may
                        // then replay ancestor session_meta records as inherited context. Either a
                        // root or parent relationship establishes that ownership, so later headers
                        // are non-owning. Legacy rollouts without either signal retain their
                        // multi-header behavior.
                        return Ok(());
                    }
                    let (start_offset, end_offset) = jsonl_locator_range(record.locator())
                        .map_err(codex_jsonl_batch_error)
                        .map_err(ProviderProjectionFatal::new)?;
                    let header_anchor = codex_header_anchor(start_offset, end_offset, bytes)
                        .map_err(ProviderProjectionFatal::new)?;
                    self.header = match bounded_codex_header(header) {
                        Ok(header) => Some(header),
                        Err(_) => {
                            return self.reject_record(
                                output,
                                line_number,
                                "Codex session metadata exceeds the bounded parser state limit"
                                    .to_owned(),
                            );
                        }
                    };
                    self.header_anchor = Some(header_anchor);
                    // Codex compaction/fork flows can concatenate multiple native sessions in
                    // one rollout file. The first accepted unit after each header must persist
                    // that exact source-scoped session before events or file touches may use it.
                    self.current_header_has_capture = false;
                    // Do not persist a session until this source yields accepted content. A later
                    // event capture includes the same session envelope, and file-touch-only
                    // content is paired with one below. This keeps an all-rejected source from
                    // attaching session scaffolding to a newly-created history record.
                    Ok(())
                }
                Err(error) => self.reject_record(output, line_number, error.to_string()),
            };
        }

        let Some(header) = self.header.clone() else {
            return self.reject_record(
                output,
                line_number,
                "codex session entry appeared before session_meta".to_owned(),
            );
        };
        let occurred_at = match codex_session_line_timestamp(&value, header.timestamp) {
            Ok(occurred_at) => occurred_at,
            Err(error) => return self.reject_record(output, line_number, error.to_string()),
        };
        output.use_explicit_file_touches();
        let raw_source_path = self
            .context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string());
        let source_root = self.context.source_root_display();
        let mut raw_event = self.correlation.event(&value, line_number, occurred_at);
        if let Some(projected) = raw_event.as_mut() {
            crate::complete_content::jsonl::attach_jsonl_complete_content_locator(
                &mut projected.event,
                CaptureProvider::Codex,
                CODEX_SESSION_SOURCE_FORMAT,
                &value,
                record,
                line_number,
            )
            .map_err(ProviderProjectionFatal::new)?;
            if let Some(content_ref) = projected.result_content_ref.as_ref() {
                crate::complete_content::jsonl::attach_codex_result_content_locator(
                    &mut projected.event,
                    content_ref,
                    record,
                    line_number,
                )
                .map_err(ProviderProjectionFatal::new)?;
            }
        }
        self.bound_call_contexts();
        let skipped_notice = usize::from(
            raw_event
                .as_ref()
                .is_some_and(|projected| projected.event.event_type == EventType::Notice),
        );
        let event = raw_event
            .as_ref()
            .filter(|projected| projected.event.event_type != EventType::Notice)
            .map(|projected| projected.event.clone());
        let mut capture_emitted = event.is_some();
        let captures = event
            .map(|event| {
                vec![(
                    line_number,
                    codex_session_capture(
                        &header,
                        Some(event),
                        line_number,
                        occurred_at,
                        &self.context,
                    ),
                )]
            })
            .unwrap_or_default();
        self.accept(
            ProviderNormalizationResult {
                summary: ProviderImportSummary {
                    skipped: skipped_notice,
                    skipped_events: skipped_notice,
                    ..ProviderImportSummary::default()
                },
                captures,
                ..ProviderNormalizationResult::default()
            },
            output,
        )?;
        let include_structured_touches = raw_event.as_ref().is_some_and(|projected| {
            event_type_supports_structured_file_touches(projected.event.event_type)
        }) || codex_value_is_tool_call(&value);
        let touch_outcome = visit_provider_file_touches_with_context(
            ProviderFileTouchEnvelopeContext {
                provider: CaptureProvider::Codex,
                provider_session_id: &header.id,
                source_format: CODEX_SESSION_SOURCE_FORMAT,
                raw_source_path: raw_source_path.as_deref(),
                source_root: source_root.as_deref(),
                occurred_at,
                provider_event_index: raw_event
                    .as_ref()
                    .map(|projected| projected.event.provider_event_index),
                provider_touch_base_index: (line_number as u64) << 16,
                line_number,
            },
            &value,
            include_structured_touches,
            |file_touch| {
                if !capture_emitted {
                    self.accept(
                        ProviderNormalizationResult {
                            captures: vec![(
                                line_number,
                                codex_session_capture(
                                    &header,
                                    None,
                                    line_number,
                                    occurred_at,
                                    &self.context,
                                ),
                            )],
                            ..ProviderNormalizationResult::default()
                        },
                        output,
                    )?;
                    capture_emitted = true;
                }
                self.accept(
                    ProviderNormalizationResult {
                        files_touched: vec![file_touch],
                        ..ProviderNormalizationResult::default()
                    },
                    output,
                )
            },
        )?;
        if touch_outcome.limit_exceeded() {
            self.reject_record(
                output,
                line_number,
                PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
            )?;
        }
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if !self.correlation.is_empty()
            || self.accepted_captures != 0
            || self.accepted_sessions != 0
            || self.current_header_has_capture
            || self.accepted_events != 0
            || self.accepted_file_touches != 0
            || self.policy_skipped_events != 0
            || self.replay_rejected_records != 0
        {
            return Err(CaptureError::SystemInvariant(
                "Codex initial cursor candidate requires unprojected state",
            ));
        }
        let offset = jsonl_position_offset(position).map_err(codex_jsonl_batch_error)?;
        if offset == 0 {
            if self.next_ordinal != 0 || self.header.is_some() || self.header_anchor.is_some() {
                return Err(CaptureError::SystemInvariant(
                    "Codex source-start cursor candidate requires fresh parser state",
                ));
            }
        } else if self.next_ordinal == 0 || self.header.is_none() || self.header_anchor.is_none() {
            return Err(CaptureError::SystemInvariant(
                "Codex tail cursor candidate requires bootstrapped session_meta state",
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&self.checkpoint(self.next_ordinal))?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_ordinal = batch
            .records()
            .last()
            .and_then(|record| record.ordinal().checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Codex captured batch did not have a next ordinal",
            ))?;
        if self.next_ordinal > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Codex projector advanced beyond the captured batch",
            ));
        }
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&self.checkpoint(next_ordinal))?,
            )?,
        ))
    }
}
