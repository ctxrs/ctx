use std::{num::NonZeroUsize, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, ContentRef, EventRole, EventType, Fidelity,
    ProviderCaptureEnvelope, ProviderEventEnvelope, ProviderSourceTrust,
};
use ctx_history_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::captured_batch::whole_json::{WholeJsonBatchProducer, WholeJsonItem};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, ProviderRecordKind,
    SourceObservation, CAPTURE_BATCH_MAX_BATCHES_PER_GROUP,
};
use crate::complete_content::structured::{
    attach_continue_result_content_locator, attach_structured_complete_content_locator,
};

use crate::provider::file_touches::{
    visit_provider_file_touches_from_raw_value, ProviderFileTouchSourceContext,
    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    captured_batch_cursor_stream, emit_projected_normalization_units, import_captured_batches,
    provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
    CapturedBatchCursorFinish, CapturedBatchCursorMode, CapturedBatchProjector,
    CapturedSourceAdmission, CertifiedProviderCursor, ProviderProjectionFatal,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_role, provider_timestamp_value,
    provider_value_text, NativeEventDraft, NativeSessionDraft,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    ProviderNormalizationResult, Result, CONTINUE_CLI_SOURCE_FORMAT,
};

mod message_text;
mod source;
#[cfg(test)]
mod tests;
mod traversal;
mod whole_json;

pub(crate) use message_text::continue_history_item_text;
use source::{ContinueIndexCache, ContinueIndexObservation, ContinueSessionObservation};
use traversal::visit_continue_session_files;
use whole_json::{
    continue_captured_batch_error, continue_whole_json_error, whole_json_position,
    whole_json_position_ordinal,
};

const CONTINUE_CAPTURE_REVISION: u32 = 2;
const CONTINUE_POLICY_REVISION: u32 = 6;
const CONTINUE_RECORD_KIND: &str = "continue-session-json-v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinueParserCheckpoint {
    next_ordinal: u64,
    accepted_sessions: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
}

struct ContinueCapturedBatchProjector<'a> {
    context: ProviderAdapterContext,
    raw_source_path: String,
    session_path: &'a Path,
    sibling_index: &'a ContinueIndexObservation,
    index_cache: &'a ContinueIndexCache,
    next_ordinal: u64,
    accepted_sessions: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
}

impl<'a> ContinueCapturedBatchProjector<'a> {
    fn fresh(
        context: ProviderAdapterContext,
        raw_source_path: String,
        session_path: &'a Path,
        sibling_index: &'a ContinueIndexObservation,
        index_cache: &'a ContinueIndexCache,
    ) -> Self {
        Self {
            context,
            raw_source_path,
            session_path,
            sibling_index,
            index_cache,
            next_ordinal: 0,
            accepted_sessions: 0,
            accepted_events: 0,
            accepted_file_touches: 0,
        }
    }

    fn resume(
        context: ProviderAdapterContext,
        raw_source_path: String,
        cursor: &CertifiedProviderCursor,
        session_path: &'a Path,
        sibling_index: &'a ContinueIndexObservation,
        index_cache: &'a ContinueIndexCache,
    ) -> Result<Self> {
        let checkpoint: ContinueParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        if checkpoint.next_ordinal != whole_json_position_ordinal(cursor.native_position())? {
            return Err(CaptureError::InvalidPayload(
                "Continue parser checkpoint does not match its native position".to_owned(),
            ));
        }
        Ok(Self {
            context,
            raw_source_path,
            session_path,
            sibling_index,
            index_cache,
            next_ordinal: checkpoint.next_ordinal,
            accepted_sessions: checkpoint.accepted_sessions,
            accepted_events: checkpoint.accepted_events,
            accepted_file_touches: checkpoint.accepted_file_touches,
        })
    }

