use std::{
    fs::{self, File, Metadata},
    io::BufReader,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::cell::Cell;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use ctx_history_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::captured_batch::jsonl::{
    initial_jsonl_position, jsonl_position_offset, verify_jsonl_append_boundary, JsonlBatchError,
    JsonlBatchProducer, VerifiedJsonlAppend,
};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, ProviderRecordKind,
    SourceObservation,
};
use crate::provider::providers::native_jsonl::{
    native_jsonl_missing_reason, visit_native_jsonl_files,
};

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::common::time::parse_optional_rfc3339_field;
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, emit_projected_normalization_units,
    provider_cursor_stream, provider_path_identity, provider_source_cursor_stream_for_path,
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchCursorMode,
    CapturedBatchProjector, CapturedSourceAdmission, CertifiedProviderCursor,
    ProviderProjectionFatal, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence,
};
use crate::{
    fnv1a64, CaptureError, NormalizedProviderImportOptions, PiSessionImportOptions,
    ProviderAdapterContext, ProviderImportSummary, ProviderNormalizationResult, Result,
    PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) const PI_SOURCE_FORMAT: &str = "pi_session_jsonl";
#[allow(dead_code)] // Registered by the universal locator integration branch.
pub(crate) const PI_RESULT_CONTENT_PROFILE: &str = "pi.result-body.v1";
const PI_CAPTURE_REVISION: u32 = 2;
const PI_POLICY_REVISION: u32 = 6;
const PI_RECORD_KIND: &str = "pi-session-jsonl-v1";

