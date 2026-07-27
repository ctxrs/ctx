use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, ContentRef, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, SessionStatus, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::captured_batch::jsonl::{initial_jsonl_position, JsonlBatchError};
use crate::captured_batch::{
    CapturedBatch, CapturedRecord, CapturedRecordPayload, NativePosition, SourceObservation,
};
use crate::provider::file_touches::{
    visit_provider_file_touches_from_raw_value, ProviderFileTouchSourceContext,
    PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
};
use crate::provider::importer::{
    emit_projected_normalization_units, provider_cursor_stream, BoundedParserCheckpoint,
    CapturedBatchCursorFinish, CapturedBatchProjector, CertifiedProviderCursor,
    ProviderProjectionFatal, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportSummary, ProviderNormalizationResult,
    Result,
};

use super::dialect::{native_jsonl_record_kind, native_jsonl_record_starts_session};
use super::normalization::{
    antigravity_session_id_from_path, native_jsonl_event_with_result_content_ref,
    native_jsonl_header_cwd, native_jsonl_header_session_id, native_jsonl_header_start_time,
    native_jsonl_normalized_header_metadata, native_jsonl_path_session,
    native_jsonl_session_metadata_from_normalized_header, native_jsonl_session_status,
    native_jsonl_timestamp, windsurf_session_id_from_path,
};

pub(super) const NATIVE_JSONL_LOCATOR_KIND: &str = "jsonl-source-item-byte-range-v1";
const NATIVE_JSONL_HEADER_ANCHOR_HASH_DOMAIN: &[u8] = b"ctx-native-jsonl-header-anchor-sha256-v1\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeJsonlParserCheckpoint {
    pub(super) session: Option<NativeJsonlSessionCheckpoint>,
    pub(super) next_ordinal: u64,
    pub(super) accepted_captures: u64,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
    #[serde(default)]
    pub(super) rejected_records: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeJsonlHeaderAnchor {
    pub(super) ordinal: u64,
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) payload_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeJsonlSessionCheckpoint {
    pub(super) native_session_id: String,
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) external_agent_id: Option<String>,
    pub(super) agent_type: AgentType,
    pub(super) status: SessionStatus,
    pub(super) started_at: DateTime<Utc>,
    pub(super) cwd: Option<String>,
    pub(super) header_anchor: NativeJsonlHeaderAnchor,
    #[serde(skip)]
    pub(super) normalized_header_metadata: Value,
}

impl NativeJsonlSessionCheckpoint {
    pub(super) fn from_header(
        provider: CaptureProvider,
        path: &Path,
        context: &ProviderAdapterContext,
        header: &Value,
        header_anchor: NativeJsonlHeaderAnchor,
    ) -> Self {
        let native_session_id = match provider {
            CaptureProvider::Antigravity => antigravity_session_id_from_path(path)
                .unwrap_or_else(|| "unknown-session".to_owned()),
            CaptureProvider::Windsurf => {
                windsurf_session_id_from_path(path).unwrap_or_else(|| "unknown-session".to_owned())
            }
            _ => native_jsonl_header_session_id(provider, header)
                .unwrap_or_else(|| "unknown-session".to_owned()),
        };
        let (provider_session_id, parent_provider_session_id, external_agent_id, agent_type) =
            native_jsonl_path_session(provider, path, header, &native_session_id);
        let started_at = native_jsonl_timestamp(header)
            .or_else(|| native_jsonl_header_start_time(provider, header))
            .unwrap_or(context.imported_at);
        Self {
            native_session_id,
            provider_session_id,
            parent_provider_session_id,
            external_agent_id,
            agent_type,
            status: native_jsonl_session_status(provider, header),
            started_at,
            cwd: native_jsonl_header_cwd(provider, header),
            header_anchor,
            normalized_header_metadata: native_jsonl_normalized_header_metadata(header),
        }
    }