    fn replay_summary(&self, cursor_rejected_records: u64) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::try_from(self.accepted_sessions).map_err(|_| {
            CaptureError::SystemInvariant("Continue replay session count exceeds platform limits")
        })?;
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("Continue replay event count exceeds platform limits")
        })?;
        let skipped_file_touches = usize::try_from(self.accepted_file_touches).map_err(|_| {
            CaptureError::SystemInvariant(
                "Continue replay file-touch count exceeds platform limits",
            )
        })?;
        let skipped = skipped_sessions
            .checked_add(skipped_events)
            .and_then(|value| value.checked_add(skipped_file_touches))
            .ok_or(CaptureError::SystemInvariant(
                "Continue replay summary count overflowed",
            ))?;
        let cursor_failed = usize::try_from(cursor_rejected_records).map_err(|_| {
            CaptureError::SystemInvariant("Continue replay rejection count exceeds platform limits")
        })?;
        let mut summary = ProviderImportSummary {
            skipped,
            skipped_sessions,
            skipped_events,
            accepted_content_records: skipped_events.saturating_add(skipped_file_touches),
            ..ProviderImportSummary::default()
        };
        // The certified cursor count is cumulative. Overlay it instead of
        // adding it to any rejection already represented by this replay.
        summary.failed = summary.failed.max(cursor_failed);
        Ok(summary)
    }
}

