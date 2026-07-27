use std::{
    fs::{self},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderEventEnvelope,
    ProviderSourceTrust,
};
use ctx_history_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::captured_batch::whole_json::{
    WholeJsonBatchError, WholeJsonBatchProducer, WholeJsonItem,
};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, ProviderRecordKind,
    SourceObservation, CAPTURE_BATCH_MAX_BATCHES_PER_GROUP,
};
use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::complete_content::structured::attach_structured_complete_content_locator;
use crate::provider::importer::{
    captured_batch_cursor_stream, emit_projected_normalization_units, import_captured_batches,
    provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
    CapturedBatchCursorFinish, CapturedBatchCursorMode, CapturedBatchProjector,
    CapturedSourceAdmission, CertifiedProviderCursor, ProviderProjectionFatal,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_block_text, provider_capped_json,
    provider_string_field, provider_timestamp_from_fields, NativeEventDraft, NativeSessionDraft,
};
use crate::{
    fnv1a64, CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext,
    ProviderImportSummary, ProviderNormalizationResult, Result, AUGGIE_SESSION_JSON_SOURCE_FORMAT,
    PROVIDER_MAX_PREVIEW_CHARS,
};

const AUGGIE_CAPTURE_REVISION: u32 = 2;
const AUGGIE_POLICY_REVISION: u32 = 4;
const AUGGIE_RECORD_KIND: &str = "auggie-session-json-v1";
const WHOLE_JSON_POSITION_KIND: &str = "whole-json-item-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuggieFrozenFile {
    path: PathBuf,
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl AuggieFrozenFile {
    fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        let metadata = fs::symlink_metadata(path)?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            path: path.to_path_buf(),
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    fn revision_component(&self, output: &mut String) {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        output.push_str(&format!(
            "{:?}\0{}\0{side}{seconds}.{nanos:09}\0{}\0{:?}\0{:?}\n",
            self.path.as_os_str(),
            self.length,
            self.readonly,
            self.device,
            self.inode,
        ));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuggieSessionObservation {
    canonical_path: PathBuf,
    session_file: AuggieFrozenFile,
}

impl AuggieSessionObservation {
    fn read(path: &Path) -> Result<Self> {
        Ok(Self {
            canonical_path: fs::canonicalize(path)?,
            session_file: AuggieFrozenFile::read(path)?,
        })
    }

    fn source_revision(&self) -> String {
        let mut input = format!(
            "auggie-session-file-v1\0capture={AUGGIE_CAPTURE_REVISION}\0policy={AUGGIE_POLICY_REVISION}\n"
        );
        self.session_file.revision_component(&mut input);
        format!(
            "auggie-session-file-v1:fnv1a64:{:016x}",
            fnv1a64(input.as_bytes())
        )
    }

    fn revalidate(&self) -> Result<bool> {
        let session_file = match AuggieFrozenFile::read(&self.session_file.path) {
            Ok(file) => file,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(session_file == self.session_file
            && fs::canonicalize(&self.session_file.path)? == self.canonical_path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuggieParserCheckpoint {
    next_ordinal: u64,
    accepted_sessions: u64,
    accepted_events: u64,
}

struct AuggieSessionProjection<'a> {
    provider_session_id: String,
    chat_history: &'a [Value],
    started_at: DateTime<Utc>,
    raw_source_path: String,
    base_draft: NativeSessionDraft,
}

impl<'a> AuggieSessionProjection<'a> {
    fn new(session: &'a Value, path: &Path, context: &ProviderAdapterContext) -> Result<Self> {
        let provider_session_id = provider_string_field(session, &["sessionId", "session_id"])
            .ok_or_else(|| {
                CaptureError::InvalidPayload("Auggie session JSON is missing sessionId".to_owned())
            })?;
        let chat_history = session
            .get("chatHistory")
            .or_else(|| session.get("chat_history"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Auggie session JSON is missing chatHistory array".to_owned(),
                )
            })?;
        let started_at = provider_timestamp_from_fields(
            session,
            &[
                "created",
                "createdAt",
                "created_at",
                "startedAt",
                "started_at",
            ],
        )
        .or_else(|| {
            chat_history
                .iter()
                .find_map(|entry| auggie_entry_time(entry, None))
        })
        .unwrap_or(context.imported_at);
        let ended_at = provider_timestamp_from_fields(
            session,
            &[
                "modified",
                "modifiedAt",
                "updatedAt",
                "updated_at",
                "endedAt",
                "ended_at",
            ],
        )
        .or_else(|| {
            chat_history
                .iter()
                .rev()
                .find_map(|entry| auggie_entry_time(entry, None))
        });
        let cwd = provider_string_field(
            session,
            &[
                "workspaceRoot",
                "workspace_root",
                "workspacePath",
                "workspace_path",
                "cwd",
            ],
        );
        let raw_source_path = path.display().to_string();
        let source_metadata = json!({
            "adapter": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            "source_path": raw_source_path,
            "upstream_schema_anchor": {
                "package": "@augmentcode/auggie@0.32.0",
                "docs": "https://docs.augmentcode.com/cli/reference",
                "package_storage": "SessionStore writes ~/.augment/sessions/<session_id>.json",
            },
        });
        let session_metadata = json!({
            "source_format": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            "provider": CaptureProvider::Auggie.as_str(),
            "display_name": "Auggie",
            "session_id": provider_session_id,
            "workspace_id": provider_string_field(session, &["workspaceId", "workspace_id"]),
            "name": provider_string_field(session, &["name", "title", "sessionName"]),
            "chat_history_count": chat_history.len(),
            "agent_state": session
                .get("agentState")
                .or_else(|| session.get("agent_state"))
                .map(|value| provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS)),
            "limitations": [
                "ctx imports request_message and response_text fields plus recognized request_nodes/response_nodes text",
                "tool calls and tool outputs in richer Auggie node schemas are retained only as capped native JSON until a public node contract is available"
            ],
        });
        let base_draft = NativeSessionDraft {
            provider: CaptureProvider::Auggie,
            source_format: AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            provider_session_id: provider_session_id.clone(),
            parent_provider_session_id: provider_string_field(
                session,
                &[
                    "parentConversationId",
                    "parentSessionId",
                    "parent_session_id",
                ],
            ),
            root_provider_session_id: provider_string_field(
                session,
                &["rootConversationId", "rootSessionId", "root_session_id"],
            ),
            external_agent_id: provider_string_field(
                session,
                &["poseidonAgentId", "agentId", "agent_id"],
            ),
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd,
            fidelity: Fidelity::Imported,
            raw_source_path: raw_source_path.clone(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata,
            session_metadata,
        };
        Ok(Self {
            provider_session_id,
            chat_history,
            started_at,
            raw_source_path,
            base_draft,
        })
    }
}

fn visit_auggie_session_normalizations(
    projection: &AuggieSessionProjection<'_>,
    context: &ProviderAdapterContext,
    session_ordinal: usize,
    record_bytes: &[u8],
    mut visit: impl FnMut(ProviderNormalizationResult) -> ProviderProjectionResult<()>,
) -> ProviderProjectionResult<usize> {
    let mut provider_event_index = 0_u64;
    let mut accepted_events = 0_usize;
    for (chat_index, entry) in projection.chat_history.iter().enumerate() {
        let exchange = entry.get("exchange").unwrap_or(entry);
        let base_time = auggie_entry_time(entry, Some(exchange)).unwrap_or_else(|| {
            projection.started_at + Duration::milliseconds(chat_index as i64 * 2)
        });
        if let Some(text) = auggie_request_text(exchange) {
            let complete_text = text.clone();
            let mut event = auggie_event(AuggieEventInput {
                provider_session_id: &projection.provider_session_id,
                provider_event_index,
                chat_index,
                role: EventRole::User,
                label: "request",
                occurred_at: base_time,
                text,
                entry,
                exchange,
                raw_source_path: &projection.raw_source_path,
            });
            let native_id = event.provider_event_hash.clone().unwrap_or_default();
            attach_structured_complete_content_locator(
                CaptureProvider::Auggie,
                &mut event,
                0,
                u32::try_from(accepted_events).unwrap_or(u32::MAX),
                &native_id,
                record_bytes,
                &complete_text,
            )
            .map_err(ProviderProjectionFatal::new)?;
            let line = session_ordinal
                .saturating_mul(10_000)
                .saturating_add(chat_index.saturating_mul(2))
                .saturating_add(1);
            visit(ProviderNormalizationResult {
                captures: vec![(
                    line,
                    native_provider_capture(projection.base_draft.clone(), context, Some(event)),
                )],
                ..ProviderNormalizationResult::default()
            })?;
            provider_event_index = provider_event_index.saturating_add(1);
            accepted_events = accepted_events.saturating_add(1);
        }
        if let Some(text) = auggie_response_text(exchange) {
            let complete_text = text.clone();
            let mut event = auggie_event(AuggieEventInput {
                provider_session_id: &projection.provider_session_id,
                provider_event_index,
                chat_index,
                role: EventRole::Assistant,
                label: "response",
                occurred_at: base_time + Duration::milliseconds(1),
                text,
                entry,
                exchange,
                raw_source_path: &projection.raw_source_path,
            });
            let native_id = event.provider_event_hash.clone().unwrap_or_default();
            attach_structured_complete_content_locator(
                CaptureProvider::Auggie,
                &mut event,
                0,
                u32::try_from(accepted_events).unwrap_or(u32::MAX),
                &native_id,
                record_bytes,
                &complete_text,
            )
            .map_err(ProviderProjectionFatal::new)?;
            let line = session_ordinal
                .saturating_mul(10_000)
                .saturating_add(chat_index.saturating_mul(2))
                .saturating_add(2);
            visit(ProviderNormalizationResult {
                captures: vec![(
                    line,
                    native_provider_capture(projection.base_draft.clone(), context, Some(event)),
                )],
                ..ProviderNormalizationResult::default()
            })?;
            provider_event_index = provider_event_index.saturating_add(1);
            accepted_events = accepted_events.saturating_add(1);
        }
    }

    if accepted_events == 0 {
        visit(ProviderNormalizationResult {
            captures: vec![(
                session_ordinal,
                native_provider_capture(projection.base_draft.clone(), context, None),
            )],
            ..ProviderNormalizationResult::default()
        })?;
    }
    Ok(accepted_events)
}

struct AuggieCapturedBatchProjector {
    context: ProviderAdapterContext,
    path: PathBuf,
    next_ordinal: u64,
    accepted_sessions: u64,
    accepted_events: u64,
}

impl AuggieCapturedBatchProjector {
    fn fresh(context: ProviderAdapterContext, path: PathBuf) -> Self {
        Self {
            context,
            path,
            next_ordinal: 0,
            accepted_sessions: 0,
            accepted_events: 0,
        }
    }

    fn resume(
        context: ProviderAdapterContext,
        path: PathBuf,
        cursor: &CertifiedProviderCursor,
    ) -> Result<Self> {
        let checkpoint: AuggieParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        if checkpoint.next_ordinal != whole_json_position_ordinal(cursor.native_position())? {
            return Err(CaptureError::InvalidPayload(
                "Auggie parser checkpoint does not match its native position".to_owned(),
            ));
        }
        Ok(Self {
            context,
            path,
            next_ordinal: checkpoint.next_ordinal,
            accepted_sessions: checkpoint.accepted_sessions,
            accepted_events: checkpoint.accepted_events,
        })
    }

    fn replay_summary(&self, rejected_records: u64) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::try_from(self.accepted_sessions).map_err(|_| {
            CaptureError::SystemInvariant("Auggie replay session count exceeds platform limits")
        })?;
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("Auggie replay event count exceeds platform limits")
        })?;
        let failed = usize::try_from(rejected_records).map_err(|_| {
            CaptureError::SystemInvariant("Auggie replay rejection count exceeds platform limits")
        })?;
        let skipped =
            skipped_sessions
                .checked_add(skipped_events)
                .ok_or(CaptureError::SystemInvariant(
                    "Auggie replay summary count overflowed",
                ))?;
        Ok(ProviderImportSummary {
            skipped,
            skipped_sessions,
            skipped_events,
            accepted_content_records: skipped_events,
            failed,
            ..ProviderImportSummary::default()
        })
    }
}

impl CapturedBatchProjector for AuggieCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != AUGGIE_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Auggie projector received an unexpected record kind",
            ));
        }
        if record.ordinal() != self.next_ordinal || record.ordinal() != 0 {
            return Err(ProviderProjectionFatal::system_invariant(
                "Auggie projector received an unexpected per-file ordinal",
            ));
        }
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Auggie projector requires whole-JSON native bytes",
            ));
        };
        self.next_ordinal = 1;
        let session = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                output.reject_record(1, format!("invalid Auggie session JSON: {error}"));
                return Ok(());
            }
        };
        let projection = match AuggieSessionProjection::new(&session, &self.path, &self.context) {
            Ok(projection) => projection,
            Err(error) => {
                output.reject_record(1, error.to_string());
                return Ok(());
            }
        };
        let accepted_events = visit_auggie_session_normalizations(
            &projection,
            &self.context,
            1,
            bytes,
            |normalization| emit_projected_normalization_units(output, normalization),
        )?;
        let accepted_events = u64::try_from(accepted_events)
            .map_err(|_| CaptureError::SystemInvariant("Auggie projected event count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_sessions = self
            .accepted_sessions
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Auggie projected session count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_events = self
            .accepted_events
            .checked_add(accepted_events)
            .ok_or(CaptureError::SystemInvariant(
                "Auggie projected event count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        Ok(())
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
            BoundedParserCheckpoint::from_serializable(&AuggieParserCheckpoint {
                next_ordinal: 0,
                accepted_sessions: 0,
                accepted_events: 0,
            })?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_ordinal = whole_json_position_ordinal(batch.range_end())?;
        if next_ordinal < self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Auggie projector advanced beyond the captured batch",
            ));
        }
        let cursor = CertifiedProviderCursor::new(
            batch.source().source_revision(),
            batch.source().capture_revision(),
            batch.source().policy_revision(),
            batch.range_end().clone(),
            BoundedParserCheckpoint::from_serializable(&AuggieParserCheckpoint {
                next_ordinal,
                accepted_sessions: self.accepted_sessions,
                accepted_events: self.accepted_events,
            })?,
        )?;
        Ok(CapturedBatchCursorFinish::Advance(cursor))
    }
}

