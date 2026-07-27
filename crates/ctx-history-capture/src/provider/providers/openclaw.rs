use std::{
    fs::{self, File, Metadata},
    io::{BufReader, Read, Seek, SeekFrom},
    mem::size_of,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use ctx_history_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::captured_batch::jsonl::{
    initial_jsonl_position, jsonl_position_offset, verify_jsonl_append_boundary, JsonlBatchError,
    JsonlBatchProducer,
};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativeLocator, NativePosition,
    ProviderRecordKind, SourceObservation,
};

use crate::common::io::{
    ensure_regular_provider_transcript_file, path_has_component, read_text_file_limited,
};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, emit_projected_normalization_units,
    provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
    CapturedBatchCursorFinish, CapturedBatchCursorMode, CapturedBatchProjector,
    CapturedSourceAdmission, CertifiedProviderCursor, ProviderProjectionFatal,
    ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_capped_json, provider_local_preview,
    provider_role, provider_timestamp_value, provider_value_text, NativeEventDraft,
    NativeSessionDraft,
};
use crate::provider::providers::native_jsonl::{
    native_jsonl_missing_reason, visit_native_jsonl_files,
};
use crate::{
    fnv1a64, CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext,
    ProviderImportSummary, ProviderNormalizationResult, Result, MAX_OPENCLAW_SESSION_INDEX_BYTES,
    MAX_PROVIDER_JSONL_LINE_BYTES, OPENCLAW_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
    PROVIDER_MAX_TEXT_CHARS,
};

