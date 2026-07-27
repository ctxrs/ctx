use std::{
    fs::{self, File, Metadata},
    io::BufReader,
    num::NonZeroUsize,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::cell::Cell;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use ctx_history_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::captured_batch::jsonl::{
    initial_jsonl_position, jsonl_position_offset, verify_jsonl_append_boundary, JsonlBatchError,
    JsonlBatchProducer,
};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, ProviderRecordKind,
    SourceObservation, CAPTURE_BATCH_MAX_BATCHES_PER_GROUP,
};

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::provider::importer::{
    captured_batch_cursor_stream, emit_projected_normalization_units, import_captured_batches,
    provider_cursor_stream, provider_path_identity, provider_source_cursor_stream_for_path,
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchCursorMode,
    CapturedBatchProjector, CapturedSourceAdmission, CertifiedProviderCursor,
    ProviderProjectionFatal, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::{
    CaptureError, CodexHistoryImportOptions, NormalizedProviderImportOptions,
    ProviderAdapterContext, ProviderImportSummary, ProviderNormalizationResult, Result,
};
const CODEX_HISTORY_CAPTURE_REVISION: u32 = 1;
const CODEX_HISTORY_POLICY_REVISION: u32 = 1;
const CODEX_HISTORY_RECORD_KIND: &str = "codex-history-jsonl-v1";
const CODEX_HISTORY_SOURCE_FORMAT: &str = "codex_history_jsonl";

#[derive(Debug, Deserialize)]
pub(crate) struct CodexHistoryLine {
    pub(crate) session_id: String,
    pub(crate) ts: i64,
    pub(crate) text: String,
}
pub fn import_codex_history_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CodexHistoryImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_codex_history_jsonl_batched(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexHistoryParserCheckpoint {
    next_ordinal: u64,
    accepted_events: u64,
    ignored_records: u64,
    session_runs: u64,
    last_session_hash: Option<[u8; 32]>,
}

struct CodexHistoryCapturedBatchProjector {
    context: ProviderAdapterContext,
    next_ordinal: u64,
    accepted_events: u64,
    ignored_records: u64,
    session_runs: u64,
    last_session_hash: Option<[u8; 32]>,
}

impl CodexHistoryCapturedBatchProjector {
    fn fresh(context: ProviderAdapterContext) -> Self {
        Self {
            context,
            next_ordinal: 0,
            accepted_events: 0,
            ignored_records: 0,
            session_runs: 0,
            last_session_hash: None,
        }
    }

    fn resume(context: ProviderAdapterContext, cursor: &CertifiedProviderCursor) -> Result<Self> {
        let checkpoint: CodexHistoryParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        Ok(Self {
            context,
            next_ordinal: checkpoint.next_ordinal,
            accepted_events: checkpoint.accepted_events,
            ignored_records: checkpoint.ignored_records,
            session_runs: checkpoint.session_runs,
            last_session_hash: checkpoint.last_session_hash,
        })
    }

    fn line_number(&mut self, ordinal: u64) -> Result<usize> {
        if ordinal < self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Codex history record ordinal moved backwards",
            ));
        }
        self.next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Codex history record ordinal overflowed",
        ))?;
        usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Codex history record ordinal exceeds platform limits",
            ))
    }

    fn reject_record(
        output: &mut dyn ProviderProjectionOutput,
        line_number: usize,
        reason: String,
    ) {
        output.reject_record(line_number, reason);
    }

    fn checkpoint(&self, next_ordinal: u64) -> CodexHistoryParserCheckpoint {
        CodexHistoryParserCheckpoint {
            next_ordinal,
            accepted_events: self.accepted_events,
            ignored_records: self.ignored_records,
            session_runs: self.session_runs,
            last_session_hash: self.last_session_hash,
        }
    }

    fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant(
                "Codex history replay event count exceeds platform limits",
            )
        })?;
        let skipped_sessions = usize::try_from(self.session_runs).map_err(|_| {
            CaptureError::SystemInvariant(
                "Codex history replay session-run count exceeds platform limits",
            )
        })?;
        let accounted = self
            .accepted_events
            .checked_add(self.ignored_records)
            .ok_or(CaptureError::SystemInvariant(
                "Codex history replay record count overflowed",
            ))?;
        let rejected_records =
            self.next_ordinal
                .checked_sub(accounted)
                .ok_or(CaptureError::SystemInvariant(
                    "Codex history replay accounting exceeds the captured record count",
                ))?;
        let failed = usize::try_from(rejected_records).map_err(|_| {
            CaptureError::SystemInvariant(
                "Codex history replay rejection count exceeds platform limits",
            )
        })?;
        let skipped =
            skipped_sessions
                .checked_add(skipped_events)
                .ok_or(CaptureError::SystemInvariant(
                    "Codex history replay summary count overflowed",
                ))?;
        Ok(ProviderImportSummary {
            skipped,
            failed,
            skipped_sessions,
            skipped_events,
            accepted_content_records: skipped_events,
            ..ProviderImportSummary::default()
        })
    }
}

