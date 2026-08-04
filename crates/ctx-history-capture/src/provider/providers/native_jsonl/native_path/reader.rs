use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType};
use serde_json::{json, Value};

use crate::provider::file_touches::visit_all_file_touch_drafts;
use crate::provider::source_backed::family::jsonl::JsonlRecordRef;
use crate::{CaptureError, OutputOutcome, Result};

use super::super::{
    dialect::{native_jsonl_record_starts_session, validate_direct_native_jsonl_provider},
    normalization,
    normalization::{
        antigravity_session_id_from_path, native_jsonl_entry_type, native_jsonl_event_text,
        native_jsonl_event_type, native_jsonl_header_cwd, native_jsonl_header_session_id,
        native_jsonl_header_start_time, native_jsonl_model, native_jsonl_path_session,
        native_jsonl_role, native_jsonl_session_metadata_from_normalized_header,
        native_jsonl_session_status, native_jsonl_timestamp, native_jsonl_tokens,
    },
    result_content,
    result_content::{
        enumerate_native_jsonl_result_subrecords, native_jsonl_result_content_profile,
        NativeJsonlResultExtractionError,
    },
};
use super::{
    copilot, enumerate_factory_droid_results, factory_droid_event_identity,
    factory_droid_event_text, factory_droid_event_type, factory_droid_model,
    factory_droid_retry_discriminator, factory_droid_role, qoder_parser, qwen_code, tabnine,
    windsurf, DirectJsonlEvent, DirectJsonlRejection, DirectJsonlSession, DirectJsonlSourceRecord,
    DirectJsonlTouch,
};

#[path = "reader_projection.rs"]
mod projection;
pub(super) use projection::ProjectedLine;

pub(crate) struct DirectJsonlProjector {
    pub(super) provider: CaptureProvider,
    pub(super) source_format: String,
    pub(super) path: PathBuf,
    pub(super) source_root: Option<PathBuf>,
    pub(super) imported_at: DateTime<Utc>,
    pub(super) session: Option<DirectJsonlSession>,
    pub(super) copilot_mcp_tool_calls: copilot::CopilotMcpToolCallAttributions,
}

impl DirectJsonlProjector {
    pub(crate) fn new(
        provider: CaptureProvider,
        source_format: &str,
        path: &Path,
        source_root: Option<PathBuf>,
        imported_at: DateTime<Utc>,
        session: Option<DirectJsonlSession>,
    ) -> Result<Self> {
        validate_direct_native_jsonl_provider(provider)?;
        if provider == CaptureProvider::Gemini {
            return Err(CaptureError::SystemInvariant(
                "Gemini requires its bespoke NativePath reader",
            ));
        }
        Ok(Self {
            provider,
            source_format: source_format.to_owned(),
            path: path.to_path_buf(),
            source_root,
            imported_at,
            session,
            copilot_mcp_tool_calls: copilot::CopilotMcpToolCallAttributions::new(),
        })
    }

    pub(super) fn set_copilot_mcp_tool_calls(
        &mut self,
        attributions: copilot::CopilotMcpToolCallAttributions,
    ) {
        self.copilot_mcp_tool_calls = attributions;
    }

    pub(crate) fn project_record(&mut self, record: JsonlRecordRef<'_>) -> Result<ProjectedLine> {
        self.project_record_inner(record)
    }

    pub(crate) fn identify_record(
        &mut self,
        record: JsonlRecordRef<'_>,
    ) -> Result<Vec<DirectJsonlRejection>> {
        Ok(self.project_record_inner(record)?.rejections)
    }

    fn project_record_inner(&mut self, record: JsonlRecordRef<'_>) -> Result<ProjectedLine> {
        let evidence = record.evidence();
        self.copilot_mcp_tool_calls
            .observe_projected_record(evidence.physical_ordinal(), evidence.record_digest());
        if record.oversized() {
            if self.provider != CaptureProvider::CopilotCli {
                return Err(CaptureError::SystemInvariant(
                    "non-Copilot direct JSONL projector received an oversized record",
                ));
            }
            return Ok(ProjectedLine::rejection(DirectJsonlRejection {
                raw_ordinal: evidence.physical_ordinal(),
                byte_start: evidence.byte_start(),
                byte_end_exclusive: evidence.byte_end_exclusive(),
                reason: format!(
                    "{}:{} discarded oversized Copilot CLI JSONL record",
                    self.path.display(),
                    evidence.physical_ordinal().saturating_add(1)
                ),
            }));
        }
        self.project_line(
            record.bytes(),
            evidence.physical_ordinal(),
            evidence.byte_start(),
            evidence.byte_end_exclusive(),
            evidence.record_digest(),
        )
    }

    pub(crate) fn session(&self) -> Option<&DirectJsonlSession> {
        self.session.as_ref()
    }

    pub(super) fn copilot_attribution_projection_matches(&self) -> bool {
        self.copilot_mcp_tool_calls.projected_records_match()
    }
}