const OPENCLAW_CAPTURE_REVISION: u32 = 2;
const OPENCLAW_POLICY_REVISION: u32 = 5;
const OPENCLAW_RECORD_KIND: &str = "openclaw-session-jsonl-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenClawSessionState {
    provider_session_id: String,
    agent_id: Option<String>,
    started_at: DateTime<Utc>,
    // This capped normalized path is the only non-ID provider string retained so resumed
    // captures reproduce the same typed session output; raw header/index values stay transient.
    cwd: Option<String>,
    index_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenClawParserCheckpoint {
    session: OpenClawSessionState,
    next_ordinal: u64,
    header_anchor: Option<OpenClawHeaderAnchor>,
    emitted_session: bool,
    accepted_events: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenClawHeaderAnchor {
    start: u64,
    end: u64,
    digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenClawFrozenFileMetadata {
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl OpenClawFrozenFileMetadata {
    fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        Self::from_metadata(&fs::symlink_metadata(path)?)
    }

    fn read_optional(path: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                Self::from_metadata(&metadata).map(Some)
            }
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
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

    fn revision_component(&self) -> String {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        format!(
            "length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={}",
            self.length,
            self.readonly,
            self.device
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            self.inode
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenClawSessionObservation {
    canonical_path: PathBuf,
    transcript: OpenClawFrozenFileMetadata,
    index_file: Option<OpenClawFrozenFileMetadata>,
    index: Value,
    index_revision: u64,
}

impl OpenClawSessionObservation {
    fn read(path: &Path) -> Result<Self> {
        let transcript = OpenClawFrozenFileMetadata::read(path)?;
        let canonical_path = fs::canonicalize(path)?;
        let index_path = path
            .parent()
            .map(|parent| parent.join("sessions.json"))
            .unwrap_or_else(|| PathBuf::from("sessions.json"));
        let index_file = OpenClawFrozenFileMetadata::read_optional(&index_path)?;
        let index = if index_file.is_some() {
            read_text_file_limited(
                &index_path,
                MAX_OPENCLAW_SESSION_INDEX_BYTES,
                "OpenClaw sessions.json",
            )
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|value| openclaw_session_index_for_file(path, &value))
            .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let index = provider_capped_json(&index, PROVIDER_MAX_PREVIEW_CHARS);
        let index_revision = openclaw_index_revision(&index)?;
        Ok(Self {
            canonical_path,
            transcript,
            index_file,
            index,
            index_revision,
        })
    }

    fn source_revision(&self) -> String {
        let index_file = self
            .index_file
            .as_ref()
            .map(OpenClawFrozenFileMetadata::revision_component)
            .unwrap_or_else(|| "absent".to_owned());
        format!(
            "openclaw-jsonl-metadata-v1:transcript={};index={index_file};index-entry={:016x}",
            self.transcript.revision_component(),
            self.index_revision,
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

pub(crate) fn openclaw_agent_id(path: &Path) -> Option<String> {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    components.windows(2).find_map(|window| {
        (window[0] == "agents" && !window[1].is_empty()).then(|| window[1].clone())
    })
}

fn openclaw_session_index_for_file(path: &Path, value: &Value) -> Value {
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("openclaw-session");
    let agent_id = openclaw_agent_id(path);
    let qualified_id = agent_id
        .as_deref()
        .map(|agent_id| format!("{agent_id}/{fallback_id}"));
    openclaw_find_session_index(value, fallback_id, qualified_id.as_deref())
        .cloned()
        .unwrap_or(Value::Null)
}

fn openclaw_find_session_index<'a>(
    value: &'a Value,
    fallback_id: &str,
    qualified_id: Option<&str>,
) -> Option<&'a Value> {
    match value {
        Value::Array(items) => items
            .iter()
            .find(|item| openclaw_index_value_matches(item, fallback_id, qualified_id)),
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("sessions") {
                return items
                    .iter()
                    .find(|item| openclaw_index_value_matches(item, fallback_id, qualified_id));
            }
            qualified_id
                .and_then(|qualified_id| map.get(qualified_id))
                .or_else(|| map.get(fallback_id))
                .or_else(|| {
                    map.values()
                        .find(|item| openclaw_index_value_matches(item, fallback_id, qualified_id))
                })
        }
        _ => None,
    }
}

fn openclaw_index_value_matches(
    value: &Value,
    fallback_id: &str,
    qualified_id: Option<&str>,
) -> bool {
    value
        .get("sessionId")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .is_some_and(|session_id| session_id == fallback_id || qualified_id == Some(session_id))
}

fn openclaw_index_revision(value: &Value) -> Result<u64> {
    Ok(fnv1a64(&serde_json::to_vec(value)?))
}

fn capped_openclaw_text(value: &str) -> String {
    provider_local_preview(value, PROVIDER_MAX_TEXT_CHARS).0
}

fn openclaw_header_digest(payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-openclaw-header-anchor-sha256-v1\0");
    hasher.update((payload.len() as u64).to_be_bytes());
    hasher.update(payload);
    hasher.finalize().into()
}

fn openclaw_header_anchor(locator: &NativeLocator, payload: &[u8]) -> Result<OpenClawHeaderAnchor> {
    const JSONL_LOCATOR_KIND: &str = "jsonl-source-item-byte-range-v1";
    let value = locator.value();
    let minimum =
        size_of::<u32>()
            .checked_add(2 * size_of::<u64>())
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw JSONL locator length overflowed",
            ))?;
    if locator.kind() != JSONL_LOCATOR_KIND || value.len() < minimum {
        return Err(CaptureError::SystemInvariant(
            "OpenClaw projector received an invalid JSONL locator",
        ));
    }
    let source_item_len = u32::from_be_bytes(
        value[..size_of::<u32>()]
            .try_into()
            .map_err(|_| CaptureError::SystemInvariant("OpenClaw JSONL locator is truncated"))?,
    );
    let range_start = size_of::<u32>()
        .checked_add(usize::try_from(source_item_len).map_err(|_| {
            CaptureError::SystemInvariant("OpenClaw JSONL source-item length is invalid")
        })?)
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw JSONL locator length overflowed",
        ))?;
    let range_end =
        range_start
            .checked_add(2 * size_of::<u64>())
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw JSONL locator length overflowed",
            ))?;
    if range_end != value.len() {
        return Err(CaptureError::SystemInvariant(
            "OpenClaw JSONL locator has an invalid length",
        ));
    }
    let start = u64::from_be_bytes(
        value[range_start..range_start + size_of::<u64>()]
            .try_into()
            .map_err(|_| CaptureError::SystemInvariant("OpenClaw JSONL start is truncated"))?,
    );
    let end = u64::from_be_bytes(
        value[range_start + size_of::<u64>()..range_end]
            .try_into()
            .map_err(|_| CaptureError::SystemInvariant("OpenClaw JSONL end is truncated"))?,
    );
    if start >= end {
        return Err(CaptureError::SystemInvariant(
            "OpenClaw JSONL header range is invalid",
        ));
    }
    Ok(OpenClawHeaderAnchor {
        start,
        end,
        digest: openclaw_header_digest(payload),
    })
}