impl CapturedBatchProjector for ContinueCapturedBatchProjector<'_> {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != CONTINUE_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Continue projector received an unexpected record kind",
            ));
        }
        if record.ordinal() != self.next_ordinal || record.ordinal() != 0 {
            return Err(ProviderProjectionFatal::system_invariant(
                "Continue projector received an unexpected per-file ordinal",
            ));
        }
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Continue projector requires whole-JSON native bytes",
            ));
        };
        self.next_ordinal = 1;
        let session = match serde_json::from_slice::<Value>(bytes) {
            Ok(session) => session,
            Err(error) => {
                output.reject_record(1, format!("invalid Continue CLI session JSON: {error}"));
                return Ok(());
            }
        };
        let Some(provider_session_id) = continue_session_id(&session, self.session_path) else {
            output.reject_record(
                1,
                "Continue CLI session is missing sessionId and has no JSON file stem".to_owned(),
            );
            return Ok(());
        };
        let indexed_metadata = self
            .index_cache
            .metadata(self.sibling_index, &provider_session_id)
            .map_err(ProviderProjectionFatal::new)?;
        let started_at = continue_session_started_at(
            &session,
            indexed_metadata.as_ref(),
            self.context.imported_at,
        );
        let history = session
            .get("history")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if history.is_empty() {
            output.emit_normalization(ProviderNormalizationResult {
                captures: vec![(
                    1,
                    continue_capture(
                        &provider_session_id,
                        &session,
                        indexed_metadata.as_ref(),
                        started_at,
                        &self.raw_source_path,
                        &self.context,
                        None,
                    ),
                )],
                ..ProviderNormalizationResult::default()
            })?;
            self.accepted_sessions = self
                .accepted_sessions
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Continue projected session count overflowed",
                ))
                .map_err(ProviderProjectionFatal::new)?;
            return Ok(());
        }
        let mut next_source_subrecord_index = 0_u32;
        for (item_index, item) in history.iter().enumerate() {
            let provider_event_index = item_index.saturating_add(1) as u64;
            let line = item_index.saturating_add(1);
            let fallback_time = started_at + chrono::Duration::milliseconds(item_index as i64);
            let occurred_at = continue_history_item_timestamp(item, fallback_time);
            let mut event = continue_history_item_event(
                &provider_session_id,
                item,
                provider_event_index,
                occurred_at,
            );
            if let Some(complete_text) = continue_history_item_text(item) {
                let native_id = event
                    .provider_event_hash
                    .clone()
                    .unwrap_or_else(|| format!("history:{provider_session_id}:{item_index}"));
                attach_structured_complete_content_locator(
                    CaptureProvider::Continue,
                    &mut event,
                    record.ordinal(),
                    next_source_subrecord_index,
                    &native_id,
                    bytes,
                    &complete_text,
                )
                .map_err(ProviderProjectionFatal::new)?;
            }
            let source_root = self.context.source_root_display();
            let capture = continue_capture(
                &provider_session_id,
                &session,
                indexed_metadata.as_ref(),
                started_at,
                &self.raw_source_path,
                &self.context,
                Some(event.clone()),
            );
            output.use_explicit_file_touches();
            emit_projected_normalization_units(
                output,
                ProviderNormalizationResult {
                    captures: vec![(line, capture)],
                    ..ProviderNormalizationResult::default()
                },
            )?;
            next_source_subrecord_index =
                next_source_subrecord_index.checked_add(1).ok_or_else(|| {
                    ProviderProjectionFatal::system_invariant(
                        "Continue source subrecord index exceeds u32",
                    )
                })?;
            let file_touch_outcome = visit_provider_file_touches_from_raw_value(
                ProviderFileTouchSourceContext::new(
                    CaptureProvider::Continue,
                    &provider_session_id,
                    CONTINUE_CLI_SOURCE_FORMAT,
                    Some(self.raw_source_path.as_str()),
                    source_root.as_deref(),
                ),
                item,
                &event,
                line,
                |file_touch| {
                    output.emit_normalization(ProviderNormalizationResult {
                        files_touched: vec![file_touch],
                        ..ProviderNormalizationResult::default()
                    })
                },
            )?;
            if file_touch_outcome.limit_exceeded() {
                output.reject_record(line, PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned());
            }
            let file_touch_count = u64::try_from(file_touch_outcome.emitted())
                .map_err(|_| {
                    CaptureError::SystemInvariant("Continue projected file-touch count exceeds u64")
                })
                .map_err(ProviderProjectionFatal::new)?;
            self.accepted_events = self
                .accepted_events
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Continue projected event count overflowed",
                ))
                .map_err(ProviderProjectionFatal::new)?;
            self.accepted_file_touches = self
                .accepted_file_touches
                .checked_add(file_touch_count)
                .ok_or(CaptureError::SystemInvariant(
                    "Continue projected file-touch count overflowed",
                ))
                .map_err(ProviderProjectionFatal::new)?;
            let item_index_u32 = u32::try_from(item_index).map_err(|_| {
                ProviderProjectionFatal::system_invariant("Continue history index exceeds u32")
            })?;
            let states = item
                .get("toolCallStates")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for (tool_state_index, state) in states.iter().enumerate() {
                let Some(result_body) = continue_tool_result_body(state) else {
                    continue;
                };
                let tool_state_index = u32::try_from(tool_state_index).map_err(|_| {
                    ProviderProjectionFatal::system_invariant(
                        "Continue tool-state index exceeds u32",
                    )
                })?;
                let result = ContinueResultProjection {
                    history_item_index: item_index_u32,
                    tool_state_index,
                    occurred_at,
                    native_record_id: continue_tool_result_native_id(
                        item,
                        item_index_u32,
                        state,
                        tool_state_index,
                    ),
                    body: result_body,
                    state: state.clone(),
                };
                let provider_event_index = continue_result_provider_event_index(
                    result.history_item_index,
                    result.tool_state_index,
                )
                .map_err(ProviderProjectionFatal::new)?;
                let mut event =
                    continue_tool_result_event(&provider_session_id, &result, provider_event_index);
                attach_continue_result_content_locator(
                    &mut event,
                    record.ordinal(),
                    next_source_subrecord_index,
                    result.history_item_index,
                    result.tool_state_index,
                    &result.native_record_id,
                    bytes,
                    &result.body,
                )
                .map_err(ProviderProjectionFatal::new)?;
                let capture = continue_capture(
                    &provider_session_id,
                    &session,
                    indexed_metadata.as_ref(),
                    started_at,
                    &self.raw_source_path,
                    &self.context,
                    Some(event),
                );
                emit_projected_normalization_units(
                    output,
                    ProviderNormalizationResult {
                        captures: vec![(line, capture)],
                        ..ProviderNormalizationResult::default()
                    },
                )?;
                next_source_subrecord_index =
                    next_source_subrecord_index.checked_add(1).ok_or_else(|| {
                        ProviderProjectionFatal::system_invariant(
                            "Continue source subrecord index exceeds u32",
                        )
                    })?;
                self.accepted_events = self
                    .accepted_events
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Continue projected event count overflowed",
                    ))
                    .map_err(ProviderProjectionFatal::new)?;
            }
        }
        self.accepted_sessions = self
            .accepted_sessions
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Continue projected session count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if whole_json_position_ordinal(position)? != 0 {
            return Err(CaptureError::InvalidPayload(
                "Continue initial cursor candidate is not at the whole-JSON source start"
                    .to_owned(),
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&ContinueParserCheckpoint {
                next_ordinal: 0,
                accepted_sessions: 0,
                accepted_events: 0,
                accepted_file_touches: 0,
            })?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_ordinal = whole_json_position_ordinal(batch.range_end())?;
        if next_ordinal < self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Continue projector advanced beyond the captured batch",
            ));
        }
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&ContinueParserCheckpoint {
                    next_ordinal,
                    accepted_sessions: self.accepted_sessions,
                    accepted_events: self.accepted_events,
                    accepted_file_touches: self.accepted_file_touches,
                })?,
            )?,
        ))
    }
}