impl CapturedBatchProjector for CodexHistoryCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != CODEX_HISTORY_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Codex history projector received an unexpected record kind",
            ));
        }
        let line_number = self
            .line_number(record.ordinal())
            .map_err(ProviderProjectionFatal::new)?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Codex history projector requires native bytes",
            ));
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            self.ignored_records = self.ignored_records.checked_add(1).ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Codex history ignored-record count overflowed",
                )
            })?;
            return Ok(());
        }

        let history = match serde_json::from_slice::<CodexHistoryLine>(bytes) {
            Ok(history) => history,
            Err(error) => {
                Self::reject_record(output, line_number, error.to_string());
                return Ok(());
            }
        };
        if history.session_id.trim().is_empty() {
            Self::reject_record(
                output,
                line_number,
                "codex history line has empty session_id".to_owned(),
            );
            return Ok(());
        }
        let Some(occurred_at) = DateTime::from_timestamp(history.ts, 0) else {
            Self::reject_record(
                output,
                line_number,
                format!(
                    "codex history line has invalid unix timestamp {}",
                    history.ts
                ),
            );
            return Ok(());
        };

        let session_hash: [u8; 32] = Sha256::digest(history.session_id.as_bytes()).into();
        if self.last_session_hash != Some(session_hash) {
            self.session_runs = self.session_runs.checked_add(1).ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Codex history session-run count overflowed",
                )
            })?;
            self.last_session_hash = Some(session_hash);
        }
        self.accepted_events = self.accepted_events.checked_add(1).ok_or_else(|| {
            ProviderProjectionFatal::system_invariant(
                "Codex history accepted-event count overflowed",
            )
        })?;
        emit_projected_normalization_units(
            output,
            codex_history_normalization(&self.context, history, occurred_at, line_number),
        )
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if *position != initial_jsonl_position().map_err(codex_history_jsonl_batch_error)? {
            return Err(CaptureError::InvalidPayload(
                "Codex history initial cursor candidate is not at the JSONL source start"
                    .to_owned(),
            ));
        }
        if self.next_ordinal != 0
            || self.accepted_events != 0
            || self.ignored_records != 0
            || self.session_runs != 0
            || self.last_session_hash.is_some()
        {
            return Err(CaptureError::SystemInvariant(
                "Codex history initial cursor candidate requires fresh projector state",
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
                "Codex history captured batch did not have a next ordinal",
            ))?;
        if self.next_ordinal > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Codex history projector advanced beyond the captured batch",
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

fn codex_history_normalization(
    context: &ProviderAdapterContext,
    history: CodexHistoryLine,
    occurred_at: DateTime<Utc>,
    line_number: usize,
) -> ProviderNormalizationResult {
    let source_format = CODEX_HISTORY_SOURCE_FORMAT;
    let session_id = history.session_id;
    ProviderNormalizationResult {
        captures: vec![(
            line_number,
            ProviderCaptureEnvelope {
                schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                provider: CaptureProvider::Codex,
                source: ProviderSourceEnvelope {
                    source_format: source_format.to_owned(),
                    machine_id: context.machine_id.clone(),
                    observed_at: context.imported_at,
                    raw_source_path: context
                        .source_path
                        .as_ref()
                        .map(|path| path.display().to_string()),
                    source_root: context.source_root_display(),
                    trust: ProviderSourceTrust::ProviderExport,
                    fidelity: Fidelity::SummaryOnly,
                    cursor: Some(ProviderCursorRange {
                        before: None,
                        after: Some(ProviderCursorCheckpoint {
                            stream: provider_cursor_stream(CaptureProvider::Codex, source_format),
                            cursor: format!("line:{line_number}"),
                            observed_at: occurred_at,
                        }),
                    }),
                    idempotency_key: Some(format!(
                        "provider-source:{}:{}:{}",
                        CaptureProvider::Codex.as_str(),
                        source_format,
                        session_id
                    )),
                    metadata: json!({
                        "adapter": "codex_history_jsonl",
                        "source_fidelity": "prompt_log_only",
                    }),
                },
                session: ProviderSessionEnvelope {
                    provider_session_id: session_id.clone(),
                    parent_provider_session_id: None,
                    root_provider_session_id: None,
                    external_agent_id: None,
                    agent_type: AgentType::Primary,
                    role_hint: Some("primary".to_owned()),
                    is_primary: true,
                    status: SessionStatus::Imported,
                    // Each bounded record contributes a deterministic candidate. The generic
                    // session merge must retain the minimum candidate to match legacy first_seen.
                    started_at: occurred_at,
                    ended_at: None,
                    cwd: None,
                    fidelity: Fidelity::SummaryOnly,
                    idempotency_key: Some(format!(
                        "provider-session:{}:{}",
                        CaptureProvider::Codex.as_str(),
                        session_id
                    )),
                    artifacts: Vec::new(),
                    metadata: json!({
                        "source_format": source_format,
                        "source_fidelity": "prompt_log_only",
                        "limitations": [
                            "user prompts only",
                            "no assistant responses",
                            "no tool calls",
                            "no command output",
                            "no child session relationships"
                        ],
                    }),
                },
                event: Some(ProviderEventEnvelope {
                    provider_event_index: (line_number - 1) as u64,
                    provider_event_hash: None,
                    cursor: Some(format!("line:{line_number}")),
                    event_type: EventType::Message,
                    role: Some(EventRole::User),
                    occurred_at,
                    fidelity: Fidelity::SummaryOnly,
                    idempotency_key: Some(format!(
                        "provider-event:{}:{}:{}",
                        CaptureProvider::Codex.as_str(),
                        session_id,
                        line_number - 1
                    )),
                    artifacts: Vec::new(),
                    payload: json!({
                        "text": history.text,
                        "source_format": source_format,
                    }),
                    metadata: json!({
                        "source": "codex_history",
                        "source_format": source_format,
                        "source_fidelity": "prompt_log_only",
                    }),
                }),
            },
        )],
        ..ProviderNormalizationResult::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexHistoryFrozenFileMetadata {
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl CodexHistoryFrozenFileMetadata {
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
            "codex-history-metadata-v1:length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={}",
            self.length,
            self.readonly,
            self.device
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            self.inode
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
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
    static CODEX_HISTORY_SOURCE_FILE_OPEN_COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

fn open_codex_history_source_file(path: &Path) -> Result<File> {
    #[cfg(test)]
    CODEX_HISTORY_SOURCE_FILE_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    Ok(File::open(path)?)
}

#[cfg(test)]
fn count_codex_history_source_file_opens<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    CODEX_HISTORY_SOURCE_FILE_OPEN_COUNT.with(|count| {
        assert_eq!(count.replace(Some(0)), None);
    });
    let output = operation();
    let opens =
        CODEX_HISTORY_SOURCE_FILE_OPEN_COUNT.with(|count| count.replace(None).unwrap_or_default());
    (output, opens)
}

fn import_codex_history_jsonl_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let frozen = CodexHistoryFrozenFileMetadata::read(path)?;
    let canonical_path = fs::canonicalize(path)?;
    let cursor_source_path =
        provider_path_identity(context.source_path.as_deref().unwrap_or(path))?;
    let canonical_path_identity = provider_path_identity(&canonical_path)?;
    let source_format = CODEX_HISTORY_SOURCE_FORMAT;
    let source = SourceObservation::new(
        CaptureProvider::Codex,
        source_format,
        format!("codex-history-file:{canonical_path_identity}"),
        frozen.source_revision(),
        provider_source_cursor_stream_for_path(
            CaptureProvider::Codex,
            source_format,
            &cursor_source_path,
        ),
        CODEX_HISTORY_CAPTURE_REVISION,
        CODEX_HISTORY_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(codex_history_captured_batch_error)?;
    let source_item = canonical_path_identity.into_bytes();
    let record_kind = ProviderRecordKind::new(CODEX_HISTORY_RECORD_KIND)
        .map_err(codex_history_captured_batch_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let mut expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_jsonl_position().map_err(codex_history_jsonl_batch_error)?;
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut start_offset = 0_u64;
    let mut start_ordinal = 0_u64;
    let mut resumed_projector = None;

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let can_resume = if certified.source_revision() == source.source_revision() {
                    true
                } else {
                    let file = open_codex_history_source_file(path)?;
                    if CodexHistoryFrozenFileMetadata::from_metadata(&file.metadata()?)? != frozen {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    let mut reader = BufReader::new(file);
                    match verify_jsonl_append_boundary(
                        &mut reader,
                        certified.native_position(),
                        &source,
                        frozen.length,
                    ) {
                        Ok(verified_append) => {
                            cursor_mode = CapturedBatchCursorMode::ResumeAppend(verified_append);
                            true
                        }
                        Err(JsonlBatchError::Io(error)) => return Err(CaptureError::Io(error)),
                        Err(_) => false,
                    }
                };
                if can_resume {
                    start_offset = jsonl_position_offset(certified.native_position())
                        .map_err(codex_history_jsonl_batch_error)?;
                    let projector =
                        CodexHistoryCapturedBatchProjector::resume(context.clone(), &certified)?;
                    start_ordinal = projector.next_ordinal;
                    resumed_projector = Some(projector);
                } else {
                    cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                }
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let mut projector = resumed_projector
        .unwrap_or_else(|| CodexHistoryCapturedBatchProjector::fresh(context.clone()));
    if !frozen.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let file = open_codex_history_source_file(path)?;
    if CodexHistoryFrozenFileMetadata::from_metadata(&file.metadata()?)? != frozen {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut producer = JsonlBatchProducer::new(
        BufReader::new(file),
        source.clone(),
        source_item,
        record_kind,
        frozen.length,
        start_offset,
        start_ordinal,
        false,
    )
    .map_err(codex_history_jsonl_batch_error)?;
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let max_batches = NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).ok_or(
        CaptureError::SystemInvariant("captured batch group limit must be nonzero"),
    )?;
    let mut merged = ProviderImportSummary::default();
    let mut imported_any = false;
    loop {
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
            || {
                producer
                    .next_batch()
                    .map_err(codex_history_jsonl_batch_error)
            },
            || frozen.revalidate(path),
        )?;
        if outcome.batches_imported == 0 {
            return if imported_any {
                Ok(merged)
            } else if expected_store_cursor.is_some() {
                projector.replay_summary()
            } else {
                Ok(merged)
            };
        }
        imported_any = true;
        merged.merge(outcome.summary);
        if outcome.source_exhausted {
            return Ok(merged);
        }
        if import_options.capture_work_limit == crate::CaptureWorkLimit::OneSafeGroup {
            merged.work_remaining = true;
            return Ok(merged);
        }
        expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
        if expected_store_cursor.is_none() {
            return Err(CaptureError::SystemInvariant(
                "published Codex history captured-batch cursor could not be reloaded",
            ));
        }
        cursor_mode = CapturedBatchCursorMode::Resume;
    }
}

fn codex_history_jsonl_batch_error(error: JsonlBatchError) -> CaptureError {
    match error {
        JsonlBatchError::Io(error) => CaptureError::Io(error),
        JsonlBatchError::SourceChangedDuringRead { .. } => CaptureError::SourceChangedDuringCapture,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn codex_history_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