mod text;
#[allow(unused_imports)] // Consumed by the universal locator integration branch.
pub(crate) use text::pi_result_content;
use text::{pi_entry_text, pi_event_role, pi_message_has_tool_call};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PiParserCheckpoint {
    header: Option<PiSessionHeaderCheckpoint>,
    next_ordinal: u64,
    accepted_captures: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PiSessionHeaderCheckpoint {
    id: String,
    version: Option<u64>,
    timestamp: DateTime<Utc>,
    cwd: Option<String>,
    parent_session: Option<String>,
}

impl PiSessionHeaderCheckpoint {
    fn from_header(header: &PiSessionHeader) -> Self {
        Self {
            id: header.id.clone(),
            version: header.version,
            timestamp: header.timestamp,
            cwd: header.cwd.clone(),
            parent_session: header.parent_session.clone(),
        }
    }

    fn into_header(self) -> PiSessionHeader {
        let mut raw = serde_json::Map::new();
        raw.insert("type".to_owned(), Value::String("session".to_owned()));
        raw.insert("id".to_owned(), Value::String(self.id.clone()));
        raw.insert(
            "timestamp".to_owned(),
            Value::String(self.timestamp.to_rfc3339()),
        );
        if let Some(version) = self.version {
            raw.insert("version".to_owned(), Value::from(version));
        }
        if let Some(cwd) = &self.cwd {
            raw.insert("cwd".to_owned(), Value::String(cwd.clone()));
        }
        if let Some(parent_session) = &self.parent_session {
            raw.insert(
                "parentSession".to_owned(),
                Value::String(parent_session.clone()),
            );
        }
        PiSessionHeader {
            id: self.id,
            version: self.version,
            timestamp: self.timestamp,
            cwd: self.cwd,
            parent_session: self.parent_session,
            raw: Value::Object(raw),
        }
    }
}

struct PiCapturedBatchProjector {
    context: ProviderAdapterContext,
    header: Option<PiSessionHeader>,
    next_ordinal: u64,
    accepted_captures: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
}

impl PiCapturedBatchProjector {
    fn fresh(context: ProviderAdapterContext) -> Self {
        Self {
            context,
            header: None,
            next_ordinal: 0,
            accepted_captures: 0,
            accepted_events: 0,
            accepted_file_touches: 0,
        }
    }

    fn resume(context: ProviderAdapterContext, cursor: &CertifiedProviderCursor) -> Result<Self> {
        let checkpoint: PiParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        Ok(Self {
            context,
            header: checkpoint
                .header
                .map(PiSessionHeaderCheckpoint::into_header),
            next_ordinal: checkpoint.next_ordinal,
            accepted_captures: checkpoint.accepted_captures,
            accepted_events: checkpoint.accepted_events,
            accepted_file_touches: checkpoint.accepted_file_touches,
        })
    }

    fn advance_to(&mut self, ordinal: u64) -> Result<usize> {
        if ordinal < self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Pi captured record ordinal moved backwards",
            ));
        }
        self.next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Pi captured record ordinal overflowed",
        ))?;
        usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Pi captured record ordinal exceeds platform limits",
            ))
    }

    fn accept_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let captures = u64::try_from(normalization.captures.len())
            .map_err(|_| CaptureError::SystemInvariant("Pi projected capture count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        let events = u64::try_from(
            normalization
                .captures
                .iter()
                .filter(|(_, capture)| capture.event.is_some())
                .count(),
        )
        .map_err(|_| CaptureError::SystemInvariant("Pi projected event count exceeds u64"))
        .map_err(ProviderProjectionFatal::new)?;
        let file_touches = u64::try_from(normalization.files_touched.len())
            .map_err(|_| CaptureError::SystemInvariant("Pi projected file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_captures = self
            .accepted_captures
            .checked_add(captures)
            .ok_or(CaptureError::SystemInvariant(
                "Pi projected capture count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_events = self
            .accepted_events
            .checked_add(events)
            .ok_or(CaptureError::SystemInvariant(
                "Pi projected event count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touches)
            .ok_or(CaptureError::SystemInvariant(
                "Pi projected file-touch count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        emit_projected_normalization_units(output, normalization)
    }

    fn replay_summary(&self, failed: usize) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::from(self.accepted_captures != 0);
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("Pi replay event count exceeds platform limits")
        })?;
        let skipped_file_touches = usize::try_from(self.accepted_file_touches).map_err(|_| {
            CaptureError::SystemInvariant("Pi replay file-touch count exceeds platform limits")
        })?;
        let skipped = skipped_sessions
            .checked_add(skipped_events)
            .and_then(|value| value.checked_add(skipped_file_touches))
            .ok_or(CaptureError::SystemInvariant(
                "Pi replay summary count overflowed",
            ))?;
        Ok(ProviderImportSummary {
            skipped,
            skipped_sessions,
            skipped_events,
            accepted_content_records: skipped_events.saturating_add(skipped_file_touches),
            failed,
            ..ProviderImportSummary::default()
        })
    }
}

impl CapturedBatchProjector for PiCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != PI_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Pi projector received an unexpected record kind",
            ));
        }
        let line_number = self
            .advance_to(record.ordinal())
            .map_err(ProviderProjectionFatal::new)?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Pi projector requires native JSONL bytes",
            ));
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return self.accept_normalization(ProviderNormalizationResult::default(), output);
        }

        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                output.reject_record(line_number, error.to_string());
                return Ok(());
            }
        };
        let entry_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if entry_type == "session" {
            return match pi_session_header(value) {
                Ok(header) => {
                    self.header = Some(header);
                    self.accept_normalization(ProviderNormalizationResult::default(), output)
                }
                Err(error) => {
                    output.reject_record(line_number, error.to_string());
                    Ok(())
                }
            };
        }

        let Some(header) = self.header.as_ref() else {
            output.reject_record(
                line_number,
                "pi session entry appeared before session header".to_owned(),
            );
            return Ok(());
        };
        let result = crate::complete_content::jsonl::result_content_and_id(
            CaptureProvider::Pi,
            PI_SOURCE_FORMAT,
            &value,
            line_number,
        );
        match pi_session_capture(header, Some(&value), line_number, &self.context) {
            Ok(mut capture) => {
                if let Some(event) = capture.event.as_mut() {
                    crate::complete_content::jsonl::attach_jsonl_complete_content_locator(
                        event,
                        CaptureProvider::Pi,
                        PI_SOURCE_FORMAT,
                        &value,
                        record,
                        line_number,
                    )
                    .map_err(ProviderProjectionFatal::new)?;
                }
                if let (Some(event), Some((content, native_record_id))) =
                    (capture.event.as_mut(), result)
                {
                    crate::complete_content::jsonl::attach_jsonl_result_content_locator(
                        event,
                        CaptureProvider::Pi,
                        PI_SOURCE_FORMAT,
                        &content,
                        &native_record_id,
                        record,
                    )
                    .map_err(ProviderProjectionFatal::new)?;
                }
                self.accept_normalization(
                    ProviderNormalizationResult {
                        captures: vec![(line_number, capture)],
                        ..ProviderNormalizationResult::default()
                    },
                    output,
                )
            }
            Err(error) => {
                output.reject_record(line_number, error.to_string());
                Ok(())
            }
        }
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
            BoundedParserCheckpoint::from_serializable(&PiParserCheckpoint {
                header: None,
                next_ordinal: 0,
                accepted_captures: 0,
                accepted_events: 0,
                accepted_file_touches: 0,
            })?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_ordinal = batch
            .records()
            .last()
            .and_then(|record| record.ordinal().checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Pi captured batch did not have a next ordinal",
            ))?;
        if self.next_ordinal > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Pi projector advanced beyond the captured batch",
            ));
        }
        let checkpoint = PiParserCheckpoint {
            header: self
                .header
                .as_ref()
                .map(PiSessionHeaderCheckpoint::from_header),
            next_ordinal,
            accepted_captures: self.accepted_captures,
            accepted_events: self.accepted_events,
            accepted_file_touches: self.accepted_file_touches,
        };
        let cursor = CertifiedProviderCursor::new(
            batch.source().source_revision(),
            batch.source().capture_revision(),
            batch.source().policy_revision(),
            batch.range_end().clone(),
            BoundedParserCheckpoint::from_serializable(&checkpoint)?,
        )?;
        Ok(CapturedBatchCursorFinish::Advance(cursor))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PiFrozenFileMetadata {
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl PiFrozenFileMetadata {
    fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        Self::from_metadata(&fs::symlink_metadata(path)?)
    }

    fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    fn source_revision(&self) -> String {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        format!(
            "pi-jsonl-metadata-v1:length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={}",
            self.length,
            self.readonly,
            self.device.map_or_else(|| "none".to_owned(), |value| value.to_string()),
            self.inode.map_or_else(|| "none".to_owned(), |value| value.to_string()),
        )
    }

    fn revalidate(&self, path: &Path) -> Result<bool> {
        match Self::read(path) {
            Ok(current) => Ok(current == *self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static PI_SOURCE_FILE_OPEN_COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

fn open_pi_source_file(path: &Path) -> Result<File> {
    #[cfg(test)]
    PI_SOURCE_FILE_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    Ok(File::open(path)?)
}

#[cfg(test)]
fn count_pi_source_file_opens<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    PI_SOURCE_FILE_OPEN_COUNT.with(|count| {
        assert_eq!(count.replace(Some(0)), None);
    });
    let output = operation();
    let opens = PI_SOURCE_FILE_OPEN_COUNT.with(|count| count.replace(None).unwrap());
    (output, opens)
}

fn pi_verified_append_cursor_mode(verified_append: VerifiedJsonlAppend) -> CapturedBatchCursorMode {
    CapturedBatchCursorMode::ResumeAppend(verified_append)
}

pub(crate) struct PiSessionHeader {
    pub(crate) id: String,
    pub(crate) version: Option<u64>,
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) cwd: Option<String>,
    pub(crate) parent_session: Option<String>,
    pub(crate) raw: Value,
}

pub(crate) fn import_pi_session_jsonl_batched(
    path: &Path,
    store: &mut Store,
    options: PiSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let import_options = NormalizedProviderImportOptions {
        history_record_id: options.history_record_id,
        persist_cursors: false,
        wrap_transaction: true,
        fast_event_inserts: true,
        capture_work_limit: options.capture_work_limit,
        inventory_observation_token: options.inventory_observation_token.clone(),
    };
    if fs::symlink_metadata(path)?.file_type().is_file() {
        let source_root = options
            .source_path
            .clone()
            .unwrap_or_else(|| path.to_path_buf());
        return import_pi_session_jsonl_file_batched(
            path,
            store,
            ProviderAdapterContext {
                machine_id: options.machine_id,
                source_path: Some(path.to_path_buf()),
                source_root: Some(source_root),
                imported_at: options.imported_at,
            },
            import_options,
        );
    }

    let source_root = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let mut merged = ProviderImportSummary::default();
    let source_count = visit_native_jsonl_files(path, CaptureProvider::Pi, &mut |file_path| {
        let summary = import_pi_session_jsonl_file_batched(
            file_path,
            store,
            ProviderAdapterContext {
                machine_id: options.machine_id.clone(),
                source_path: Some(file_path.to_path_buf()),
                source_root: Some(source_root.clone()),
                imported_at: options.imported_at,
            },
            import_options.clone(),
        )?;
        merged.merge(summary);
        Ok(())
    })?;
    if source_count == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::Pi),
        });
    }
    Ok(merged)
}