fn openclaw_bootstrap_header(
    path: &Path,
    anchor: Option<OpenClawHeaderAnchor>,
    observation: &OpenClawSessionObservation,
) -> Result<Option<Value>> {
    let Some(anchor) = anchor else {
        return Ok(Some(Value::Null));
    };
    let length = anchor
        .end
        .checked_sub(anchor.start)
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw checkpoint header range is invalid",
        ))?;
    let maximum = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(2);
    if length > maximum || anchor.end > observation.transcript.length {
        return Err(CaptureError::InvalidPayload(
            "OpenClaw checkpoint header range exceeds the frozen source".to_owned(),
        ));
    }
    let length = usize::try_from(length).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenClaw checkpoint header range exceeds platform limits".to_owned(),
        )
    })?;
    let mut file = File::open(path)?;
    if OpenClawFrozenFileMetadata::from_metadata(&file.metadata()?)? != observation.transcript {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(anchor.start))?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)?;
    if OpenClawFrozenFileMetadata::from_metadata(&file.metadata()?)? != observation.transcript {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if openclaw_header_digest(&bytes) != anchor.digest {
        return Ok(None);
    }
    let header: Value = serde_json::from_slice(&bytes)?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return Err(CaptureError::InvalidPayload(
            "OpenClaw checkpoint header does not reference a session record".to_owned(),
        ));
    }
    Ok(Some(provider_capped_json(
        &header,
        PROVIDER_MAX_PREVIEW_CHARS,
    )))
}

struct OpenClawCapturedBatchProjector {
    context: ProviderAdapterContext,
    session: OpenClawSessionState,
    // Re-read from the current frozen observation on every invocation, never serialized.
    index: Value,
    // Re-read from the compact byte range on resume, never serialized.
    header_raw: Value,
    next_ordinal: u64,
    header_anchor: Option<OpenClawHeaderAnchor>,
    emitted_session: bool,
    accepted_events: u64,
    rejected_records: u64,
    complete_content_binding: crate::complete_content::jsonl::ExactJsonlSourceBinding,
}

impl OpenClawCapturedBatchProjector {
    fn fresh(
        path: &Path,
        context: ProviderAdapterContext,
        observation: &OpenClawSessionObservation,
        complete_content_binding: crate::complete_content::jsonl::ExactJsonlSourceBinding,
    ) -> Self {
        let fallback_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("openclaw-session");
        let agent_id = openclaw_agent_id(path).map(|agent_id| capped_openclaw_text(&agent_id));
        let provider_session_id = agent_id
            .as_deref()
            .map(|agent_id| format!("{agent_id}/{fallback_id}"))
            .unwrap_or_else(|| fallback_id.to_owned());
        let imported_at = context.imported_at;
        Self {
            context,
            session: OpenClawSessionState {
                provider_session_id: capped_openclaw_text(&provider_session_id),
                agent_id,
                started_at: imported_at,
                cwd: None,
                index_revision: observation.index_revision,
            },
            index: observation.index.clone(),
            header_raw: Value::Null,
            next_ordinal: 0,
            header_anchor: None,
            emitted_session: false,
            accepted_events: 0,
            rejected_records: 0,
            complete_content_binding,
        }
    }

