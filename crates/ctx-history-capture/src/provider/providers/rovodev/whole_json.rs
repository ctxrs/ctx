use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Fidelity, ProviderCaptureEnvelope, ProviderEventEnvelope,
    ProviderSourceTrust,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
};
use crate::complete_content::structured::attach_structured_complete_content_locator;
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
    native_provider_capture, provider_block_text, provider_capped_json_value,
    provider_string_field, provider_timestamp_from_fields, NativeSessionDraft,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportFailure, ProviderImportSummary,
    ProviderNormalizationResult, Result, PROVIDER_MAX_PREVIEW_CHARS, ROVODEV_SOURCE_FORMAT,
};

use super::event::rovodev_event;
use super::source::RovoDevSessionSource;
use super::{rovodev_captured_batch_error, ROVODEV_RECORD_KIND};

const WHOLE_JSON_POSITION_KIND: &str = "whole-json-item-v1";
const MAX_ROVODEV_CHECKPOINT_FAILURES: usize = 4;
const MAX_ROVODEV_FAILURE_BYTES: usize = 4 * 1024;
// Keep recursive preview projection within the plan's conservative whole-JSON ceiling.
// The root is depth zero, so values through 128 child edges are accepted.
const ROVODEV_WHOLE_JSON_MAX_DEPTH: usize = 128;
// Count every array element and object entry, including transcript arrays omitted from previews.
// This matches existing structured-JSON and file-touch ceilings and is cumulative per document.
pub(super) const ROVODEV_WHOLE_JSON_MAX_COLLECTION_ELEMENTS: usize = 65_536;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RovoDevParserCheckpoint {
    next_ordinal: u64,
    accepted_sessions: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    failures: Vec<RovoDevCheckpointFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RovoDevCheckpointFailure {
    line: usize,
    error: String,
}

struct RovoDevProjectionDocument<'a> {
    context_json: &'a Value,
    metadata: &'a Value,
    context_metadata: &'a Value,
    metadata_preview: &'a Value,
    messages: &'a [Value],
    record_bytes: &'a [u8],
}

