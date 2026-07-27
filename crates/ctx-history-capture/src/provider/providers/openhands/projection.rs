use std::path::PathBuf;

#[cfg(test)]
use std::cell::Cell;

use ctx_history_core::{
    AgentType, CaptureProvider, Fidelity, ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
    StructuralRejectionKind,
};
use crate::complete_content::structured::{
    attach_structured_complete_content_locator, attach_structured_result_content_locator,
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
use crate::provider::normalization::{
    native_event, native_provider_capture, NativeEventDraft, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportFailure, ProviderImportSummary,
    ProviderNormalizationResult, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
};

use super::event::{decode_openhands_event, OpenHandsDecodedEvent};
use super::source::{
    decode_openhands_position, openhands_conversation_id_from_path, openhands_line_number,
    openhands_user_id_from_path,
};
use super::{
    openhands_bounded_derived_text, openhands_checkpoint_matches_position, OpenHandsEventIdentity,
    OpenHandsParserCheckpoint, OpenHandsProjectionMode, OPENHANDS_CAPTURED_BATCH_PROJECTION_MARKER,
    OPENHANDS_LOCATOR_KIND, OPENHANDS_MAX_FAILURE_BYTES, OPENHANDS_RECORD_KIND,
};

pub(super) struct OpenHandsCapturedBatchProjector {
    context: ProviderAdapterContext,
    event_path: PathBuf,
    conversation_dir: PathBuf,
    session_id: String,
    identity: OpenHandsEventIdentity,
    mode: OpenHandsProjectionMode,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejection: Option<ProviderImportFailure>,
}

impl OpenHandsCapturedBatchProjector {
    pub(super) fn fresh(
        context: ProviderAdapterContext,
        event_path: PathBuf,
        conversation_dir: PathBuf,
        session_id: String,
        identity: OpenHandsEventIdentity,
        mode: OpenHandsProjectionMode,
    ) -> Self {
        Self {
            context,
            event_path,
            conversation_dir,
            session_id,
            identity,
            mode,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejection: None,
        }
    }

    pub(super) fn resume(
        context: ProviderAdapterContext,
        event_path: PathBuf,
        conversation_dir: PathBuf,
        session_id: String,
        identity: OpenHandsEventIdentity,
        mode: OpenHandsProjectionMode,
        cursor: &CertifiedProviderCursor,
    ) -> Result<Self> {
        let checkpoint: OpenHandsParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        let position = decode_openhands_position(cursor.native_position())?;
        if !openhands_checkpoint_matches_position(&checkpoint, position, &event_path) {
            return Err(CaptureError::InvalidPayload(
                "OpenHands parser checkpoint does not match its event-file position".to_owned(),
            ));
        }
        Ok(Self {
            context,
            event_path,
            conversation_dir,
            session_id,
            identity,
            mode,
            accepted_events: checkpoint.accepted_events,
            accepted_file_touches: checkpoint.accepted_file_touches,
            rejection: checkpoint.rejection,
        })
    }

    fn reject_record(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        line: usize,
        reason: String,
    ) {
        let failure = ProviderImportFailure {
            line,
            error: bounded_openhands_failure(reason),
        };
        output.reject_record(failure.line, failure.error.clone());
        self.rejection = Some(failure);
    }

    pub(super) fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("OpenHands replay event count exceeds platform limits")
        })?;
        let skipped_file_touches = usize::try_from(self.accepted_file_touches).map_err(|_| {
            CaptureError::SystemInvariant(
                "OpenHands replay file-touch count exceeds platform limits",
            )
        })?;
        let skipped_sessions = usize::from(skipped_events != 0);
        let accepted_content_records = skipped_events.checked_add(skipped_file_touches).ok_or(
            CaptureError::SystemInvariant("OpenHands replay accepted-content count overflowed"),
        )?;
        let skipped = skipped_sessions
            .checked_add(accepted_content_records)
            .ok_or(CaptureError::SystemInvariant(
                "OpenHands replay summary count overflowed",
            ))?;
        let failures = self.rejection.iter().cloned().collect::<Vec<_>>();
        Ok(ProviderImportSummary {
            skipped,
            failed: failures.len(),
            skipped_sessions,
            skipped_events,
            accepted_content_records,
            failures,
            ..ProviderImportSummary::default()
        })
    }

    fn checkpoint_rejection(&self, batch: &CapturedBatch) -> Option<ProviderImportFailure> {
        self.rejection.clone().or_else(|| {
            batch.records().iter().find_map(|record| match record.payload() {
                CapturedRecordPayload::StructuralRejection {
                    kind: StructuralRejectionKind::OversizeRecord,
                    observed_bytes,
                } => Some(ProviderImportFailure {
                    line: openhands_line_number(&self.event_path),
                    error: format!(
                        "provider record exceeds the {} byte limit (observed {observed_bytes} bytes)",
                        MAX_PROVIDER_JSONL_LINE_BYTES
                    ),
                }),
                CapturedRecordPayload::NativeBytes(_) | CapturedRecordPayload::SqliteValues(_) => {
                    None
                }
            })
        })
    }
}

