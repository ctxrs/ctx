mod metadata;
pub(crate) mod native_path;
mod normalization;
mod source;

#[allow(unused_imports)]
pub(crate) use native_path::{
    discover_mux_source_backed_sources, import_mux_native_path, revalidate_mux_source_backed,
    scan_mux_source_backed, MuxBoundedProjection, MuxReplacementEvidence, MuxReplacementReason,
    MuxSourceBackedCandidate, MuxSourceBackedDisposition, MuxSourceBackedError,
    MuxSourceBackedPage, MuxSourceBackedRecord, MuxSourceBackedResolverV0, MuxSourceBackedResult,
    MuxSourceBackedScanReceipt, MuxUnaddressableReason, MuxUnaddressableRecord,
};
pub(crate) use normalization::{mux_event_id, mux_event_text, mux_event_type};

const MUX_CAPTURE_REVISION: u32 = 2;
const MUX_POLICY_REVISION: u32 = 5;
const MUX_MAX_ID_BYTES: usize = 4 * 1024;
const MUX_MAX_FAILURE_BYTES: usize = 4 * 1024;

#[cfg(test)]
#[path = "mux/tests.rs"]
mod native_path_tests;