fn visit_rovodev_session_normalizations(
    source: &RovoDevSessionSource,
    document: RovoDevProjectionDocument<'_>,
    context: &ProviderAdapterContext,
    mut visit: impl FnMut(RovoDevProjectionUnit) -> ProviderProjectionResult<()>,
) -> ProviderProjectionResult<(usize, u64)> {
    let RovoDevProjectionDocument {
        context_json,
        metadata,
        context_metadata,
        metadata_preview,
        messages,
        record_bytes,
    } = document;
    let provider_session_id = provider_string_field(metadata, &["session_id", "sessionId"])
        .or_else(|| provider_string_field(context_json, &["session_id", "sessionId"]))
        .unwrap_or_else(|| source.provider_session_id.clone());
    let parent_provider_session_id = provider_string_field(
        metadata,
        &[
            "parent_session_id",
            "parentSessionId",
            "forked_from_session_id",
            "forkedFromSessionId",
            "fork_parent_id",
        ],
    );
    let started_at = provider_timestamp_from_fields(
        metadata,
        &["created_at", "createdAt", "started_at", "startedAt"],
    )
    .or_else(|| messages.iter().find_map(rovodev_message_timestamp))
    .unwrap_or(context.imported_at);
    let ended_at = provider_timestamp_from_fields(
        metadata,
        &["updated_at", "updatedAt", "last_updated", "lastUpdated"],
    )
    .or_else(|| messages.iter().rev().find_map(rovodev_message_timestamp));
    let cwd = provider_string_field(
        metadata,
        &[
            "workspace_path",
            "workspacePath",
            "working_directory",
            "workingDirectory",
            "cwd",
        ],
    );
    let raw_source_path = source.context_path.display().to_string();

    if messages.is_empty() {
        visit(RovoDevProjectionUnit::Normalization(
            ProviderNormalizationResult {
                captures: vec![(
                    0,
                    rovodev_capture(
                        RovoDevCaptureDraft {
                            provider_session_id,
                            parent_provider_session_id,
                            started_at,
                            ended_at,
                            cwd,
                            source,
                            context_metadata,
                            metadata,
                            metadata_preview,
                            message_count: 0,
                            event: None,
                        },
                        context,
                    ),
                )],
                ..ProviderNormalizationResult::default()
            },
        ))?;
        return Ok((0, 0));
    }

    let message_count = messages.len();
    let mut file_touch_count = 0_usize;
    let mut limit_rejections = 0_u64;
    for (index, message) in messages.iter().enumerate() {
        let line = index + 1;
        let occurred_at = rovodev_message_timestamp(message).unwrap_or(started_at);
        let mut event = rovodev_event(
            &provider_session_id,
            index as u64,
            message,
            occurred_at,
            source,
        );
        if let Some(complete_text) = provider_block_text(message) {
            let native_id = event.provider_event_hash.clone().unwrap_or_default();
            attach_structured_complete_content_locator(
                CaptureProvider::RovoDev,
                &mut event,
                0,
                u32::try_from(index).map_err(|_| {
                    ProviderProjectionFatal::system_invariant(
                        "Rovo Dev subrecord index exceeds u32",
                    )
                })?,
                &native_id,
                record_bytes,
                &complete_text,
            )
            .map_err(ProviderProjectionFatal::new)?;
        }
        let source_root = context.source_root_display();
        visit(RovoDevProjectionUnit::UseExplicitFileTouches)?;
        visit(RovoDevProjectionUnit::Normalization(
            ProviderNormalizationResult {
                captures: vec![(
                    line,
                    rovodev_capture(
                        RovoDevCaptureDraft {
                            provider_session_id: provider_session_id.clone(),
                            parent_provider_session_id: parent_provider_session_id.clone(),
                            started_at,
                            ended_at,
                            cwd: cwd.clone(),
                            source,
                            context_metadata,
                            metadata,
                            metadata_preview,
                            message_count,
                            event: Some(event.clone()),
                        },
                        context,
                    ),
                )],
                ..ProviderNormalizationResult::default()
            },
        ))?;
        let file_touch_outcome = visit_provider_file_touches_from_raw_value(
            ProviderFileTouchSourceContext::new(
                CaptureProvider::RovoDev,
                &provider_session_id,
                ROVODEV_SOURCE_FORMAT,
                Some(raw_source_path.as_str()),
                source_root.as_deref(),
            ),
            message,
            &event,
            line,
            |file_touch| {
                visit(RovoDevProjectionUnit::Normalization(
                    ProviderNormalizationResult {
                        files_touched: vec![file_touch],
                        ..ProviderNormalizationResult::default()
                    },
                ))
            },
        )?;
        file_touch_count = file_touch_count.saturating_add(file_touch_outcome.emitted());
        if file_touch_outcome.limit_exceeded() {
            visit(RovoDevProjectionUnit::Rejection {
                line,
                error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
            })?;
            limit_rejections = limit_rejections.saturating_add(1);
        }
    }
    Ok((file_touch_count, limit_rejections))
}

enum RovoDevProjectionUnit {
    UseExplicitFileTouches,
    Normalization(ProviderNormalizationResult),
    Rejection { line: usize, error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RovoDevJsonBoundsError {
    Depth { maximum: usize },
    CollectionElements { maximum: usize },
}

impl std::fmt::Display for RovoDevJsonBoundsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Depth { maximum } => {
                write!(formatter, "exceeds maximum JSON depth of {maximum}")
            }
            Self::CollectionElements { maximum } => write!(
                formatter,
                "exceeds JSON collection element budget of {maximum}"
            ),
        }
    }
}

fn rovodev_metadata_without_transcripts(
    value: &Value,
) -> std::result::Result<Value, RovoDevJsonBoundsError> {
    rovodev_metadata_without_transcripts_with_bounds(
        value,
        ROVODEV_WHOLE_JSON_MAX_DEPTH,
        ROVODEV_WHOLE_JSON_MAX_COLLECTION_ELEMENTS,
    )
}