    fn resume(
        context: ProviderAdapterContext,
        observation: &OpenClawSessionObservation,
        cursor: &CertifiedProviderCursor,
        complete_content_binding: crate::complete_content::jsonl::ExactJsonlSourceBinding,
    ) -> Result<Option<Self>> {
        let checkpoint: OpenClawParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        let path = context
            .source_path
            .as_deref()
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw resume requires its session source path",
            ))?;
        let Some(header_raw) =
            openclaw_bootstrap_header(path, checkpoint.header_anchor, observation)?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            context,
            session: checkpoint.session,
            index: observation.index.clone(),
            header_raw,
            next_ordinal: checkpoint.next_ordinal,
            header_anchor: checkpoint.header_anchor,
            emitted_session: checkpoint.emitted_session,
            accepted_events: checkpoint.accepted_events,
            rejected_records: cursor.rejected_records(),
            complete_content_binding,
        }))
    }

    fn advance_to(&mut self, ordinal: u64) -> Result<usize> {
        if ordinal < self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "OpenClaw captured record ordinal moved backwards",
            ));
        }
        self.next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "OpenClaw captured record ordinal overflowed",
        ))?;
        usize::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw captured record ordinal exceeds platform limits",
            ))
    }

    fn update_header(&mut self, value: &Value) {
        if let Some(id) = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        {
            let id = capped_openclaw_text(id);
            self.session.provider_session_id = self
                .session
                .agent_id
                .as_deref()
                .map(|agent_id| format!("{agent_id}/{id}"))
                .unwrap_or(id);
        }
        self.session.started_at =
            provider_timestamp_value(value.get("timestamp"), self.context.imported_at);
        self.session.cwd = value
            .get("cwd")
            .and_then(Value::as_str)
            .map(capped_openclaw_text);
        self.header_raw = provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS);
    }

    fn emit_capture(
        &mut self,
        line_number: usize,
        event: Option<ProviderEventEnvelope>,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let path = self.context.source_path.as_deref().ok_or_else(|| {
            ProviderProjectionFatal::system_invariant(
                "OpenClaw captured import requires its actual session source path",
            )
        })?;
        emit_projected_normalization_units(
            output,
            ProviderNormalizationResult {
                captures: vec![(
                    line_number,
                    openclaw_capture(
                        OpenClawCaptureDraft {
                            provider_session_id: &self.session.provider_session_id,
                            agent_id: self.session.agent_id.as_deref(),
                            started_at: self.session.started_at,
                            ended_at: None,
                            cwd: self.session.cwd.clone(),
                            path,
                            index: self.index.clone(),
                            header_raw: self.header_raw.clone(),
                            event,
                        },
                        &self.context,
                    ),
                )],
                ..ProviderNormalizationResult::default()
            },
        )?;
        self.emitted_session = true;
        Ok(())
    }

    fn reject_record(
        &mut self,
        line_number: usize,
        reason: String,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        self.rejected_records = self.rejected_records.checked_add(1).ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("OpenClaw rejection count overflowed")
        })?;
        output.reject_record(line_number, reason);
        Ok(())
    }

    fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::from(self.emitted_session);
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("OpenClaw replay event count exceeds platform limits")
        })?;
        let skipped =
            skipped_sessions
                .checked_add(skipped_events)
                .ok_or(CaptureError::SystemInvariant(
                    "OpenClaw replay summary count overflowed",
                ))?;
        let failed = usize::try_from(self.rejected_records).map_err(|_| {
            CaptureError::SystemInvariant("OpenClaw replay rejection count exceeds platform limits")
        })?;
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

