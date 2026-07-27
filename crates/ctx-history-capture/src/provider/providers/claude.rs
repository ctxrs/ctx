use std::{
    fs::{self, File, Metadata},
    io::BufReader,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

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
    JsonlBatchProducer,
};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, ProviderRecordKind,
    SourceObservation,
};

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::common::time::parse_rfc3339_utc;
use crate::provider::file_touches::{
    visit_provider_file_touches_from_raw_value, ProviderFileTouchSourceContext,
    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, emit_projected_normalization_units,
    provider_cursor_stream, provider_path_identity, provider_source_cursor_stream_for_path,
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchCursorMode,
    CapturedBatchProjector, CapturedSourceAdmission, CertifiedProviderCursor,
    ProviderProjectionFatal, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence, provider_role,
    provider_value_text,
};
use crate::provider::providers::native_jsonl::{
    native_jsonl_missing_reason, visit_native_jsonl_files,
};
use crate::{
    CaptureError, ClaudeProjectsImportOptions, NormalizedProviderImportOptions,
    ProviderAdapterContext, ProviderImportSummary, ProviderNormalizationResult, Result,
    CLAUDE_PROJECTS_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

mod complete_content;

pub(crate) use complete_content::{
    claude_complete_content_message_record, claude_complete_content_normalized_payload,
    claude_event_type, claude_result_content, CLAUDE_RESULT_CONTENT_PROFILE,
};

const CLAUDE_CAPTURE_REVISION: u32 = 1;
const CLAUDE_POLICY_REVISION: u32 = 6;
const CLAUDE_RECORD_KIND: &str = "claude-project-jsonl-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeParserCheckpoint {
    session: Option<ClaudeSessionCheckpoint>,
    next_ordinal: u64,
    accepted_captures: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeSessionCheckpoint {
    native_session_id: String,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    external_agent_id: Option<String>,
    is_subagent: bool,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    version: Option<String>,
    git_branch: Option<String>,
}

impl ClaudeSessionCheckpoint {
    fn from_first_record(path: &Path, context: &ProviderAdapterContext, value: &Value) -> Self {
        let file_stem = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown-session");
        let native_session_id = value
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .unwrap_or(file_stem)
            .to_owned();
        let (provider_session_id, parent_provider_session_id, external_agent_id, is_subagent) =
            claude_path_session_ids(path, &native_session_id);
        Self {
            native_session_id,
            provider_session_id,
            parent_provider_session_id,
            external_agent_id,
            is_subagent,
            started_at: claude_timestamp(value).unwrap_or(context.imported_at),
            cwd: value
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|cwd| !cwd.trim().is_empty())
                .map(str::to_owned),
            version: value
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_owned),
            git_branch: value
                .get("gitBranch")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }

    fn capture(
        &mut self,
        path: &Path,
        context: &ProviderAdapterContext,
        value: &Value,
        line_number: usize,
    ) -> ProviderNormalizationResult {
        let occurred_at = claude_timestamp(value).unwrap_or(context.imported_at);
        self.started_at = self.started_at.min(occurred_at);
        let event = claude_event(value, line_number, occurred_at);
        let raw_source_path = path.display().to_string();
        ProviderNormalizationResult {
            captures: vec![(
                line_number,
                ProviderCaptureEnvelope {
                    schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                    provider: CaptureProvider::Claude,
                    source: ProviderSourceEnvelope {
                        source_format: CLAUDE_PROJECTS_SOURCE_FORMAT.to_owned(),
                        machine_id: context.machine_id.clone(),
                        observed_at: context.imported_at,
                        raw_source_path: Some(raw_source_path.clone()),
                        source_root: context.source_root_display(),
                        trust: ProviderSourceTrust::ProviderNative,
                        fidelity: Fidelity::Imported,
                        cursor: Some(ProviderCursorRange {
                            before: None,
                            after: Some(ProviderCursorCheckpoint {
                                stream: provider_cursor_stream(
                                    CaptureProvider::Claude,
                                    CLAUDE_PROJECTS_SOURCE_FORMAT,
                                ),
                                cursor: format!("{}:line:{line_number}", path.display()),
                                observed_at: occurred_at,
                            }),
                        }),
                        idempotency_key: Some(format!(
                            "provider-source:claude:{CLAUDE_PROJECTS_SOURCE_FORMAT}:{}",
                            self.provider_session_id
                        )),
                        metadata: json!({
                            "adapter": CLAUDE_PROJECTS_SOURCE_FORMAT,
                            "native_session_id": self.native_session_id,
                            "source_path": raw_source_path,
                        }),
                    },
                    session: ProviderSessionEnvelope {
                        provider_session_id: self.provider_session_id.clone(),
                        parent_provider_session_id: self.parent_provider_session_id.clone(),
                        root_provider_session_id: self.parent_provider_session_id.clone(),
                        external_agent_id: self.external_agent_id.clone(),
                        agent_type: if self.is_subagent {
                            AgentType::Subagent
                        } else {
                            AgentType::Primary
                        },
                        role_hint: Some(
                            if self.is_subagent {
                                "subagent"
                            } else {
                                "primary"
                            }
                            .to_owned(),
                        ),
                        is_primary: !self.is_subagent,
                        status: SessionStatus::Imported,
                        started_at: self.started_at,
                        ended_at: None,
                        cwd: self.cwd.clone(),
                        fidelity: Fidelity::Imported,
                        idempotency_key: Some(format!(
                            "provider-session:claude:{}",
                            self.provider_session_id
                        )),
                        artifacts: Vec::new(),
                        metadata: json!({
                            "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                            "native_session_id": self.native_session_id,
                            "version": self.version,
                            "git_branch": self.git_branch,
                            "source_path": path.display().to_string(),
                            "limitations": [
                                "binary attachments are referenced by native payload metadata but not expanded",
                                "previews are capped before local indexing/export"
                            ],
                        }),
                    },
                    event,
                },
            )],
            ..ProviderNormalizationResult::default()
        }
    }
}