fn rovodev_metadata_without_transcripts_with_bounds(
    value: &Value,
    maximum_depth: usize,
    maximum_collection_elements: usize,
) -> std::result::Result<Value, RovoDevJsonBoundsError> {
    fn validate_bounds(
        value: &Value,
        depth: usize,
        maximum_depth: usize,
        remaining_collection_elements: &mut usize,
        maximum_collection_elements: usize,
    ) -> std::result::Result<(), RovoDevJsonBoundsError> {
        if depth > maximum_depth {
            return Err(RovoDevJsonBoundsError::Depth {
                maximum: maximum_depth,
            });
        }
        let children = match value {
            Value::Array(items) => items.len(),
            Value::Object(object) => object.len(),
            _ => return Ok(()),
        };
        if children > *remaining_collection_elements {
            return Err(RovoDevJsonBoundsError::CollectionElements {
                maximum: maximum_collection_elements,
            });
        }
        *remaining_collection_elements -= children;
        match value {
            Value::Array(items) => {
                for item in items {
                    validate_bounds(
                        item,
                        depth + 1,
                        maximum_depth,
                        remaining_collection_elements,
                        maximum_collection_elements,
                    )?;
                }
            }
            Value::Object(object) => {
                for item in object.values() {
                    validate_bounds(
                        item,
                        depth + 1,
                        maximum_depth,
                        remaining_collection_elements,
                        maximum_collection_elements,
                    )?;
                }
            }
            _ => unreachable!("non-collection JSON values return before traversal"),
        }
        Ok(())
    }

    fn strip_transcript_arrays(value: &Value) -> Value {
        match value {
            Value::Array(items) => {
                Value::Array(items.iter().map(strip_transcript_arrays).collect())
            }
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .filter(|(key, value)| {
                        !(value.is_array()
                            && matches!(key.as_str(), "message_history" | "messages"))
                    })
                    .map(|(key, value)| (key.clone(), strip_transcript_arrays(value)))
                    .collect(),
            ),
            _ => value.clone(),
        }
    }

    let mut remaining_collection_elements = maximum_collection_elements;
    validate_bounds(
        value,
        0,
        maximum_depth,
        &mut remaining_collection_elements,
        maximum_collection_elements,
    )?;
    Ok(provider_capped_json_value(
        &strip_transcript_arrays(value),
        PROVIDER_MAX_PREVIEW_CHARS,
    ))
}

pub(super) struct RovoDevCapturedBatchProjector {
    context: ProviderAdapterContext,
    source: RovoDevSessionSource,
    metadata: Value,
    metadata_failure: Option<String>,
    next_ordinal: u64,
    accepted_sessions: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    failures: Vec<RovoDevCheckpointFailure>,
}

impl RovoDevCapturedBatchProjector {
    pub(super) fn fresh(
        context: ProviderAdapterContext,
        source: RovoDevSessionSource,
        metadata: Value,
        metadata_failure: Option<String>,
    ) -> Self {
        Self {
            context,
            source,
            metadata,
            metadata_failure,
            next_ordinal: 0,
            accepted_sessions: 0,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejected_records: 0,
            failures: Vec::new(),
        }
    }