pub(crate) fn import_continue_cli_sessions_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let mut merged = ProviderImportSummary::default();
    let mut index_cache = ContinueIndexCache::default();
    let mut source_count = 0_usize;
    visit_continue_session_files(path, &mut |session_path| {
        source_count = source_count.saturating_add(1);
        let summary = import_continue_session_file_batched(
            session_path,
            store,
            &context,
            &import_options,
            &mut index_cache,
        )?;
        merged.merge(summary);
        Ok(())
    })?;
    if source_count == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Continue CLI session JSON files found",
        });
    }
    Ok(merged)
}

fn import_continue_session_file_batched(
    path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
    index_cache: &mut ContinueIndexCache,
) -> Result<ProviderImportSummary> {
    let observation = ContinueSessionObservation::read(path, index_cache)?;
    let path_identity = provider_path_identity(observation.canonical_path())?;
    let raw_source_path = path.display().to_string();
    let file_context = ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    };
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        &path_identity,
    );
    let source = SourceObservation::new(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        format!("continue-session-file:{path_identity}"),
        observation.source_revision(),
        cursor_stream,
        CONTINUE_CAPTURE_REVISION,
        CONTINUE_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(continue_captured_batch_error)?;
    let record_kind =
        ProviderRecordKind::new(CONTINUE_RECORD_KIND).map_err(continue_captured_batch_error)?;
    let initial_position = whole_json_position(0)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut resumable_cursor = None;
    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.source_revision() == source.source_revision()
                    && certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let ordinal = whole_json_position_ordinal(certified.native_position())?;
                if ordinal > 1 {
                    return Err(CaptureError::InvalidPayload(
                        "Continue per-file cursor exceeds its source".to_owned(),
                    ));
                }
                let projector = ContinueCapturedBatchProjector::resume(
                    file_context.clone(),
                    raw_source_path.clone(),
                    &certified,
                    observation.session_path(),
                    observation.sibling_index(),
                    index_cache,
                )?;
                if ordinal == 1 {
                    if !observation.revalidate()? {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    return projector.replay_summary(certified.rejected_records());
                }
                resumable_cursor = Some(certified);
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let mut emitted = false;
    let item_path = observation.session_path().to_path_buf();
    let item_length = observation.session_length();
    let source_item = path_identity.into_bytes();
    let mut producer = WholeJsonBatchProducer::new(source.clone(), record_kind, move || {
        if emitted {
            return Ok(None);
        }
        emitted = true;
        WholeJsonItem::new(0, source_item.clone(), item_length, item_path.clone()).map(Some)
    })
    .map_err(continue_whole_json_error)?;
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &file_context)?;
    let batch = producer
        .next_batch()
        .map_err(continue_whole_json_error)?
        .ok_or(CaptureError::SystemInvariant(
            "Continue per-file producer returned no captured batch",
        ))?;
    let mut projector = match resumable_cursor.as_ref() {
        Some(cursor) if cursor_mode == CapturedBatchCursorMode::Resume => {
            ContinueCapturedBatchProjector::resume(
                file_context.clone(),
                raw_source_path,
                cursor,
                observation.session_path(),
                observation.sibling_index(),
                index_cache,
            )?
        }
        _ => ContinueCapturedBatchProjector::fresh(
            file_context.clone(),
            raw_source_path,
            observation.session_path(),
            observation.sibling_index(),
            index_cache,
        ),
    };
    let mut pending_batch = Some(batch);
    let max_batches = NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).ok_or(
        CaptureError::SystemInvariant("captured batch group limit must be nonzero"),
    )?;
    let outcome = import_captured_batches(
        store,
        &admission,
        import_options.clone(),
        &context.machine_id,
        context.imported_at,
        expected_store_cursor.as_ref(),
        &initial_position,
        cursor_mode,
        max_batches,
        &mut projector,
        || Ok(pending_batch.take()),
        || observation.revalidate(),
    )?;
    if outcome.batches_imported != 1 || !outcome.source_exhausted {
        return Err(CaptureError::SystemInvariant(
            "Continue per-file import did not consume exactly one batch",
        ));
    }
    Ok(outcome.summary)
}

pub(crate) fn continue_session_json_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("json")
        && path.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
}

