use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::ContentRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    complete_content::{
        jsonl::{mux_record_locator, MUX_LOCATOR_KIND},
        CompleteContentBodyDigest, CompleteContentSourceLocator,
    },
    provider::file_touches::{
        event_type_supports_structured_file_touches, visit_provider_file_touch_drafts_with_limit,
        PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
    },
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::{
    metadata::{bounded_mux_failure, bounded_mux_id, MuxBoundedSessionMetadata},
    normalization::{
        apply_mux_core_output_diagnostic, mux_core_event, mux_event_id, mux_event_text,
        mux_event_type, mux_message_model, mux_message_timestamp_opt, mux_output_projection,
        mux_partial_event_index, mux_result_content, MuxCoreEvent, MuxMessageRow, MuxOutputOutcome,
    },
    source::{visit_mux_session_sources, MuxFileObservation, MuxSessionSource},
};

mod model;
mod parse;
mod source;
#[cfg_attr(not(test), allow(dead_code))]
mod source_backed;

use model::*;

pub(crate) use source_backed::{
    discover_mux_source_backed_sources, revalidate_mux_source_backed, scan_mux_source_backed,
    MuxBoundedProjection, MuxReplacementEvidence, MuxReplacementReason, MuxSourceBackedCandidate,
    MuxSourceBackedDisposition, MuxSourceBackedError, MuxSourceBackedPage, MuxSourceBackedRecord,
    MuxSourceBackedResolverV0, MuxSourceBackedResult, MuxSourceBackedScanReceipt,
    MuxUnaddressableReason, MuxUnaddressableRecord,
};

/// Temporary compatibility entry point for shared v0.25 import APIs.
///
/// Mux production ingestion is source-backed. The legacy Store publisher was
/// deleted provider-locally and must not be reintroduced behind this symbol.
pub(crate) fn import_mux_native_path(
    _path: &Path,
    _store: &mut ctx_history_store::Store,
    _context: ProviderAdapterContext,
    _options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(
        "Mux legacy Store publication is unavailable; use source-backed ingestion".to_owned(),
    ))
}