    pub(super) fn resume(
        context: ProviderAdapterContext,
        source: RovoDevSessionSource,
        metadata: Value,
        metadata_failure: Option<String>,
        cursor: &CertifiedProviderCursor,
    ) -> Result<Self> {
        let checkpoint: RovoDevParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        if checkpoint.next_ordinal != whole_json_position_ordinal(cursor.native_position())? {
            return Err(CaptureError::InvalidPayload(
                "Rovo Dev parser checkpoint does not match its native position".to_owned(),
            ));
        }
        let retained_failures = u64::try_from(checkpoint.failures.len()).map_err(|_| {
            CaptureError::InvalidPayload(
                "Rovo Dev parser checkpoint rejection count exceeds u64".to_owned(),
            )
        })?;
        if checkpoint.failures.len() > MAX_ROVODEV_CHECKPOINT_FAILURES
            || checkpoint
                .failures
                .iter()
                .any(|failure| failure.error.len() > MAX_ROVODEV_FAILURE_BYTES)
            || checkpoint.rejected_records != retained_failures
        {
            return Err(CaptureError::InvalidPayload(
                "Rovo Dev parser checkpoint has invalid rejection diagnostics".to_owned(),
            ));
        }
        Ok(Self {
            context,
            source,
            metadata,
            metadata_failure,
            next_ordinal: checkpoint.next_ordinal,
            accepted_sessions: checkpoint.accepted_sessions,
            accepted_events: checkpoint.accepted_events,
            accepted_file_touches: checkpoint.accepted_file_touches,
            rejected_records: checkpoint.rejected_records.max(cursor.rejected_records()),
            failures: checkpoint.failures,
        })
    }

    fn reject_record(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        line: usize,
        error: String,
    ) -> ProviderProjectionResult<()> {
        let error = bounded_rovodev_failure(error);
        self.rejected_records = self
            .rejected_records
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Rovo Dev rejection count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        if self.failures.len() < MAX_ROVODEV_CHECKPOINT_FAILURES {
            self.failures.push(RovoDevCheckpointFailure {
                line,
                error: error.clone(),
            });
        }
        output.reject_record(line, error);
        Ok(())
    }