impl CapturedBatchProjector for OpenClawCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str() != OPENCLAW_RECORD_KIND {
            return Err(ProviderProjectionFatal::system_invariant(
                "OpenClaw projector received an unexpected record kind",
            ));
        }
        let line_number = self
            .advance_to(record.ordinal())
            .map_err(ProviderProjectionFatal::new)?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "OpenClaw projector requires native JSONL bytes",
            ));
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                return self.reject_record(
                    line_number,
                    format!("malformed OpenClaw JSONL: {error}"),
                    output,
                );
            }
        };
        let row_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message");
        if row_type == "session" {
            self.update_header(&value);
            self.header_anchor = Some(
                openclaw_header_anchor(record.locator(), bytes)
                    .map_err(ProviderProjectionFatal::new)?,
            );
            return self.emit_capture(line_number, None, output);
        }
        if !self.emitted_session {
            self.emit_capture(line_number, None, output)?;
        }
        let occurred_at = provider_timestamp_value(value.get("timestamp"), self.session.started_at);
        let mut event = openclaw_event(
            &self.session.provider_session_id,
            record.ordinal(),
            line_number,
            &value,
            occurred_at,
        );
        crate::complete_content::jsonl::attach_exact_jsonl_complete_content_locator(
            &mut event,
            CaptureProvider::OpenClaw,
            OPENCLAW_SOURCE_FORMAT,
            &value,
            record,
            line_number,
            &self.complete_content_binding,
        )
        .map_err(ProviderProjectionFatal::new)?;
        self.emit_capture(line_number, Some(event), output)?;
        self.accepted_events = self
            .accepted_events
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw projected event count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if *position != initial_jsonl_position().map_err(openclaw_jsonl_batch_error)? {
            return Err(CaptureError::InvalidPayload(
                "OpenClaw initial cursor candidate is not at the JSONL source start".to_owned(),
            ));
        }
        if self.next_ordinal != 0
            || self.header_anchor.is_some()
            || self.emitted_session
            || self.accepted_events != 0
            || self.rejected_records != 0
            || self.header_raw != Value::Null
        {
            return Err(CaptureError::SystemInvariant(
                "OpenClaw initial cursor candidate requires fresh projector state",
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&OpenClawParserCheckpoint {
                session: self.session.clone(),
                next_ordinal: 0,
                header_anchor: None,
                emitted_session: false,
                accepted_events: 0,
            })?,
        )
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let next_ordinal = batch
            .records()
            .last()
            .and_then(|record| record.ordinal().checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw captured batch did not have a next ordinal",
            ))?;
        if self.next_ordinal > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "OpenClaw projector advanced beyond the captured batch",
            ));
        }
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&OpenClawParserCheckpoint {
                    session: self.session.clone(),
                    next_ordinal,
                    header_anchor: self.header_anchor,
                    emitted_session: self.emitted_session,
                    accepted_events: self.accepted_events,
                })?,
            )?,
        ))
    }
}