pub(crate) fn continue_session_id(session: &Value, path: &Path) -> Option<String> {
    session
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned)
        })
}

pub(crate) fn continue_session_started_at(
    session: &Value,
    indexed_metadata: Option<&Value>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    session
        .get("createdAt")
        .or_else(|| session.get("startedAt"))
        .or_else(|| indexed_metadata.and_then(|metadata| metadata.get("dateCreated")))
        .map(|value| provider_timestamp_value(Some(value), fallback))
        .unwrap_or(fallback)
}

pub(crate) fn continue_history_item_timestamp(
    item: &Value,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    item.get("timestamp")
        .or_else(|| item.get("createdAt"))
        .or_else(|| item.pointer("/message/timestamp"))
        .map(|value| provider_timestamp_value(Some(value), fallback))
        .unwrap_or(fallback)
}

pub(crate) fn continue_capture(
    provider_session_id: &str,
    session: &Value,
    indexed_metadata: Option<&Value>,
    started_at: DateTime<Utc>,
    raw_source_path: &str,
    context: &ProviderAdapterContext,
    event: Option<ProviderEventEnvelope>,
) -> ProviderCaptureEnvelope {
    let title = session.get("title").and_then(Value::as_str);
    let cwd = session
        .get("workspaceDirectory")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Continue,
            source_format: CONTINUE_CLI_SOURCE_FORMAT,
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("continue-cli".to_owned()),
            is_primary: true,
            started_at,
            ended_at: None,
            cwd,
            fidelity: Fidelity::Imported,
            raw_source_path: raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": CONTINUE_CLI_SOURCE_FORMAT,
                "source_format": CONTINUE_CLI_SOURCE_FORMAT,
            }),
            session_metadata: json!({
                "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                "title": title,
                "mode": session.get("mode").cloned(),
                "chat_model_title": session.get("chatModelTitle").cloned(),
                "usage": session.get("usage").cloned(),
                "session_index": indexed_metadata.cloned(),
            }),
        },
        context,
        event,
    )
}

pub(crate) fn continue_history_item_event(
    provider_session_id: &str,
    item: &Value,
    provider_event_index: u64,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let role_text = item.pointer("/message/role").and_then(Value::as_str);
    let role = Some(provider_role(role_text));
    let has_tool_calls = item
        .get("toolCallStates")
        .and_then(Value::as_array)
        .is_some_and(|states| !states.is_empty());
    let event_type = if has_tool_calls {
        EventType::ToolCall
    } else {
        EventType::Message
    };
    native_event(NativeEventDraft {
        provider: CaptureProvider::Continue,
        source_format: CONTINUE_CLI_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index,
        provider_event_hash: item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned),
        cursor: format!("history:{provider_session_id}:{provider_event_index}"),
        event_type,
        role,
        occurred_at,
        text: continue_history_item_text(item).unwrap_or_default(),
        body: item.clone(),
        metadata: json!({
            "source": CONTINUE_CLI_SOURCE_FORMAT,
            "source_format": CONTINUE_CLI_SOURCE_FORMAT,
            "message_role": role_text,
            "has_tool_calls": has_tool_calls,
        }),
    })
}

#[derive(Debug)]
struct ContinueResultProjection {
    history_item_index: u32,
    tool_state_index: u32,
    occurred_at: DateTime<Utc>,
    native_record_id: String,
    body: String,
    state: Value,
}

pub(crate) fn continue_tool_result_body(state: &Value) -> Option<String> {
    match state.get("output")? {
        Value::Null => None,
        Value::String(output) => Some(output.clone()),
        output => serde_json::to_string(output).ok(),
    }
}

pub(crate) fn continue_tool_result_native_id(
    item: &Value,
    history_item_index: u32,
    state: &Value,
    tool_state_index: u32,
) -> String {
    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| valid_continue_native_id_part(value));
    let tool_call_id = state
        .get("toolCallId")
        .or_else(|| state.pointer("/toolCall/id"))
        .and_then(Value::as_str)
        .filter(|value| valid_continue_native_id_part(value));
    match (item_id, tool_call_id) {
        (Some(item_id), Some(tool_call_id)) => {
            format!("{item_id}:tool:{tool_call_id}:result")
        }
        (Some(item_id), None) => format!("{item_id}:tool-state:{tool_state_index}:result"),
        (None, Some(tool_call_id)) => {
            format!("history:{history_item_index}:tool:{tool_call_id}:result")
        }
        (None, None) => {
            format!("history:{history_item_index}:tool-state:{tool_state_index}:result")
        }
    }
}