    pub(super) fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::try_from(self.accepted_sessions).map_err(|_| {
            CaptureError::SystemInvariant("Rovo Dev replay session count exceeds platform limits")
        })?;
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("Rovo Dev replay event count exceeds platform limits")
        })?;
        let skipped_file_touches = usize::try_from(self.accepted_file_touches).map_err(|_| {
            CaptureError::SystemInvariant(
                "Rovo Dev replay file-touch count exceeds platform limits",
            )
        })?;
        let skipped = skipped_sessions
            .checked_add(skipped_events)
            .and_then(|value| value.checked_add(skipped_file_touches))
            .ok_or(CaptureError::SystemInvariant(
                "Rovo Dev replay summary count overflowed",
            ))?;
        let failed = usize::try_from(self.rejected_records).map_err(|_| {
            CaptureError::SystemInvariant("Rovo Dev replay rejection count exceeds platform limits")
        })?;
        let failures = self
            .failures
            .iter()
            .cloned()
            .map(|failure| ProviderImportFailure {
                line: failure.line,
                error: failure.error,
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

impl CapturedBatchProjector for RovoDevCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != ROVODEV_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Rovo Dev projector received an unexpected record kind",
            ));
        }
        if record.ordinal() != self.next_ordinal || record.ordinal() != 0 {
            return Err(ProviderProjectionFatal::system_invariant(
                "Rovo Dev projector received an unexpected per-file ordinal",
            ));
        }
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Rovo Dev projector requires whole-JSON native bytes",
            ));
        };
        self.next_ordinal = 1;
        let context_json = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                self.reject_record(
                    output,
                    1,
                    format!("invalid Rovo Dev session_context.json: {error}"),
                )?;
                return Ok(());
            }
        };
        if let Some(error) = self.metadata_failure.clone() {
            self.reject_record(output, 1, error)?;
        }
        let Some(messages) = rovodev_message_history(&context_json) else {
            self.reject_record(
                output,
                1,
                "Rovo Dev session_context.json is missing message_history array".to_owned(),
            )?;
            return Ok(());
        };
        let accepted_events = u64::try_from(messages.len())
            .map_err(|_| {
                CaptureError::SystemInvariant("Rovo Dev projected event count exceeds u64")
            })
            .map_err(ProviderProjectionFatal::new)?;
        let context_metadata = match rovodev_metadata_without_transcripts(&context_json) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.reject_record(output, 1, format!("Rovo Dev session_context.json {error}"))?;
                return Ok(());
            }
        };
        let metadata_preview = match rovodev_metadata_without_transcripts(&self.metadata) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.reject_record(output, 1, format!("Rovo Dev metadata.json {error}"))?;
                return Ok(());
            }
        };
        let mut limit_failures = Vec::new();
        let (accepted_file_touches, limit_rejections) = visit_rovodev_session_normalizations(
            &self.source,
            RovoDevProjectionDocument {
                context_json: &context_json,
                metadata: &self.metadata,
                context_metadata: &context_metadata,
                metadata_preview: &metadata_preview,
                messages,
                record_bytes: bytes,
            },
            &self.context,
            |unit| {
                match unit {
                    RovoDevProjectionUnit::UseExplicitFileTouches => {
                        output.use_explicit_file_touches();
                    }
                    RovoDevProjectionUnit::Normalization(normalization) => {
                        emit_projected_normalization_units(output, normalization)?;
                    }
                    RovoDevProjectionUnit::Rejection { line, error } => {
                        output.reject_record(line, error.clone());
                        if limit_failures.len() < MAX_ROVODEV_CHECKPOINT_FAILURES {
                            limit_failures.push(RovoDevCheckpointFailure { line, error });
                        }
                    }
                }
                Ok(())
            },
        )?;
        let accepted_file_touches = u64::try_from(accepted_file_touches)
            .map_err(|_| {
                CaptureError::SystemInvariant("Rovo Dev projected file-touch count exceeds u64")
            })
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_sessions = self
            .accepted_sessions
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Rovo Dev projected session count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_events = self
            .accepted_events
            .checked_add(accepted_events)
            .ok_or(CaptureError::SystemInvariant(
                "Rovo Dev projected event count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(accepted_file_touches)
            .ok_or(CaptureError::SystemInvariant(
                "Rovo Dev projected file-touch count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.rejected_records = self
            .rejected_records
            .checked_add(limit_rejections)
            .ok_or(CaptureError::SystemInvariant(
                "Rovo Dev rejection count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        let remaining_failure_capacity =
            MAX_ROVODEV_CHECKPOINT_FAILURES.saturating_sub(self.failures.len());
        self.failures
            .extend(limit_failures.into_iter().take(remaining_failure_capacity));
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        let next_ordinal = whole_json_position_ordinal(position)?;
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&RovoDevParserCheckpoint {
                next_ordinal,
                accepted_sessions: 0,
                accepted_events: 0,
                accepted_file_touches: 0,
                rejected_records: 0,
                failures: Vec::new(),
            })?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_ordinal = whole_json_position_ordinal(batch.range_end())?;
        if next_ordinal < self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Rovo Dev projector advanced beyond the captured batch",
            ));
        }
        CertifiedProviderCursor::new(
            batch.source().source_revision(),
            batch.source().capture_revision(),
            batch.source().policy_revision(),
            batch.range_end().clone(),
            BoundedParserCheckpoint::from_serializable(&RovoDevParserCheckpoint {
                next_ordinal,
                accepted_sessions: self.accepted_sessions,
                accepted_events: self.accepted_events,
                accepted_file_touches: self.accepted_file_touches,
                rejected_records: self.rejected_records,
                failures: self.failures.clone(),
            })?,
        )
        .map(CapturedBatchCursorFinish::Advance)
    }
}

fn bounded_rovodev_failure(mut error: String) -> String {
    if error.len() <= MAX_ROVODEV_FAILURE_BYTES {
        return error;
    }
    let mut boundary = MAX_ROVODEV_FAILURE_BYTES;
    while !error.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    error.truncate(boundary);
    error
}

pub(super) fn whole_json_position(ordinal: u64) -> Result<NativePosition> {
    NativePosition::new(WHOLE_JSON_POSITION_KIND, ordinal.to_be_bytes().to_vec())
        .map_err(rovodev_captured_batch_error)
}