struct ClaudeCapturedBatchProjector {
    path: std::path::PathBuf,
    context: ProviderAdapterContext,
    session: Option<ClaudeSessionCheckpoint>,
    next_ordinal: u64,
    accepted_captures: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
}

impl ClaudeCapturedBatchProjector {
    fn fresh(path: &Path, context: ProviderAdapterContext) -> Self {
        Self {
            path: path.to_path_buf(),
            context,
            session: None,
            next_ordinal: 0,
            accepted_captures: 0,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejected_records: 0,
        }
    }

    fn resume(
        path: &Path,
        context: ProviderAdapterContext,
        cursor: &CertifiedProviderCursor,
    ) -> Result<Self> {
        let checkpoint: ClaudeParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        Ok(Self {
            path: path.to_path_buf(),
            context,
            session: checkpoint.session,
            next_ordinal: checkpoint.next_ordinal,
            accepted_captures: checkpoint.accepted_captures,
            accepted_events: checkpoint.accepted_events,
            accepted_file_touches: checkpoint.accepted_file_touches,
            rejected_records: checkpoint.rejected_records.max(cursor.rejected_records()),
        })
    }

    fn line_number(&mut self, ordinal: u64) -> Result<usize> {
        if ordinal < self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Claude captured record ordinal moved backwards",
            ));
        }
        self.next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Claude captured record ordinal overflowed",
        ))?;
        usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Claude captured record ordinal exceeds platform limits",
            ))
    }

    fn accept(
        &mut self,
        normalization: ProviderNormalizationResult,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let captures = u64::try_from(normalization.captures.len())
            .map_err(|_| CaptureError::SystemInvariant("Claude capture count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        let events = u64::try_from(
            normalization
                .captures
                .iter()
                .filter(|(_, capture)| capture.event.is_some())
                .count(),
        )
        .map_err(|_| CaptureError::SystemInvariant("Claude event count exceeds u64"))
        .map_err(ProviderProjectionFatal::new)?;
        let file_touches = u64::try_from(normalization.files_touched.len())
            .map_err(|_| CaptureError::SystemInvariant("Claude file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_captures = self
            .accepted_captures
            .checked_add(captures)
            .ok_or(CaptureError::SystemInvariant(
                "Claude capture count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_events = self
            .accepted_events
            .checked_add(events)
            .ok_or(CaptureError::SystemInvariant(
                "Claude event count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touches)
            .ok_or(CaptureError::SystemInvariant(
                "Claude file-touch count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        emit_projected_normalization_units(output, normalization)
    }

    fn reject_record(
        &mut self,
        output: &mut dyn ProviderProjectionOutput,
        line_number: usize,
        reason: String,
    ) -> ProviderProjectionResult<()> {
        self.rejected_records = self.rejected_records.checked_add(1).ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("Claude rejection count overflowed")
        })?;
        output.reject_record(line_number, reason);
        Ok(())
    }

    fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::from(self.accepted_captures != 0);
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("Claude replay event count exceeds platform limits")
        })?;
        let skipped_file_touches = usize::try_from(self.accepted_file_touches).map_err(|_| {
            CaptureError::SystemInvariant("Claude replay file-touch count exceeds platform limits")
        })?;
        let skipped = skipped_sessions
            .checked_add(skipped_events)
            .and_then(|value| value.checked_add(skipped_file_touches))
            .ok_or(CaptureError::SystemInvariant(
                "Claude replay summary count overflowed",
            ))?;
        let failed = usize::try_from(self.rejected_records).map_err(|_| {
            CaptureError::SystemInvariant("Claude replay rejection count exceeds platform limits")
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

impl CapturedBatchProjector for ClaudeCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != CLAUDE_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "Claude projector received an unexpected record kind",
            ));
        }
        let line_number = self
            .line_number(record.ordinal())
            .map_err(ProviderProjectionFatal::new)?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "Claude projector requires native JSONL bytes",
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
                    format!("malformed JSONL in {}: {error}", self.path.display()),
                );
            }
        };
        if self.session.is_none() {
            self.session = Some(ClaudeSessionCheckpoint::from_first_record(
                &self.path,
                &self.context,
                &value,
            ));
        }
        let result = crate::complete_content::jsonl::result_content_and_id(
            CaptureProvider::Claude,
            CLAUDE_PROJECTS_SOURCE_FORMAT,
            &value,
            line_number,
        );
        let mut normalization = self
            .session
            .as_mut()
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "Claude projector did not retain its discovered session",
                )
            })?
            .capture(&self.path, &self.context, &value, line_number);
        if let Some(event) = normalization
            .captures
            .first_mut()
            .and_then(|(_, capture)| capture.event.as_mut())
        {
            crate::complete_content::jsonl::attach_jsonl_complete_content_locator(
                event,
                CaptureProvider::Claude,
                CLAUDE_PROJECTS_SOURCE_FORMAT,
                &value,
                record,
                line_number,
            )
            .map_err(ProviderProjectionFatal::new)?;
        }
        if let (Some(event), Some((content, native_record_id))) = (
            normalization
                .captures
                .first_mut()
                .and_then(|(_, capture)| capture.event.as_mut()),
            result,
        ) {
            crate::complete_content::jsonl::attach_jsonl_result_content_locator(
                event,
                CaptureProvider::Claude,
                CLAUDE_PROJECTS_SOURCE_FORMAT,
                &content,
                &native_record_id,
                record,
            )
            .map_err(ProviderProjectionFatal::new)?;
        }
        let event = normalization
            .captures
            .first()
            .and_then(|(_, capture)| capture.event.clone());
        output.use_explicit_file_touches();
        self.accept(normalization, output)?;
        let Some(event) = event else {
            return Ok(());
        };
        let raw_source_path = self.path.display().to_string();
        let file_touch_outcome = visit_provider_file_touches_from_raw_value(
            ProviderFileTouchSourceContext::new(
                CaptureProvider::Claude,
                self.session
                    .as_ref()
                    .map(|session| session.provider_session_id.as_str())
                    .ok_or_else(|| {
                        ProviderProjectionFatal::system_invariant(
                            "Claude projector lost its discovered session",
                        )
                    })?,
                CLAUDE_PROJECTS_SOURCE_FORMAT,
                Some(raw_source_path.as_str()),
                Some(raw_source_path.as_str()),
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
            .map_err(|_| CaptureError::SystemInvariant("Claude file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touch_count)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant("Claude file-touch count overflowed")
            })?;
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if *position != initial_jsonl_position().map_err(claude_jsonl_batch_error)? {
            return Err(CaptureError::InvalidPayload(
                "Claude initial cursor candidate is not at the JSONL source start".to_owned(),
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&ClaudeParserCheckpoint {
                session: None,
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
                "Claude captured batch did not have a next ordinal",
            ))?;
        if self.next_ordinal > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "Claude projector advanced beyond the captured batch",
            ));
        }
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&ClaudeParserCheckpoint {
                    session: self.session.clone(),
                    next_ordinal,
                    accepted_captures: self.accepted_captures,
                    accepted_events: self.accepted_events,
                    accepted_file_touches: self.accepted_file_touches,
                    rejected_records: self.rejected_records,
                })?,
            )?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClaudeFrozenFileMetadata {
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl ClaudeFrozenFileMetadata {
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
            "claude-jsonl-metadata-v1:length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={}",
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

pub(crate) fn import_claude_projects_jsonl_tree_batched(
    path: &Path,
    store: &mut Store,
    options: ClaudeProjectsImportOptions,
) -> Result<ProviderImportSummary> {
    let source_root = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let mut merged = ProviderImportSummary::default();
    let source_count = visit_native_jsonl_files(path, CaptureProvider::Claude, &mut |file_path| {
        let mut file_options = options.clone();
        file_options.source_path = Some(source_root.clone());
        merged.merge(import_claude_projects_jsonl_file_batched(
            file_path,
            store,
            file_options,
        )?);
        Ok(())
    })?;
    if source_count == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::Claude),
        });
    }
    Ok(merged)
}