    pub(super) fn capture(
        &self,
        provider: CaptureProvider,
        source_format: &str,
        path: &Path,
        context: &ProviderAdapterContext,
        value: &Value,
        line_number: usize,
    ) -> (ProviderNormalizationResult, Option<ContentRef>) {
        let occurred_at = native_jsonl_timestamp(value).unwrap_or(self.started_at);
        let (event, result_content_ref) = native_jsonl_event_with_result_content_ref(
            provider,
            source_format,
            value,
            line_number,
            occurred_at,
        )
        .map_or((None, None), |(event, content_ref)| {
            (Some(event), content_ref)
        });
        let raw_source_path = path.display().to_string();
        let source_root = context
            .source_root_display()
            .or_else(|| Some(raw_source_path.clone()));
        let is_subagent =
            self.parent_provider_session_id.is_some() || self.agent_type == AgentType::Subagent;
        let capture = ProviderCaptureEnvelope {
            schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
            provider,
            source: ProviderSourceEnvelope {
                source_format: source_format.to_owned(),
                machine_id: context.machine_id.clone(),
                observed_at: context.imported_at,
                raw_source_path: Some(raw_source_path.clone()),
                source_root,
                trust: ProviderSourceTrust::ProviderNative,
                fidelity: Fidelity::Imported,
                cursor: Some(ProviderCursorRange {
                    before: None,
                    after: Some(ProviderCursorCheckpoint {
                        stream: provider_cursor_stream(provider, source_format),
                        cursor: format!("{}:line:{line_number}", path.display()),
                        observed_at: occurred_at,
                    }),
                }),
                idempotency_key: Some(format!(
                    "provider-source:{}:{source_format}:{}",
                    provider.as_str(),
                    self.provider_session_id
                )),
                metadata: json!({
                    "adapter": source_format,
                    "native_session_id": self.native_session_id,
                    "source_path": raw_source_path,
                }),
            },
            session: ProviderSessionEnvelope {
                provider_session_id: self.provider_session_id.clone(),
                parent_provider_session_id: self.parent_provider_session_id.clone(),
                root_provider_session_id: self.parent_provider_session_id.clone(),
                external_agent_id: self.external_agent_id.clone(),
                agent_type: self.agent_type,
                role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
                is_primary: !is_subagent,
                status: self.status,
                started_at: self.started_at,
                ended_at: None,
                cwd: self.cwd.clone(),
                fidelity: Fidelity::Imported,
                idempotency_key: Some(format!(
                    "provider-session:{}:{}",
                    provider.as_str(),
                    self.provider_session_id
                )),
                artifacts: Vec::new(),
                metadata: native_jsonl_session_metadata_from_normalized_header(
                    provider,
                    source_format,
                    &self.normalized_header_metadata,
                    path,
                ),
            },
            event,
        };
        (
            ProviderNormalizationResult {
                captures: vec![(line_number, capture)],
                ..ProviderNormalizationResult::default()
            },
            result_content_ref,
        )
    }
}

pub(super) struct NativeJsonlCapturedBatchProjector {
    provider: CaptureProvider,
    source_format: String,
    path: PathBuf,
    context: ProviderAdapterContext,
    session: Option<NativeJsonlSessionCheckpoint>,
    pub(super) next_ordinal: u64,
    accepted_captures: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    pending_resume_session_bootstrap: bool,
}

impl NativeJsonlCapturedBatchProjector {
    pub(super) fn fresh(
        provider: CaptureProvider,
        source_format: &str,
        path: &Path,
        context: ProviderAdapterContext,
    ) -> Self {
        Self {
            provider,
            source_format: source_format.to_owned(),
            path: path.to_path_buf(),
            context,
            session: None,
            next_ordinal: 0,
            accepted_captures: 0,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejected_records: 0,
            pending_resume_session_bootstrap: false,
        }
    }