pub(super) fn whole_json_position_ordinal(position: &NativePosition) -> Result<u64> {
    if position.kind() != WHOLE_JSON_POSITION_KIND || position.value().len() != 8 {
        return Err(CaptureError::InvalidPayload(
            "Rovo Dev cursor has an invalid whole-JSON position".to_owned(),
        ));
    }
    let bytes: [u8; 8] = position.value().try_into().map_err(|_| {
        CaptureError::InvalidPayload("Rovo Dev cursor has an invalid whole-JSON ordinal".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

struct RovoDevCaptureDraft<'a> {
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cwd: Option<String>,
    source: &'a RovoDevSessionSource,
    context_metadata: &'a Value,
    metadata: &'a Value,
    metadata_preview: &'a Value,
    message_count: usize,
    event: Option<ProviderEventEnvelope>,
}

fn rovodev_capture(
    draft: RovoDevCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let is_primary = draft.parent_provider_session_id.is_none();
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::RovoDev,
            source_format: ROVODEV_SOURCE_FORMAT,
            provider_session_id: draft.provider_session_id.clone(),
            parent_provider_session_id: draft.parent_provider_session_id.clone(),
            root_provider_session_id: draft.parent_provider_session_id.clone(),
            external_agent_id: provider_string_field(
                draft.metadata,
                &["agent_id", "agentId", "agent_name", "agentName"],
            ),
            agent_type: if is_primary {
                AgentType::Primary
            } else {
                AgentType::Subagent
            },
            role_hint: Some(if is_primary { "primary" } else { "subagent" }.to_owned()),
            is_primary,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd,
            fidelity: Fidelity::Imported,
            raw_source_path: draft.source.context_path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": ROVODEV_SOURCE_FORMAT,
                "source_path": draft.source.context_path.display().to_string(),
                "metadata_path": draft.source.metadata_path.as_ref().map(|path| path.display().to_string()),
                "session_dir": draft.source.session_dir.display().to_string(),
                "upstream_schema_anchor": {
                    "docs": "https://support.atlassian.com/rovo/docs/manage-sessions-in-rovo-dev-cli/"
                },
            }),
            session_metadata: json!({
                "source_format": ROVODEV_SOURCE_FORMAT,
                "provider": CaptureProvider::RovoDev.as_str(),
                "session_id": draft.provider_session_id,
                "title": provider_string_field(draft.metadata, &["title", "name"]),
                "workspace_path": provider_string_field(draft.metadata, &["workspace_path", "workspacePath"]),
                "message_count": draft.message_count,
                "metadata": draft.metadata_preview,
                "context": draft.context_metadata,
            }),
        },
        context,
        draft.event,
    )
}

fn rovodev_message_history(value: &Value) -> Option<&Vec<Value>> {
    value
        .get("message_history")
        .or_else(|| value.pointer("/session_context/message_history"))
        .or_else(|| value.get("messages"))
        .or_else(|| value.pointer("/conversation/messages"))
        .and_then(Value::as_array)
}