pub(crate) fn import_openclaw_session_jsonl_file_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    context.source_path = Some(path.to_path_buf());
    context.source_root = Some(source_root);
    let observation = OpenClawSessionObservation::read(path)?;
    let cursor_source_path = provider_path_identity(path)?;
    let canonical_path_identity = provider_path_identity(&observation.canonical_path)?;
    let source = SourceObservation::new(
        CaptureProvider::OpenClaw,
        OPENCLAW_SOURCE_FORMAT,
        format!("openclaw-session-jsonl:{canonical_path_identity}"),
        observation.source_revision(),
        provider_source_cursor_stream_for_path(
            CaptureProvider::OpenClaw,
            OPENCLAW_SOURCE_FORMAT,
            &cursor_source_path,
        ),
        OPENCLAW_CAPTURE_REVISION,
        OPENCLAW_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(openclaw_captured_batch_error)?;
    let complete_content_binding = crate::complete_content::jsonl::ExactJsonlSourceBinding::new(
        source.source_revision(),
        &canonical_path_identity,
    );
    let source_item = canonical_path_identity.into_bytes();
    let record_kind =
        ProviderRecordKind::new(OPENCLAW_RECORD_KIND).map_err(openclaw_captured_batch_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let had_expected_store_cursor = expected_store_cursor.is_some();
    let initial_position = initial_jsonl_position().map_err(openclaw_jsonl_batch_error)?;
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut start_offset = 0_u64;
    let mut start_ordinal = 0_u64;
    let mut projector = OpenClawCapturedBatchProjector::fresh(
        path,
        context.clone(),
        &observation,
        complete_content_binding.clone(),
    );

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let checkpoint: OpenClawParserCheckpoint =
                    certified.parser_checkpoint().deserialize()?;
                let auxiliary_unchanged =
                    checkpoint.session.index_revision == observation.index_revision;
                let source_revision_unchanged =
                    certified.source_revision() == source.source_revision();
                let can_resume = if source_revision_unchanged {
                    true
                } else if auxiliary_unchanged {
                    let file = File::open(path)?;
                    if OpenClawFrozenFileMetadata::from_metadata(&file.metadata()?)?
                        != observation.transcript
                    {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    let mut reader = BufReader::new(file);
                    match verify_jsonl_append_boundary(
                        &mut reader,
                        certified.native_position(),
                        &source,
                        observation.transcript.length,
                    ) {
                        Ok(verified) => {
                            cursor_mode = CapturedBatchCursorMode::ResumeAppend(verified);
                            true
                        }
                        Err(JsonlBatchError::Io(error)) => return Err(CaptureError::Io(error)),
                        Err(_) => false,
                    }
                } else {
                    false
                };
                if can_resume {
                    let resumed = OpenClawCapturedBatchProjector::resume(
                        context.clone(),
                        &observation,
                        &certified,
                        complete_content_binding.clone(),
                    )?;
                    if let Some(resumed) = resumed {
                        start_offset = jsonl_position_offset(certified.native_position())
                            .map_err(openclaw_jsonl_batch_error)?;
                        start_ordinal = resumed.next_ordinal;
                        projector = resumed;
                    } else if source_revision_unchanged {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    } else {
                        cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                    }
                } else {
                    cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                }
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    if !observation.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let file = File::open(path)?;
    if OpenClawFrozenFileMetadata::from_metadata(&file.metadata()?)? != observation.transcript {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut producer = JsonlBatchProducer::new(
        BufReader::new(file),
        source.clone(),
        source_item,
        record_kind,
        observation.transcript.length,
        start_offset,
        start_ordinal,
        false,
    )
    .map_err(openclaw_jsonl_batch_error)?;
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
            let batch = producer.next_batch().map_err(openclaw_jsonl_batch_error)?;
            imported_any |= batch.is_some();
            Ok(batch)
        },
        || observation.revalidate(path),
    )?;
    if !imported_any && had_expected_store_cursor {
        projector.replay_summary()
    } else {
        Ok(summary)
    }
}

pub(crate) fn openclaw_complete_content_record(
    value: &Value,
    line_number: usize,
) -> Option<(String, String)> {
    let row_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    let message = value.get("message").unwrap_or(value);
    let role = message
        .get("role")
        .or_else(|| value.get("role"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)));
    let event_type = match row_type {
        "message" if role != Some(EventRole::Tool) => EventType::Message,
        "message" => EventType::ToolOutput,
        _ => EventType::Notice,
    };
    (event_type == EventType::Message).then(|| {
        let native_record_id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("line-{line_number}"));
        let text = message
            .get("content")
            .or_else(|| message.get("text"))
            .or_else(|| message.get("output"))
            .and_then(provider_value_text)
            .unwrap_or_default();
        (text, native_record_id)
    })
}

pub(crate) fn openclaw_complete_content_source(path: &Path) -> Result<(String, String)> {
    let observation = OpenClawSessionObservation::read(path)?;
    Ok((
        observation.source_revision(),
        provider_path_identity(&observation.canonical_path)?,
    ))
}