pub(crate) fn import_auggie_sessions_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let mut merged = ProviderImportSummary::default();
    let source_count = visit_auggie_session_paths(path, &mut |session_path| {
        let summary =
            import_auggie_session_file_batched(session_path, store, &context, &import_options)?;
        merged.merge(summary);
        Ok(())
    })?;
    if source_count == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Auggie session JSON files were found",
        });
    }
    Ok(merged)
}

fn import_auggie_session_file_batched(
    path: PathBuf,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let observation = AuggieSessionObservation::read(&path)?;
    let path_identity = provider_path_identity(&observation.canonical_path)?;
    let file_context = ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(path.clone()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    };
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        &path_identity,
    );
    let source = SourceObservation::new(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        format!("auggie-session-file:{path_identity}"),
        observation.source_revision(),
        cursor_stream,
        AUGGIE_CAPTURE_REVISION,
        AUGGIE_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(auggie_captured_batch_error)?;
    let record_kind =
        ProviderRecordKind::new(AUGGIE_RECORD_KIND).map_err(auggie_captured_batch_error)?;
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
                        "Auggie per-file cursor exceeds its source".to_owned(),
                    ));
                }
                let projector = AuggieCapturedBatchProjector::resume(
                    file_context.clone(),
                    path.clone(),
                    &certified,
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
    let item_path = observation.session_file.path.clone();
    let item_length = observation.session_file.length;
    let source_item = path_identity.into_bytes();
    let mut producer = WholeJsonBatchProducer::new(source.clone(), record_kind, move || {
        if emitted {
            return Ok(None);
        }
        emitted = true;
        WholeJsonItem::new(0, source_item.clone(), item_length, item_path.clone()).map(Some)
    })
    .map_err(auggie_whole_json_error)?;
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &file_context)?;
    let batch = producer
        .next_batch()
        .map_err(auggie_whole_json_error)?
        .ok_or(CaptureError::SystemInvariant(
            "Auggie per-file producer returned no captured batch",
        ))?;
    let mut projector = match resumable_cursor.as_ref() {
        Some(cursor) if cursor_mode == CapturedBatchCursorMode::Resume => {
            AuggieCapturedBatchProjector::resume(file_context.clone(), path.clone(), cursor)?
        }
        _ => AuggieCapturedBatchProjector::fresh(file_context.clone(), path.clone()),
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
            "Auggie per-file import did not consume exactly one batch",
        ));
    }
    Ok(outcome.summary)
}