fn rovodev_message_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    provider_timestamp_from_fields(
        value,
        &[
            "timestamp",
            "created_at",
            "createdAt",
            "updated_at",
            "updatedAt",
            "user_sent_time",
        ],
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Map, Value};

    use super::{
        rovodev_metadata_without_transcripts, rovodev_metadata_without_transcripts_with_bounds,
        RovoDevJsonBoundsError, ROVODEV_WHOLE_JSON_MAX_COLLECTION_ELEMENTS,
        ROVODEV_WHOLE_JSON_MAX_DEPTH,
    };

    fn nested_array(depth: usize) -> Value {
        (0..depth).fold(Value::Null, |value, _| Value::Array(vec![value]))
    }

    #[test]
    fn context_metadata_recursively_strips_transcript_arrays() {
        let metadata = rovodev_metadata_without_transcripts(&json!({
            "message_history": [{"content": "top-level secret"}],
            "session_context": {
                "message_history": [{"content": "nested secret"}],
                "keep": "session metadata"
            },
            "conversation": {
                "messages": [{"content": "conversation secret"}],
                "keep": "conversation metadata"
            }
        }))
        .unwrap();

        assert!(metadata.get("message_history").is_none());
        assert!(metadata
            .pointer("/session_context/message_history")
            .is_none());
        assert!(metadata.pointer("/conversation/messages").is_none());
        assert_eq!(
            metadata
                .pointer("/session_context/keep")
                .and_then(Value::as_str),
            Some("session metadata")
        );
        assert_eq!(
            metadata
                .pointer("/conversation/keep")
                .and_then(Value::as_str),
            Some("conversation metadata")
        );
    }

    #[test]
    fn json_depth_budget_accepts_boundary_and_rejects_next_level() {
        let boundary =
            rovodev_metadata_without_transcripts(&nested_array(ROVODEV_WHOLE_JSON_MAX_DEPTH))
                .unwrap();
        assert_eq!(
            boundary.pointer(&"/0".repeat(ROVODEV_WHOLE_JSON_MAX_DEPTH)),
            Some(&Value::Null)
        );

        let error =
            rovodev_metadata_without_transcripts(&nested_array(ROVODEV_WHOLE_JSON_MAX_DEPTH + 1))
                .unwrap_err();
        assert_eq!(
            error,
            RovoDevJsonBoundsError::Depth {
                maximum: ROVODEV_WHOLE_JSON_MAX_DEPTH
            }
        );
    }

    #[test]
    fn collection_budget_accepts_boundary_and_rejects_broad_array() {
        let boundary = Value::Array(vec![
            Value::Null;
            ROVODEV_WHOLE_JSON_MAX_COLLECTION_ELEMENTS
        ]);
        assert_eq!(
            rovodev_metadata_without_transcripts(&boundary)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            ROVODEV_WHOLE_JSON_MAX_COLLECTION_ELEMENTS
        );

        let over_budget = Value::Array(vec![
            Value::Null;
            ROVODEV_WHOLE_JSON_MAX_COLLECTION_ELEMENTS + 1
        ]);
        assert_eq!(
            rovodev_metadata_without_transcripts(&over_budget).unwrap_err(),
            RovoDevJsonBoundsError::CollectionElements {
                maximum: ROVODEV_WHOLE_JSON_MAX_COLLECTION_ELEMENTS
            }
        );
    }

    #[test]
    fn collection_budget_rejects_broad_maps_and_cumulative_children() {
        let broad_map = (0..=ROVODEV_WHOLE_JSON_MAX_COLLECTION_ELEMENTS)
            .map(|index| (format!("key-{index:05}"), Value::Null))
            .collect::<Map<_, _>>();
        assert_eq!(
            rovodev_metadata_without_transcripts(&Value::Object(broad_map)).unwrap_err(),
            RovoDevJsonBoundsError::CollectionElements {
                maximum: ROVODEV_WHOLE_JSON_MAX_COLLECTION_ELEMENTS
            }
        );

        let cumulative = json!({"first": [1, 2], "second": [3, 4]});
        assert_eq!(
            rovodev_metadata_without_transcripts_with_bounds(&cumulative, 4, 5).unwrap_err(),
            RovoDevJsonBoundsError::CollectionElements { maximum: 5 }
        );
    }

    #[test]
    fn bounded_metadata_projection_is_deterministic() {
        let input = json!({
            "z": {"keep": [3, 1, 2], "messages": [{"secret": "z"}]},
            "message_history": [{"secret": "root"}],
            "a": {"keep": true}
        });
        let expected = r#"{"a":{"keep":true},"z":{"keep":[3,1,2]}}"#;

        for _ in 0..8 {
            let projected = rovodev_metadata_without_transcripts(&input).unwrap();
            assert_eq!(serde_json::to_string(&projected).unwrap(), expected);
        }
    }
}