pub(crate) fn import_pi_session_jsonl_file_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let frozen = PiFrozenFileMetadata::read(path)?;
    let canonical_path = fs::canonicalize(path)?;
    let cursor_source_path =
        provider_path_identity(context.source_path.as_deref().unwrap_or(path))?;
    let canonical_path_identity = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Pi,
        PI_SOURCE_FORMAT,
        &cursor_source_path,
    );
    let source = SourceObservation::new(
        CaptureProvider::Pi,
        PI_SOURCE_FORMAT,
        format!("pi-jsonl-file:{canonical_path_identity}"),
        frozen.source_revision(),
        cursor_stream,
        PI_CAPTURE_REVISION,
        PI_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(pi_captured_batch_error)?;
    let source_item = canonical_path_identity.into_bytes();
    let record_kind = ProviderRecordKind::new(PI_RECORD_KIND).map_err(pi_captured_batch_error)?;

    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let had_expected_store_cursor = expected_store_cursor.is_some();
    let initial_position = initial_jsonl_position().map_err(pi_jsonl_batch_error)?;
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut start_offset = 0;
    let mut start_ordinal = 0;
    let mut projector = PiCapturedBatchProjector::fresh(context.clone());

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let can_resume = if certified.source_revision() == source.source_revision() {
                    true
                } else {
                    let file = open_pi_source_file(path)?;
                    if PiFrozenFileMetadata::from_metadata(&file.metadata()?)? != frozen {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    let mut reader = BufReader::new(file);
                    match verify_jsonl_append_boundary(
                        &mut reader,
                        certified.native_position(),
                        &source,
                        frozen.length,
                    ) {
                        Ok(verified) => {
                            cursor_mode = pi_verified_append_cursor_mode(verified);
                            true
                        }
                        Err(JsonlBatchError::Io(error)) => return Err(CaptureError::Io(error)),
                        Err(_) => false,
                    }
                };
                if can_resume {
                    start_offset = jsonl_position_offset(certified.native_position())
                        .map_err(pi_jsonl_batch_error)?;
                    projector = PiCapturedBatchProjector::resume(context.clone(), &certified)?;
                    start_ordinal = projector.next_ordinal;
                } else {
                    cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                }
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    if !frozen.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let file = open_pi_source_file(path)?;
    if PiFrozenFileMetadata::from_metadata(&file.metadata()?)? != frozen {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut producer = JsonlBatchProducer::new(
        BufReader::new(file),
        source,
        source_item,
        record_kind,
        frozen.length,
        start_offset,
        start_ordinal,
        false,
    )
    .map_err(pi_jsonl_batch_error)?;
    let mut imported_any = false;
    let summary = drain_captured_batches(
        store,
        &admission,
        import_options,
        &context.machine_id,
        context.imported_at,
        expected_store_cursor,
        &initial_position,
        cursor_mode,
        &stream,
        &mut projector,
        || {
            let batch = producer.next_batch().map_err(pi_jsonl_batch_error)?;
            imported_any |= batch.is_some();
            Ok(batch)
        },
        || frozen.revalidate(path),
    )?;
    if !imported_any && had_expected_store_cursor {
        projector.replay_summary(summary.failed)
    } else {
        Ok(summary)
    }
}

fn pi_jsonl_batch_error(error: JsonlBatchError) -> CaptureError {
    match error {
        JsonlBatchError::Io(error) => CaptureError::Io(error),
        JsonlBatchError::SourceChangedDuringRead { .. } => CaptureError::SourceChangedDuringCapture,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn pi_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(crate) fn pi_session_header(value: Value) -> Result<PiSessionHeader> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| CaptureError::InvalidPayload("pi session header missing id".to_owned()))?
        .to_owned();
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CaptureError::InvalidPayload("pi session header missing timestamp".to_owned())
        })
        .and_then(|timestamp| {
            DateTime::parse_from_rfc3339(timestamp)
                .map(|time| time.with_timezone(&Utc))
                .map_err(CaptureError::from)
        })?;
    Ok(PiSessionHeader {
        id,
        version: value.get("version").and_then(Value::as_u64),
        timestamp,
        cwd: value.get("cwd").and_then(Value::as_str).map(str::to_owned),
        parent_session: value
            .get("parentSession")
            .and_then(Value::as_str)
            .map(str::to_owned),
        raw: value,
    })
}