impl CapturedBatchProjector for OpenHandsCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != OPENHANDS_RECORD_KIND
            || record.locator().kind() != OPENHANDS_LOCATOR_KIND
        {
            return Err(ProviderProjectionFatal::system_invariant(
                "OpenHands projector received an unexpected record shape",
            ));
        }
        let path = std::str::from_utf8(record.locator().value())
            .map(PathBuf::from)
            .map_err(|_| {
                ProviderProjectionFatal::new(CaptureError::InvalidPayload(
                    "OpenHands event locator is not valid UTF-8".to_owned(),
                ))
            })?;
        if path != self.event_path {
            return Err(ProviderProjectionFatal::new(
                CaptureError::SourceChangedDuringCapture,
            ));
        }
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "OpenHands projector requires exact native event JSON bytes",
            ));
        };
        let line_number = openhands_line_number(&path);
        let decoded = match decode_openhands_event(&path, bytes) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.reject_record(output, line_number, error.to_string());
                return Ok(());
            }
        };
        let mut timestamp = decoded.timestamp();
        if openhands_conversation_id_from_path(&path).as_deref() != Some(self.session_id.as_str()) {
            return Err(ProviderProjectionFatal::new(
                CaptureError::SourceChangedDuringCapture,
            ));
        }
        if self.accepted_events != 0 || self.rejection.is_some() {
            return Err(ProviderProjectionFatal::system_invariant(
                "OpenHands event-file projector accepted more than one record",
            ));
        }
        if self.mode == OpenHandsProjectionMode::ExistingStableNoop {
            emit_projected_normalization_units(
                output,
                ProviderNormalizationResult {
                    summary: ProviderImportSummary {
                        skipped: 2,
                        skipped_sessions: 1,
                        skipped_events: 1,
                        accepted_content_records: 1,
                        ..ProviderImportSummary::default()
                    },
                    ..ProviderNormalizationResult::default()
                },
            )?;
            self.accepted_events = 1;
            return Ok(());
        }
        if let OpenHandsProjectionMode::LegacyUpgrade { occurred_at, .. } = &self.mode {
            timestamp = *occurred_at;
        }
        let user_id = match openhands_user_id_from_path(&path)
            .map(|value| openhands_bounded_derived_text(value, "user id"))
            .transpose()
        {
            Ok(user_id) => user_id,
            Err(error) => {
                self.reject_record(output, line_number, error.to_string());
                return Ok(());
            }
        };
        let verified_legacy_provider_event_index = match &self.mode {
            OpenHandsProjectionMode::LegacyUpgrade {
                provider_event_index,
                ..
            } => Some(*provider_event_index),
            OpenHandsProjectionMode::Full
            | OpenHandsProjectionMode::ExistingStableNoop
            | OpenHandsProjectionMode::ExistingStableUpgrade
            | OpenHandsProjectionMode::ExistingStableRepair => None,
        };
        let mut event = openhands_provider_event_with_identity(
            &self.session_id,
            &path,
            &decoded,
            timestamp,
            self.identity,
            verified_legacy_provider_event_index,
        );
        attach_structured_complete_content_locator(
            CaptureProvider::OpenHands,
            &mut event,
            record.ordinal(),
            0,
            decoded.event_id(),
            bytes,
            decoded.text(),
        )
        .map_err(ProviderProjectionFatal::new)?;
        if let Some(content) = super::openhands_result_content(&decoded) {
            attach_structured_result_content_locator(
                CaptureProvider::OpenHands,
                &mut event,
                record.ordinal(),
                0,
                decoded.event_id(),
                bytes,
                &content,
            )
            .map_err(ProviderProjectionFatal::new)?;
        }
        let raw_source_path = path.display().to_string();
        let conversation_dir = self.conversation_dir.display().to_string();
        let extract_file_touches = !matches!(
            &self.mode,
            OpenHandsProjectionMode::ExistingStableUpgrade
                | OpenHandsProjectionMode::LegacyUpgrade { .. }
        );
        let source_root = self.context.source_root_display();
        let mut capture = native_provider_capture(
            NativeSessionDraft {
                provider: CaptureProvider::OpenHands,
                source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                provider_session_id: self.session_id.clone(),
                parent_provider_session_id: None,
                root_provider_session_id: None,
                external_agent_id: user_id.clone(),
                agent_type: AgentType::Primary,
                role_hint: Some("primary".to_owned()),
                is_primary: true,
                started_at: timestamp,
                ended_at: Some(timestamp),
                cwd: None,
                fidelity: Fidelity::Imported,
                raw_source_path: raw_source_path.clone(),
                trust: ProviderSourceTrust::ProviderNative,
                source_metadata: json!({
                    "adapter": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    "storage": "filesystem_event_service",
                    "conversation_dir": conversation_dir,
                    "event_path": raw_source_path,
                    "event_file_identity": format!(
                        "{:016x}",
                        self.identity.canonical_path_hash
                    ),
                    "captured_file_event_count": 1,
                }),
                session_metadata: json!({
                    "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    "provider": "openhands",
                    "conversation_id": self.session_id,
                    "user_id": user_id,
                }),
            },
            &self.context,
            Some(event.clone()),
        );
        if extract_file_touches {
            capture.source.metadata["captured_batch_projection"] =
                json!(OPENHANDS_CAPTURED_BATCH_PROJECTION_MARKER);
        }
        if let OpenHandsProjectionMode::LegacyUpgrade { session, .. } = &self.mode {
            capture.session.external_agent_id = session.external_agent_id.clone();
            capture.session.agent_type = session.agent_type;
            capture.session.role_hint = session.role_hint.clone();
            capture.session.is_primary = session.is_primary;
            capture.session.status = session.status;
            capture.session.fidelity = session.fidelity;
            capture.session.metadata.clone_from(&session.metadata);
        }
        output.use_explicit_file_touches();
        emit_projected_normalization_units(
            output,
            ProviderNormalizationResult {
                captures: vec![(line_number, capture)],
                ..ProviderNormalizationResult::default()
            },
        )?;
        let file_touch_outcome = if extract_file_touches {
            visit_provider_file_touches_from_raw_value(
                ProviderFileTouchSourceContext::new(
                    CaptureProvider::OpenHands,
                    &self.session_id,
                    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
                    Some(raw_source_path.as_str()),
                    source_root.as_deref(),
                ),
                decoded.value(),
                &event,
                line_number,
                |file_touch| {
                    output.emit_normalization(ProviderNormalizationResult {
                        files_touched: vec![file_touch],
                        ..ProviderNormalizationResult::default()
                    })?;
                    #[cfg(test)]
                    maybe_inject_openhands_post_touch_failure()?;
                    Ok(())
                },
            )?
        } else {
            crate::provider::file_touches::ProviderFileTouchVisitOutcome::empty()
        };
        if file_touch_outcome.limit_exceeded() {
            self.reject_record(
                output,
                line_number,
                PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
            );
        }
        let file_touch_count = u64::try_from(file_touch_outcome.emitted()).map_err(|_| {
            ProviderProjectionFatal::system_invariant(
                "OpenHands projected file-touch count exceeds u64",
            )
        })?;
        self.accepted_events = 1;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touch_count)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "OpenHands accepted file-touch count overflowed",
                )
            })?;
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        let next_position = decode_openhands_position(position)?;
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&OpenHandsParserCheckpoint {
                next_position,
                accepted_events: 0,
                accepted_file_touches: 0,
                rejection: None,
            })?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_position = decode_openhands_position(batch.range_end())?;
        CertifiedProviderCursor::new(
            batch.source().source_revision(),
            batch.source().capture_revision(),
            batch.source().policy_revision(),
            batch.range_end().clone(),
            BoundedParserCheckpoint::from_serializable(&OpenHandsParserCheckpoint {
                next_position,
                accepted_events: self.accepted_events,
                accepted_file_touches: self.accepted_file_touches,
                rejection: self.checkpoint_rejection(batch),
            })?,
        )
        .map(CapturedBatchCursorFinish::Advance)
    }
}