pub(crate) fn import_claude_projects_jsonl_file_batched(
    path: &Path,
    store: &mut Store,
    options: ClaudeProjectsImportOptions,
) -> Result<ProviderImportSummary> {
    let source_root = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let context = ProviderAdapterContext {
        machine_id: options.machine_id,
        source_path: Some(path.to_path_buf()),
        source_root: Some(source_root),
        imported_at: options.imported_at,
    };
    let import_options = NormalizedProviderImportOptions {
        history_record_id: options.history_record_id,
        persist_cursors: false,
        wrap_transaction: true,
        fast_event_inserts: true,
        capture_work_limit: options.capture_work_limit,
        inventory_observation_token: options.inventory_observation_token.clone(),
    };
    let frozen = ClaudeFrozenFileMetadata::read(path)?;
    let canonical_path = fs::canonicalize(path)?;
    let cursor_source_path = provider_path_identity(path)?;
    let canonical_path_identity = provider_path_identity(&canonical_path)?;
    let source = SourceObservation::new(
        CaptureProvider::Claude,
        CLAUDE_PROJECTS_SOURCE_FORMAT,
        format!("claude-jsonl-file:{canonical_path_identity}"),
        frozen.source_revision(),
        provider_source_cursor_stream_for_path(
            CaptureProvider::Claude,
            CLAUDE_PROJECTS_SOURCE_FORMAT,
            &cursor_source_path,
        ),
        CLAUDE_CAPTURE_REVISION,
        CLAUDE_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(claude_captured_batch_error)?;
    let source_item = canonical_path_identity.into_bytes();
    let record_kind =
        ProviderRecordKind::new(CLAUDE_RECORD_KIND).map_err(claude_captured_batch_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let had_expected_store_cursor = expected_store_cursor.is_some();
    let initial_position = initial_jsonl_position().map_err(claude_jsonl_batch_error)?;
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
                    let file = File::open(path)?;
                    if ClaudeFrozenFileMetadata::from_metadata(&file.metadata()?)? != frozen {
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
                        .map_err(claude_jsonl_batch_error)?;
                    let projector =
                        ClaudeCapturedBatchProjector::resume(path, context.clone(), &certified)?;
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
        .unwrap_or_else(|| ClaudeCapturedBatchProjector::fresh(path, context.clone()));
    if !frozen.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let file = File::open(path)?;
    if ClaudeFrozenFileMetadata::from_metadata(&file.metadata()?)? != frozen {
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
    .map_err(claude_jsonl_batch_error)?;
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
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
            let batch = producer.next_batch().map_err(claude_jsonl_batch_error)?;
            imported_any |= batch.is_some();
            Ok(batch)
        },
        || frozen.revalidate(path),
    )?;
    if !imported_any && had_expected_store_cursor {
        projector.replay_summary()
    } else {
        Ok(summary)
    }
}

fn claude_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
}

fn claude_jsonl_batch_error(error: JsonlBatchError) -> CaptureError {
    match error {
        JsonlBatchError::Io(error) => CaptureError::Io(error),
        JsonlBatchError::SourceChangedDuringRead { .. } => CaptureError::SourceChangedDuringCapture,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn claude_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(crate) fn claude_path_session_ids(
    path: &Path,
    native_session_id: &str,
) -> (String, Option<String>, Option<String>, bool) {
    let Some(parent) = path.parent() else {
        return (native_session_id.to_owned(), None, None, false);
    };
    if parent.file_name().and_then(|name| name.to_str()) == Some("subagents") {
        let parent_session_id = parent
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(native_session_id)
            .to_owned();
        let agent_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("subagent")
            .to_owned();
        return (
            format!("{parent_session_id}/subagents/{agent_id}"),
            Some(parent_session_id),
            Some(agent_id),
            true,
        );
    }
    (native_session_id.to_owned(), None, None, false)
}

pub(crate) fn claude_event(
    value: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
) -> Option<ProviderEventEnvelope> {
    let entry_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = value.get("message").unwrap_or(value);
    let message_role = message
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| value.get("role").and_then(Value::as_str));
    let null = Value::Null;
    let content = message.get("content").unwrap_or(&null);
    let event_type = claude_event_type(entry_type, message);
    let role = Some(provider_role(message_role));
    let text = provider_value_text(content).unwrap_or_else(|| {
        if event_type == EventType::Notice {
            format!("Claude event: {entry_type}")
        } else {
            String::new()
        }
    });
    let retained_text = provider_policy_event_text(event_type, &text, content);
    // Claude may put the native command result beside `message`, rather than in
    // the `tool_result` content block. Inspect both bounded native structures,
    // but only the shared explicit-outcome classifier may assert completion.
    let result_source = json!({
        "content": content,
        "tool_use_result": value.get("toolUseResult"),
    });
    let result_evidence = provider_result_identifier_evidence(event_type, &text, &result_source);
    let result_outcome = provider_result_outcome_evidence(event_type, &result_source);

    Some(ProviderEventEnvelope {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: value.get("uuid").and_then(Value::as_str).map(str::to_owned),
        cursor: value.get("uuid").and_then(Value::as_str).map(str::to_owned),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: value
            .get("uuid")
            .and_then(Value::as_str)
            .map(|uuid| format!("provider-event:claude:{uuid}")),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": entry_type,
            "uuid": value.get("uuid").and_then(Value::as_str),
            "parent_uuid": value.get("parentUuid").and_then(Value::as_str),
            "message_id": message.get("id").and_then(Value::as_str),
            "request_id": value.get("requestId").and_then(Value::as_str),
            "role": message_role,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "content_preview": provider_capped_json(&provider_policy_body(event_type, content), PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "claude_projects_jsonl",
            "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
            "line": line_number,
            "entry_type": entry_type,
            "model": message.get("model").and_then(Value::as_str),
            "usage": message.get("usage").cloned(),
            "stop_reason": message.get("stop_reason").and_then(Value::as_str),
            "is_sidechain": value.get("isSidechain").and_then(Value::as_bool),
            "tool_use_result": value.get("toolUseResult").map(|value| provider_policy_body(EventType::ToolOutput, value)),
        }),
    })
}

#[cfg(test)]
mod tests {
    use crate::test_support_paths::tempdir;

    use super::*;

    include!("claude_outcome_tests.rs");

    #[test]
    fn certified_rejection_replay_preserves_cumulative_failed_status() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("projects");
        fs::create_dir_all(&root).unwrap();
        let transcript = root.join("rejected-session.jsonl");
        fs::write(&transcript, b"{malformed-claude-record\n").unwrap();
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let options = ClaudeProjectsImportOptions {
            machine_id: "claude-rejection-replay-machine".to_owned(),
            source_path: Some(root),
            imported_at: "2026-07-18T12:00:00Z".parse().unwrap(),
            history_record_id: None,
            capture_work_limit: crate::CaptureWorkLimit::Drain,
            inventory_observation_token: None,
        };

        let first =
            import_claude_projects_jsonl_file_batched(&transcript, &mut store, options.clone())
                .unwrap();
        assert_eq!(first.failed, 1, "{:?}", first.failures);
        assert_eq!(first.failures.len(), 1);
        assert!(first.failures[0].error.contains("malformed JSONL"));

        let path_identity = provider_path_identity(&transcript).unwrap();
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Claude,
            CLAUDE_PROJECTS_SOURCE_FORMAT,
            &path_identity,
        );
        let mut stored = store
            .get_sync_cursor(None, &options.machine_id, &stream)
            .unwrap()
            .unwrap();
        let certified = CertifiedProviderCursor::decode(&stored.cursor).unwrap();
        assert_eq!(certified.rejected_records(), 1);
        stored.cursor = certified.with_rejected_records(2).encode().unwrap();
        store.upsert_sync_cursor(&stored).unwrap();

        let replay =
            import_claude_projects_jsonl_file_batched(&transcript, &mut store, options).unwrap();
        assert_eq!(replay.failed, 2);
        assert_eq!(replay.imported_sessions, 0);
        assert_eq!(replay.imported_events, 0);
        assert!(replay.failures.is_empty());
    }
}