pub(crate) fn pi_session_capture(
    header: &PiSessionHeader,
    entry: Option<&Value>,
    line_number: usize,
    context: &ProviderAdapterContext,
) -> Result<ProviderCaptureEnvelope> {
    let event = entry
        .map(|entry| pi_session_event(header, entry, line_number))
        .transpose()?;
    let cursor = event.as_ref().and_then(|event| {
        event.cursor.as_ref().map(|cursor| ProviderCursorRange {
            before: None,
            after: Some(ProviderCursorCheckpoint {
                stream: provider_cursor_stream(CaptureProvider::Pi, "pi_session_jsonl"),
                cursor: cursor.clone(),
                observed_at: event.occurred_at,
            }),
        })
    });

    Ok(ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::Pi,
        source: ProviderSourceEnvelope {
            source_format: "pi_session_jsonl".to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: context
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
            source_root: context.source_root_display(),
            trust: ProviderSourceTrust::ProviderExport,
            fidelity: Fidelity::Imported,
            cursor,
            idempotency_key: Some(format!("provider-source:pi:pi_session_jsonl:{}", header.id)),
            metadata: json!({
                "adapter": "pi_session_jsonl",
                "source_fidelity": "documented_session_jsonl",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: header.id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            started_at: header.timestamp,
            ended_at: None,
            cwd: header.cwd.clone(),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!("provider-session:pi:{}", header.id)),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": "pi_session_jsonl",
                "source_fidelity": "documented_session_jsonl",
                "version": header.version,
                "parent_session": header.parent_session,
                "header": header.raw,
                "limitations": [
                    "message branch parentId values are preserved as event metadata, not ctx session edges",
                    "files touched are available only when Pi message payloads include them",
                    "raw image content is not expanded into artifacts by this importer"
                ],
            }),
        },
        event,
    })
}