fn valid_continue_native_id_part(value: &str) -> bool {
    !value.is_empty() && value.len() <= 384 && !value.chars().any(char::is_control)
}

fn continue_tool_result_event(
    provider_session_id: &str,
    result: &ContinueResultProjection,
    provider_event_index: u64,
) -> ProviderEventEnvelope {
    let tool_name = result
        .state
        .pointer("/toolCall/function/name")
        .or_else(|| result.state.pointer("/toolCall/name"))
        .and_then(Value::as_str)
        .filter(|value| valid_continue_result_token(value, 256))
        .unwrap_or("tool");
    let tool_call_id = result
        .state
        .get("toolCallId")
        .or_else(|| result.state.pointer("/toolCall/id"))
        .and_then(Value::as_str)
        .filter(|value| valid_continue_result_token(value, 256));
    let tool_status = result
        .state
        .get("status")
        .and_then(Value::as_str)
        .filter(|value| valid_continue_result_token(value, 64));
    let event_type = continue_result_event_type(tool_name);
    let content_ref = ContentRef::from_bytes(result.body.as_bytes());
    let mut event = native_event(NativeEventDraft {
        provider: CaptureProvider::Continue,
        source_format: CONTINUE_CLI_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index,
        provider_event_hash: None,
        cursor: format!(
            "history:{provider_session_id}:{}:tool-state:{}:result",
            result.history_item_index, result.tool_state_index
        ),
        event_type,
        role: Some(EventRole::Tool),
        occurred_at: result.occurred_at,
        text: result.body.clone(),
        body: result.state.clone(),
        metadata: json!({
            "source": CONTINUE_CLI_SOURCE_FORMAT,
            "source_format": CONTINUE_CLI_SOURCE_FORMAT,
            "tool_name": tool_name,
            "tool_call_id": tool_call_id,
            "tool_status": tool_status,
            "history_item_index": result.history_item_index,
            "tool_state_index": result.tool_state_index,
        }),
    });
    if let (Some(object), Some(content_ref)) = (event.payload.as_object_mut(), content_ref) {
        object.insert("result_content_ref".to_owned(), json!(content_ref));
        object.insert("tool".to_owned(), json!(tool_name));
        if let Some(tool_call_id) = tool_call_id {
            object.insert("call_id".to_owned(), json!(tool_call_id));
        }
    }
    event
}

fn valid_continue_result_token(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
}

fn continue_result_provider_event_index(
    history_item_index: u32,
    tool_state_index: u32,
) -> Result<u64> {
    const RESULT_EVENT_NAMESPACE: u64 = 1_u64 << 63;
    const TOOL_STATE_BITS: u32 = 31;
    const MAX_TOOL_STATE_INDEX: u32 = (1_u32 << TOOL_STATE_BITS) - 1;
    if tool_state_index > MAX_TOOL_STATE_INDEX {
        return Err(CaptureError::InvalidPayload(
            "Continue tool-state index exceeds stable result identity bounds".to_owned(),
        ));
    }
    Ok(RESULT_EVENT_NAMESPACE
        | (u64::from(history_item_index) << TOOL_STATE_BITS)
        | u64::from(tool_state_index))
}

fn continue_result_event_type(tool_name: &str) -> EventType {
    let normalized = tool_name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if matches!(
        normalized.as_str(),
        "bash"
            | "shell"
            | "terminal"
            | "command"
            | "executecommand"
            | "runcommand"
            | "runterminalcommand"
    ) {
        EventType::CommandOutput
    } else {
        EventType::ToolOutput
    }
}

pub(crate) fn continue_context_items_text(value: &Value) -> Option<String> {
    let items = value.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        if let Some(content) = item.get("content").and_then(provider_value_text) {
            parts.push(content);
        } else if let Some(name) = item.get("name").and_then(Value::as_str) {
            parts.push(name.to_owned());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn continue_tool_states_text(value: &Value) -> Option<String> {
    let states = value.as_array()?;
    let mut parts = Vec::new();
    for state in states {
        let name = state
            .pointer("/toolCall/function/name")
            .or_else(|| state.pointer("/toolCall/name"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let status = state
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        parts.push(format!("tool: {name} | status: {status}"));
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}
