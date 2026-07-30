use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, ContentRef, EventType};
use serde_json::{json, Value};

use crate::provider::file_touches::visit_all_file_touch_drafts;
use crate::provider::source_backed::family::jsonl::JsonlRecordRef;
use crate::{CaptureError, OutputOutcome, Result};

use super::super::{
    dialect::{native_jsonl_record_starts_session, validate_direct_native_jsonl_provider},
    normalization,
    normalization::{
        antigravity_session_id_from_path, native_jsonl_entry_type, native_jsonl_event_id,
        native_jsonl_event_text, native_jsonl_event_type, native_jsonl_header_cwd,
        native_jsonl_header_session_id, native_jsonl_header_start_time, native_jsonl_model,
        native_jsonl_path_session, native_jsonl_role,
        native_jsonl_session_metadata_from_normalized_header, native_jsonl_session_status,
        native_jsonl_timestamp, native_jsonl_tokens,
    },
    result_content,
    result_content::{
        enumerate_native_jsonl_result_subrecords, native_jsonl_result_content_profile,
        NativeJsonlResultExtractionError,
    },
};
use super::{
    copilot, enumerate_factory_droid_results, factory_droid_event_identity,
    factory_droid_event_text, factory_droid_event_type, factory_droid_model, factory_droid_role,
    qoder_parser, qwen_code, tabnine, windsurf, DirectJsonlEvent, DirectJsonlRejection,
    DirectJsonlSession, DirectJsonlSourceRecord, DirectJsonlTouch,
};

const DIRECT_JSONL_EVENT_ENVELOPE_BYTES: usize = 1024;
pub(super) const DIRECT_JSONL_MAX_FILE_TOUCHES_PER_RECORD: usize = 63;

#[path = "reader_projection.rs"]
mod projection;
pub(crate) use projection::direct_jsonl_complete_message_provider_event_hash;
pub(super) use projection::hydrated_direct_jsonl_lexical_text;
pub(super) use projection::ProjectedLine;

pub(crate) struct DirectJsonlProjector {
    pub(super) provider: CaptureProvider,
    pub(super) source_format: String,
    pub(super) path: PathBuf,
    pub(super) source_root: Option<PathBuf>,
    pub(super) imported_at: DateTime<Utc>,
    pub(super) session: Option<DirectJsonlSession>,
}

#[cfg(test)]
std::thread_local! {
    static DIRECT_JSONL_PROVIDER_PROJECTIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_provider_projection_count() {
    DIRECT_JSONL_PROVIDER_PROJECTIONS.set(0);
}

#[cfg(test)]
pub(super) fn provider_projection_count() -> usize {
    DIRECT_JSONL_PROVIDER_PROJECTIONS.get()
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
        })
    }

    pub(crate) fn project_record(&mut self, record: JsonlRecordRef<'_>) -> Result<ProjectedLine> {
        #[cfg(test)]
        DIRECT_JSONL_PROVIDER_PROJECTIONS
            .set(DIRECT_JSONL_PROVIDER_PROJECTIONS.get().saturating_add(1));
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
}

fn rejection_wire_bytes(rejection: &DirectJsonlRejection) -> usize {
    128_usize.saturating_add(rejection.reason.len())
}

fn event_wire_bytes(event: &DirectJsonlEvent) -> usize {
    DIRECT_JSONL_EVENT_ENVELOPE_BYTES
        .saturating_add(event.provider_event_hash.len())
        .saturating_add(event.lexical_text.len())
        .saturating_add(serde_json::to_vec(&event.metadata).map_or(usize::MAX, |value| value.len()))
        .saturating_add(
            event
                .touches
                .iter()
                .map(|touch| {
                    touch
                        .path
                        .len()
                        .saturating_add(touch.old_path.as_deref().map_or(0, str::len))
                })
                .sum::<usize>(),
        )
}