pub(super) fn openhands_provider_event_with_identity(
    session_id: &str,
    event_path: &std::path::Path,
    decoded: &OpenHandsDecodedEvent,
    occurred_at: chrono::DateTime<chrono::Utc>,
    identity: OpenHandsEventIdentity,
    verified_legacy_provider_event_index: Option<u64>,
) -> ProviderEventEnvelope {
    let legacy_source_event_candidate = verified_legacy_provider_event_index
        .zip(event_path.parent())
        .map(|(provider_event_index, conversation_dir)| {
            json!({
                "raw_source_path": conversation_dir.display().to_string(),
                "provider_event_index": provider_event_index,
            })
        });
    native_event(NativeEventDraft {
        provider: CaptureProvider::OpenHands,
        source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        provider_session_id: session_id.to_owned(),
        provider_event_index: identity.provider_event_index,
        provider_event_hash: Some(decoded.event_id().to_owned()),
        cursor: format!("{}:{}", event_path.display(), decoded.event_id()),
        event_type: decoded.event_type(),
        role: Some(decoded.role()),
        occurred_at,
        text: decoded.text().to_owned(),
        body: decoded.value().clone(),
        metadata: json!({
            "source": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "source_format": OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            "event_id": decoded.event_id(),
            "entry_type": decoded.entry_type(),
            "event_path": event_path.display().to_string(),
            "conversation_id": session_id,
            "provider_event_identity_index": identity.provider_event_identity_index,
            "event_file_identity": format!("{:016x}", identity.canonical_path_hash),
            "legacy_source_event_candidate_v1": legacy_source_event_candidate,
            "tool_name": decoded.value().get("tool_name").and_then(Value::as_str),
            "tool_call_id": decoded.value().get("tool_call_id").and_then(Value::as_str),
            "action_id": decoded.value().get("action_id").and_then(Value::as_str),
        }),
    })
}