fn visit_auggie_session_paths(
    root: &Path,
    visit: &mut dyn FnMut(PathBuf) -> Result<()>,
) -> Result<usize> {
    let metadata = fs::symlink_metadata(root)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if file_type.is_file() {
        ensure_regular_provider_transcript_file(root)?;
        if root.extension().and_then(|extension| extension.to_str()) == Some("json") {
            visit(root.to_path_buf())?;
            return Ok(1);
        }
        return Ok(0);
    }
    if !file_type.is_dir() {
        return Ok(0);
    }
    let mut visited = 0_usize;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            visited = visited.saturating_add(visit_auggie_session_paths(&path, visit)?);
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("json")
        {
            ensure_regular_provider_transcript_file(&path)?;
            visit(path)?;
            visited = visited.saturating_add(1);
        }
    }
    Ok(visited)
}

fn whole_json_position(ordinal: u64) -> Result<NativePosition> {
    NativePosition::new(WHOLE_JSON_POSITION_KIND, ordinal.to_be_bytes().to_vec())
        .map_err(auggie_captured_batch_error)
}

fn whole_json_position_ordinal(position: &NativePosition) -> Result<u64> {
    if position.kind() != WHOLE_JSON_POSITION_KIND || position.value().len() != 8 {
        return Err(CaptureError::InvalidPayload(
            "Auggie cursor has an invalid whole-JSON position".to_owned(),
        ));
    }
    let bytes: [u8; 8] = position.value().try_into().map_err(|_| {
        CaptureError::InvalidPayload("Auggie cursor has an invalid whole-JSON ordinal".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

fn auggie_whole_json_error(error: WholeJsonBatchError) -> CaptureError {
    match error {
        WholeJsonBatchError::Io(error) => CaptureError::Io(error),
        WholeJsonBatchError::SourceSizeChanged { .. }
        | WholeJsonBatchError::SourceMetadataChangedDuringRead => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn auggie_captured_batch_error(error: crate::captured_batch::CapturedBatchError) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(crate) struct AuggieEventInput<'a> {
    pub(crate) provider_session_id: &'a str,
    pub(crate) provider_event_index: u64,
    pub(crate) chat_index: usize,
    pub(crate) role: EventRole,
    pub(crate) label: &'static str,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) text: String,
    pub(crate) entry: &'a Value,
    pub(crate) exchange: &'a Value,
    pub(crate) raw_source_path: &'a str,
}

pub(crate) fn auggie_event(input: AuggieEventInput<'_>) -> ProviderEventEnvelope {
    let request_id = input
        .exchange
        .get("request_id")
        .or_else(|| input.exchange.get("requestId"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty());
    let event_hash = request_id
        .map(|id| format!("{id}:{}", input.label))
        .unwrap_or_else(|| format!("chat-{}:{}", input.chat_index, input.label));
    let body = auggie_event_body(&input, request_id);
    native_event(NativeEventDraft {
        provider: CaptureProvider::Auggie,
        source_format: AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        provider_session_id: input.provider_session_id.to_owned(),
        provider_event_index: input.provider_event_index,
        provider_event_hash: Some(event_hash.clone()),
        cursor: format!("{}:{event_hash}", input.raw_source_path),
        event_type: EventType::Message,
        role: Some(input.role),
        occurred_at: input.occurred_at,
        text: input.text,
        body,
        metadata: json!({
            "source": "auggie_chat_history",
            "source_format": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            "chat_history_index": input.chat_index,
            "message_kind": input.label,
            "request_id": request_id,
            "sequence_id": input
                .entry
                .get("sequenceId")
                .or_else(|| input.entry.get("sequence_id"))
                .and_then(Value::as_u64),
            "completed": input.entry.get("completed").and_then(Value::as_bool),
            "source_kind": input.entry.get("source").and_then(Value::as_str),
        }),
    })
}

pub(crate) fn auggie_event_body(input: &AuggieEventInput<'_>, request_id: Option<&str>) -> Value {
    json!({
        "message_kind": input.label,
        "request_id": request_id,
        "raw_exchange_retention": "metadata_only",
        "sequence_id": input
            .entry
            .get("sequenceId")
            .or_else(|| input.entry.get("sequence_id"))
            .and_then(Value::as_u64),
        "completed": input.entry.get("completed").and_then(Value::as_bool),
        "source_kind": input.entry.get("source").and_then(Value::as_str),
        "request_node_count": auggie_node_count(
            input
                .exchange
                .get("request_nodes")
                .or_else(|| input.exchange.get("requestNodes")),
        ),
        "response_node_count": auggie_node_count(
            input
                .exchange
                .get("response_nodes")
                .or_else(|| input.exchange.get("responseNodes")),
        ),
        "tool_node_count": auggie_tool_node_count(input.exchange),
    })
}

fn auggie_node_count(value: Option<&Value>) -> Option<usize> {
    value.and_then(Value::as_array).map(Vec::len)
}

fn auggie_tool_node_count(exchange: &Value) -> usize {
    [
        "request_nodes",
        "requestNodes",
        "response_nodes",
        "responseNodes",
    ]
    .iter()
    .filter_map(|key| exchange.get(*key).and_then(Value::as_array))
    .flatten()
    .filter(|node| auggie_node_is_tool_metadata(node))
    .count()
}

pub(crate) fn auggie_entry_time(entry: &Value, exchange: Option<&Value>) -> Option<DateTime<Utc>> {
    provider_timestamp_from_fields(
        entry,
        &[
            "finishedAt",
            "finished_at",
            "createdAt",
            "created_at",
            "timestamp",
            "time",
        ],
    )
    .or_else(|| {
        exchange.and_then(|exchange| {
            provider_timestamp_from_fields(
                exchange,
                &[
                    "createdAt",
                    "created_at",
                    "updatedAt",
                    "updated_at",
                    "timestamp",
                    "time",
                ],
            )
        })
    })
}

pub(crate) fn auggie_request_text(exchange: &Value) -> Option<String> {
    provider_string_field(exchange, &["request_message", "requestMessage", "message"]).or_else(
        || {
            auggie_nodes_text(
                exchange
                    .get("request_nodes")
                    .or_else(|| exchange.get("requestNodes")),
            )
        },
    )
}

pub(crate) fn auggie_response_text(exchange: &Value) -> Option<String> {
    provider_string_field(exchange, &["response_text", "responseText", "response"]).or_else(|| {
        auggie_nodes_text(
            exchange
                .get("response_nodes")
                .or_else(|| exchange.get("responseNodes")),
        )
    })
}

pub(crate) fn auggie_nodes_text(value: Option<&Value>) -> Option<String> {
    let nodes = value?.as_array()?;
    let rendered = nodes
        .iter()
        .filter_map(auggie_node_text)
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>();
    (!rendered.is_empty()).then(|| rendered.join("\n"))
}

pub(crate) fn auggie_node_text(node: &Value) -> Option<String> {
    if auggie_node_is_tool_metadata(node) {
        return None;
    }
    node.pointer("/text_node/content")
        .or_else(|| node.pointer("/textNode/content"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| provider_block_text(node))
}

pub(crate) fn auggie_node_is_tool_metadata(node: &Value) -> bool {
    let tool_kind = node
        .get("type")
        .or_else(|| node.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| {
            matches!(
                kind,
                "tool"
                    | "tool_call"
                    | "tool-call"
                    | "tool_use"
                    | "tool-use"
                    | "tool_result"
                    | "tool-result"
                    | "tool_use_result"
                    | "function_call"
                    | "function_result"
            )
        });
    tool_kind
        || node.get("tool_name").is_some()
        || node.get("toolName").is_some()
        || node.get("tool_call").is_some()
        || node.get("toolCall").is_some()
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use crate::test_support_paths::tempdir;

    use crate::captured_batch::CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES;

    use super::*;

    include!("auggie_outcome_tests.rs");
}