pub(crate) fn pi_session_event(
    header: &PiSessionHeader,
    entry: &Value,
    line_number: usize,
) -> Result<ProviderEventEnvelope> {
    let entry_type = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = entry.get("message");
    let message_role = message
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str);
    let occurred_at = parse_optional_rfc3339_field(entry, "timestamp")?.ok_or_else(|| {
        CaptureError::InvalidPayload("pi session event missing timestamp".to_owned())
    })?;
    let event_type = pi_event_type(entry_type, message);
    let role = message_role.map(pi_event_role);
    let payload = pi_normalized_event_payload(entry, event_type);
    let provider_event_index = (line_number - 1) as u64;
    let provider_event_identity_index =
        pi_provider_event_identity_index(header, entry).unwrap_or(provider_event_index);
    let legacy_provider_event_index = provider_event_index;

    Ok(ProviderEventEnvelope {
        provider_event_index,
        provider_event_hash: None,
        cursor: entry.get("id").and_then(Value::as_str).map(str::to_owned),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: Some(pi_event_idempotency_key(header, entry, line_number)),
        artifacts: Vec::new(),
        payload,
        metadata: json!({
            "source": "pi_session",
            "source_format": "pi_session_jsonl",
            "line": line_number,
            "entry_type": entry_type,
            "entry_id": entry.get("id").and_then(Value::as_str),
            "parent_id": entry.get("parentId").and_then(Value::as_str),
            "provider_event_identity_index": provider_event_identity_index,
            "legacy_provider_event_index": legacy_provider_event_index,
            "message_role": message_role,
            "model": message
                .and_then(|message| message.get("model"))
                .and_then(Value::as_str),
            "provider": message
                .and_then(|message| message.get("provider"))
                .and_then(Value::as_str),
            "usage": message.and_then(|message| message.get("usage")).cloned(),
        }),
    })
}