pub(crate) fn import_openclaw_session_jsonl_tree_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let mut merged = ProviderImportSummary::default();
    let restrict_to_session_directories = path.is_dir();
    let mut source_count = 0_usize;
    visit_native_jsonl_files(path, CaptureProvider::OpenClaw, &mut |file_path| {
        if restrict_to_session_directories && !path_has_component(file_path, "sessions") {
            return Ok(());
        }
        source_count = source_count.saturating_add(1);
        let mut file_context = context.clone();
        file_context.source_path = Some(file_path.to_path_buf());
        file_context.source_root = Some(source_root.clone());
        merged.merge(import_openclaw_session_jsonl_file_batched(
            file_path,
            store,
            file_context,
            import_options.clone(),
        )?);
        Ok(())
    })?;
    if source_count == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::OpenClaw),
        });
    }
    Ok(merged)
}

fn openclaw_jsonl_batch_error(error: JsonlBatchError) -> CaptureError {
    match error {
        JsonlBatchError::Io(error) => CaptureError::Io(error),
        JsonlBatchError::SourceChangedDuringRead { .. } => CaptureError::SourceChangedDuringCapture,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn openclaw_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

pub(crate) struct OpenClawCaptureDraft<'a> {
    pub(crate) provider_session_id: &'a str,
    pub(crate) agent_id: Option<&'a str>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) path: &'a Path,
    pub(crate) index: Value,
    pub(crate) header_raw: Value,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn openclaw_capture(
    draft: OpenClawCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let OpenClawCaptureDraft {
        provider_session_id,
        agent_id,
        started_at,
        ended_at,
        cwd,
        path,
        index,
        header_raw,
        event,
    } = draft;
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::OpenClaw,
            source_format: OPENCLAW_SOURCE_FORMAT,
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: index
                .get("parentSessionId")
                .or_else(|| index.get("parent_session_id"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            root_provider_session_id: None,
            external_agent_id: agent_id.map(str::to_owned),
            agent_type: AgentType::Primary,
            role_hint: Some("personal-agent".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd,
            fidelity: Fidelity::Partial,
            raw_source_path: path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": OPENCLAW_SOURCE_FORMAT,
                "index": provider_capped_json(&index, PROVIDER_MAX_PREVIEW_CHARS),
                "header": provider_capped_json(&header_raw, PROVIDER_MAX_PREVIEW_CHARS),
                "support_level": "beta",
            }),
            session_metadata: json!({
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "agent_id": agent_id,
                "session_index": provider_capped_json(&index, PROVIDER_MAX_PREVIEW_CHARS),
                "fidelity_gap": "OpenClaw session JSONL is current native storage, but upstream keeps a storage-neutral accessor for future schema changes",
            }),
        },
        context,
        event,
    )
}

pub(crate) fn openclaw_event(
    provider_session_id: &str,
    event_index: u64,
    line_number: usize,
    row: &Value,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let row_type = row.get("type").and_then(Value::as_str).unwrap_or("message");
    let message = row.get("message").unwrap_or(row);
    let role = message
        .get("role")
        .or_else(|| row.get("role"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)));
    let event_type = match row_type {
        "message" => match role {
            Some(EventRole::Tool) => EventType::ToolOutput,
            _ => EventType::Message,
        },
        "leaf" | "compaction" | "custom" => EventType::Notice,
        _ => EventType::Notice,
    };
    let text = message
        .get("content")
        .or_else(|| message.get("text"))
        .or_else(|| message.get("output"))
        .and_then(provider_value_text)
        .unwrap_or_default();
    native_event(NativeEventDraft {
        provider: CaptureProvider::OpenClaw,
        source_format: OPENCLAW_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index: event_index,
        provider_event_hash: row.get("id").and_then(Value::as_str).map(str::to_owned),
        cursor: format!("line:{line_number}"),
        event_type,
        role,
        occurred_at,
        text,
        body: row.clone(),
        metadata: json!({
            "source": "openclaw_jsonl",
            "source_format": OPENCLAW_SOURCE_FORMAT,
            "row_type": row_type,
            "message_id": row.get("id").and_then(Value::as_str),
            "parent_id": row.get("parentId").or_else(|| row.get("parent_id")).cloned(),
        }),
    })
}

#[cfg(test)]
#[path = "openclaw/tests.rs"]
mod captured_batch_tests;