    pub(super) fn resume(
        provider: CaptureProvider,
        source_format: &str,
        path: &Path,
        context: ProviderAdapterContext,
        cursor: &CertifiedProviderCursor,
        normalized_header_metadata: Option<Value>,
    ) -> Result<Self> {
        let checkpoint: NativeJsonlParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
        let session = match (checkpoint.session, normalized_header_metadata) {
            (Some(mut session), Some(normalized_header_metadata)) => {
                session.normalized_header_metadata = normalized_header_metadata;
                Some(session)
            }
            (Some(_), None) => {
                return Err(CaptureError::SystemInvariant(
                    "native JSONL resume requires rehydrated session header metadata",
                ));
            }
            (None, None) => None,
            (None, Some(_)) => {
                return Err(CaptureError::SystemInvariant(
                    "native JSONL resume rehydrated a header without session state",
                ));
            }
        };
        let pending_resume_session_bootstrap = session.is_some();
        Ok(Self {
            provider,
            source_format: source_format.to_owned(),
            path: path.to_path_buf(),
            context,
            session,
            next_ordinal: checkpoint.next_ordinal,
            accepted_captures: checkpoint.accepted_captures,
            accepted_events: checkpoint.accepted_events,
            accepted_file_touches: checkpoint.accepted_file_touches,
            rejected_records: checkpoint.rejected_records.max(cursor.rejected_records()),
            pending_resume_session_bootstrap,
        })
    }