/// Pure message normalization shared by capture and verified source reopening.
///
/// The native entry ID is preferred; records without one remain addressable by
/// their exact JSONL line and are still protected by record/content hashes.
pub(crate) fn pi_complete_content_message_record(
    entry: &Value,
    line_number: usize,
) -> Option<(String, String)> {
    let entry_type = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = entry.get("message");
    (pi_event_type(entry_type, message) == EventType::Message).then(|| {
        (
            pi_entry_text(entry, message).unwrap_or_default(),
            entry
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("line-{line_number}")),
        )
    })
}

/// Rebuilds the exact normalized payload used by Store event hashing.
pub(crate) fn pi_complete_content_normalized_payload(entry: &Value) -> Option<Value> {
    let entry_type = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = entry.get("message");
    let event_type = pi_event_type(entry_type, message);
    if event_type != EventType::Message {
        return None;
    }
    Some(pi_normalized_event_payload(entry, event_type))
}

fn pi_normalized_event_payload(entry: &Value, event_type: EventType) -> Value {
    let entry_type = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = entry.get("message");
    let message_role = message
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str);
    let text = pi_entry_text(entry, message).unwrap_or_default();
    let retained_text = provider_policy_event_text(event_type, &text, entry);
    let result_evidence = provider_result_identifier_evidence(event_type, &text, entry);
    let result_outcome = provider_result_outcome_evidence(event_type, entry);
    let command = message
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str);
    let exit_code = message
        .and_then(|value| value.get("exitCode"))
        .and_then(Value::as_i64);
    json!({
        "entry_type": entry_type,
        "entry_id": entry.get("id").and_then(Value::as_str),
        "parent_id": entry.get("parentId").and_then(Value::as_str),
        "message_role": message_role,
        "command": command,
        "exit_code": exit_code,
        "text": retained_text.text,
        "text_retention": retained_text.retention.as_json(),
        "result_evidence": result_evidence,
        "result_outcome": result_outcome,
        "body": provider_capped_json(
            &provider_policy_body(event_type, entry),
            PROVIDER_MAX_PREVIEW_CHARS,
        ),
    })
}

pub(crate) fn pi_provider_event_identity_index(
    header: &PiSessionHeader,
    entry: &Value,
) -> Option<u64> {
    entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| fnv1a64(format!("pi:{}:{id}", header.id).as_bytes()))
}

pub(crate) fn pi_event_idempotency_key(
    header: &PiSessionHeader,
    entry: &Value,
    line_number: usize,
) -> String {
    entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("provider-event:pi:{}:{id}", header.id))
        .unwrap_or_else(|| format!("provider-event:pi:{}:{line_number}", header.id))
}

pub(crate) fn pi_event_type(entry_type: &str, message: Option<&Value>) -> EventType {
    match entry_type {
        "compaction" | "branch_summary" => EventType::Summary,
        "message" => match message
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
        {
            "toolResult" => EventType::ToolOutput,
            "bashExecution" => EventType::CommandOutput,
            "assistant" if message.is_some_and(pi_message_has_tool_call) => EventType::ToolCall,
            _ => EventType::Message,
        },
        "model_change"
        | "thinking_level_change"
        | "custom"
        | "custom_message"
        | "label"
        | "session_info" => EventType::Notice,
        _ => EventType::Notice,
    }
}

#[cfg(test)]
mod tests {
    include!("pi/tests.rs");
}