fn bounded_openhands_failure(mut failure: String) -> String {
    if failure.len() <= OPENHANDS_MAX_FAILURE_BYTES {
        return failure;
    }
    let mut boundary = OPENHANDS_MAX_FAILURE_BYTES;
    while !failure.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    failure.truncate(boundary);
    failure
}

#[cfg(test)]
std::thread_local! {
    static OPENHANDS_FAIL_AFTER_FILE_TOUCH_COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

#[cfg(test)]
pub(super) fn with_openhands_post_touch_failure<T>(
    touches_before_failure: usize,
    operation: impl FnOnce() -> T,
) -> T {
    OPENHANDS_FAIL_AFTER_FILE_TOUCH_COUNT.with(|remaining| {
        assert_eq!(remaining.replace(Some(touches_before_failure)), None);
    });
    let output = operation();
    OPENHANDS_FAIL_AFTER_FILE_TOUCH_COUNT.with(|remaining| remaining.set(None));
    output
}

#[cfg(test)]
fn maybe_inject_openhands_post_touch_failure() -> ProviderProjectionResult<()> {
    let should_fail = OPENHANDS_FAIL_AFTER_FILE_TOUCH_COUNT.with(|remaining| {
        let Some(current) = remaining.get() else {
            return false;
        };
        if current <= 1 {
            remaining.set(None);
            true
        } else {
            remaining.set(Some(current - 1));
            false
        }
    });
    if should_fail {
        return Err(ProviderProjectionFatal::system_invariant(
            "injected OpenHands post-touch projection failure",
        ));
    }
    Ok(())
}