    fn line_number(&mut self, ordinal: u64) -> Result<usize> {
        if ordinal < self.next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "native JSONL record ordinal moved backwards",
            ));
        }
        self.next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "native JSONL record ordinal overflowed",
        ))?;
        usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "native JSONL record ordinal exceeds platform limits",
            ))
    }

    fn accept(
        &mut self,
        normalization: ProviderNormalizationResult,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let captures = u64::try_from(normalization.captures.len())
            .map_err(|_| CaptureError::SystemInvariant("native JSONL capture count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        let events = u64::try_from(
            normalization
                .captures
                .iter()
                .filter(|(_, capture)| capture.event.is_some())
                .count(),
        )
        .map_err(|_| CaptureError::SystemInvariant("native JSONL event count exceeds u64"))
        .map_err(ProviderProjectionFatal::new)?;
        let file_touches = u64::try_from(normalization.files_touched.len())
            .map_err(|_| CaptureError::SystemInvariant("native JSONL file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_captures = self
            .accepted_captures
            .checked_add(captures)
            .ok_or(CaptureError::SystemInvariant(
                "native JSONL capture count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_events = self
            .accepted_events
            .checked_add(events)
            .ok_or(CaptureError::SystemInvariant(
                "native JSONL event count overflowed",
            ))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touches)
            .ok_or(CaptureError::SystemInvariant(
                "native JSONL file-touch count overflowed",
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
            ProviderProjectionFatal::system_invariant("native JSONL rejection count overflowed")
        })?;
        output.reject_record(line_number, reason);
        Ok(())
    }

    pub(super) fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let skipped_sessions = usize::from(self.accepted_captures != 0);
        let skipped_events = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("native JSONL replay event count exceeds platform limits")
        })?;
        let skipped_file_touches = usize::try_from(self.accepted_file_touches).map_err(|_| {
            CaptureError::SystemInvariant(
                "native JSONL replay file-touch count exceeds platform limits",
            )
        })?;
        let skipped = skipped_sessions
            .checked_add(skipped_events)
            .and_then(|value| value.checked_add(skipped_file_touches))
            .ok_or(CaptureError::SystemInvariant(
                "native JSONL replay count overflowed",
            ))?;
        let failed = usize::try_from(self.rejected_records).map_err(|_| {
            CaptureError::SystemInvariant(
                "native JSONL replay rejection count exceeds platform limits",
            )
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

impl CapturedBatchProjector for NativeJsonlCapturedBatchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        if record.record_kind().as_str()
            != native_jsonl_record_kind(self.provider, &self.source_format)
        {
            return Err(ProviderProjectionFatal::system_invariant(
                "native JSONL projector received an unexpected record kind",
            ));
        }
        let line_number = self
            .line_number(record.ordinal())
            .map_err(ProviderProjectionFatal::new)?;
        let CapturedRecordPayload::NativeBytes(bytes) = record.payload() else {
            return Err(ProviderProjectionFatal::system_invariant(
                "native JSONL projector requires native bytes",
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
                    native_jsonl_file_failure(&self.path, format!("malformed JSONL: {error}")),
                );
            }
        };
        if self.session.is_none() {
            if !native_jsonl_record_starts_session(self.provider, &value) {
                return self.reject_record(
                    output,
                    line_number,
                    native_jsonl_file_failure(
                        &self.path,
                        "record appeared before an importable native JSONL session header",
                    ),
                );
            }
            let session = NativeJsonlSessionCheckpoint::from_header(
                self.provider,
                &self.path,
                &self.context,
                &value,
                native_jsonl_header_anchor(record).map_err(ProviderProjectionFatal::new)?,
            );
            self.session = Some(session);
        }
        let (mut normalization, result_content_ref) = self
            .session
            .as_ref()
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "native JSONL projector did not retain its discovered session",
                )
            })?
            .capture(
                self.provider,
                &self.source_format,
                &self.path,
                &self.context,
                &value,
                line_number,
            );
        if let Some(event) = normalization
            .captures
            .first_mut()
            .and_then(|(_, capture)| capture.event.as_mut())
        {
            crate::complete_content::jsonl::attach_jsonl_complete_content_locator(
                event,
                self.provider,
                &self.source_format,
                &value,
                record,
                line_number,
            )
            .map_err(ProviderProjectionFatal::new)?;
            crate::complete_content::jsonl::attach_native_jsonl_result_content_locator(
                event,
                self.provider,
                &self.source_format,
                &value,
                record,
                line_number,
                result_content_ref.as_ref(),
            )
            .map_err(ProviderProjectionFatal::new)?;
        }
        let event = normalization
            .captures
            .first()
            .and_then(|(_, capture)| capture.event.clone());
        output.use_explicit_file_touches();
        if self.pending_resume_session_bootstrap {
            let (_, capture) = normalization.captures.first().ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "native JSONL accepted record did not produce a capture",
                )
            })?;
            let session = self.session.as_ref().ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "native JSONL resume bootstrap lost its discovered session",
                )
            })?;
            let bootstrap_line = usize::try_from(session.header_anchor.ordinal)
                .ok()
                .and_then(|ordinal| ordinal.checked_add(1))
                .ok_or_else(|| {
                    ProviderProjectionFatal::system_invariant(
                        "native JSONL header ordinal exceeds platform limits",
                    )
                })?;
            let mut bootstrap = capture.clone();
            bootstrap.event = None;
            let after = bootstrap
                .source
                .cursor
                .as_mut()
                .and_then(|cursor| {
                    cursor.before = None;
                    cursor.after.as_mut()
                })
                .ok_or_else(|| {
                    ProviderProjectionFatal::system_invariant(
                        "native JSONL resume bootstrap requires a source cursor",
                    )
                })?;
            after.cursor = format!("{}:line:{bootstrap_line}", self.path.display());
            after.observed_at = session.started_at;
            emit_projected_normalization_units(
                output,
                ProviderNormalizationResult {
                    captures: vec![(bootstrap_line, bootstrap)],
                    ..ProviderNormalizationResult::default()
                },
            )?;
            self.pending_resume_session_bootstrap = false;
        }
        self.accept(normalization, output)?;
        let Some(event) = event else {
            return Ok(());
        };
        let raw_source_path = self.path.display().to_string();
        let source_root = self
            .context
            .source_root_display()
            .or_else(|| Some(raw_source_path.clone()));
        let file_touch_outcome = visit_provider_file_touches_from_raw_value(
            ProviderFileTouchSourceContext::new(
                self.provider,
                self.session
                    .as_ref()
                    .map(|session| session.provider_session_id.as_str())
                    .ok_or_else(|| {
                        ProviderProjectionFatal::system_invariant(
                            "native JSONL projector lost its discovered session",
                        )
                    })?,
                &self.source_format,
                Some(raw_source_path.as_str()),
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
            self.reject_record(
                output,
                line_number,
                PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
            )?;
        }
        let file_touch_count = u64::try_from(file_touch_outcome.emitted())
            .map_err(|_| CaptureError::SystemInvariant("native JSONL file-touch count exceeds u64"))
            .map_err(ProviderProjectionFatal::new)?;
        self.accepted_file_touches = self
            .accepted_file_touches
            .checked_add(file_touch_count)
            .ok_or_else(|| {
                ProviderProjectionFatal::system_invariant(
                    "native JSONL file-touch count overflowed",
                )
            })?;
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        if *position != initial_jsonl_position().map_err(native_jsonl_batch_error)? {
            return Err(CaptureError::InvalidPayload(
                "native JSONL initial cursor candidate is not at the JSONL source start".to_owned(),
            ));
        }
        CertifiedProviderCursor::new(
            source.source_revision(),
            source.capture_revision(),
            source.policy_revision(),
            position.clone(),
            BoundedParserCheckpoint::from_serializable(&NativeJsonlParserCheckpoint {
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
                "native JSONL captured batch did not have a next ordinal",
            ))?;
        if self.next_ordinal > next_ordinal {
            return Err(CaptureError::SystemInvariant(
                "native JSONL projector advanced beyond the captured batch",
            ));
        }
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&NativeJsonlParserCheckpoint {
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
fn native_jsonl_header_anchor(record: &CapturedRecord) -> Result<NativeJsonlHeaderAnchor> {
    let locator = record.locator();
    if locator.kind() != NATIVE_JSONL_LOCATOR_KIND {
        return Err(CaptureError::InvalidPayload(
            "native JSONL header has an invalid locator kind".to_owned(),
        ));
    }
    let value = locator.value();
    let source_length_bytes = value.get(..4).ok_or_else(|| {
        CaptureError::InvalidPayload("native JSONL header locator is truncated".to_owned())
    })?;
    let source_length = usize::try_from(u32::from_be_bytes(
        source_length_bytes.try_into().map_err(|_| {
            CaptureError::InvalidPayload(
                "native JSONL header locator has an invalid source length".to_owned(),
            )
        })?,
    ))
    .map_err(|_| {
        CaptureError::InvalidPayload(
            "native JSONL header locator source length exceeds platform limits".to_owned(),
        )
    })?;
    let range_start = 4_usize.checked_add(source_length).ok_or_else(|| {
        CaptureError::InvalidPayload("native JSONL header locator length overflowed".to_owned())
    })?;
    let expected_length = range_start.checked_add(16).ok_or_else(|| {
        CaptureError::InvalidPayload("native JSONL header locator length overflowed".to_owned())
    })?;
    if value.len() != expected_length {
        return Err(CaptureError::InvalidPayload(
            "native JSONL header locator has an invalid length".to_owned(),
        ));
    }
    let start = u64::from_be_bytes(value[range_start..range_start + 8].try_into().map_err(
        |_| CaptureError::InvalidPayload("native JSONL header locator start is invalid".to_owned()),
    )?);
    let end = u64::from_be_bytes(value[range_start + 8..expected_length].try_into().map_err(
        |_| CaptureError::InvalidPayload("native JSONL header locator end is invalid".to_owned()),
    )?);
    if start >= end {
        return Err(CaptureError::InvalidPayload(
            "native JSONL header locator range is invalid".to_owned(),
        ));
    }
    let CapturedRecordPayload::NativeBytes(payload) = record.payload() else {
        return Err(CaptureError::SystemInvariant(
            "native JSONL header anchor requires native bytes",
        ));
    };
    Ok(NativeJsonlHeaderAnchor {
        ordinal: record.ordinal(),
        start,
        end,
        payload_sha256: native_jsonl_header_anchor_digest(payload),
    })
}

pub(super) fn native_jsonl_header_anchor_digest(payload: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(NATIVE_JSONL_HEADER_ANCHOR_HASH_DOMAIN);
    hasher.update((payload.len() as u64).to_be_bytes());
    hasher.update(payload);
    hasher.finalize().into()
}
fn native_jsonl_file_failure(path: &Path, reason: impl AsRef<str>) -> String {
    format!("{}: {}", path.display(), reason.as_ref())
}

pub(super) fn native_jsonl_batch_error(error: JsonlBatchError) -> CaptureError {
    match error {
        JsonlBatchError::Io(error) => CaptureError::Io(error),
        JsonlBatchError::SourceChangedDuringRead { .. } => CaptureError::SourceChangedDuringCapture,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}
