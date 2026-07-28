//! Provider-owned Continue NativePath discovery and normalization.
//!
//! Provider-private DTOs retain Continue's schema authority. The Store adapter
//! converts only certified page/frontier mechanics into canonical Store rows.

mod decode;
mod lifecycle;
mod normalize;
mod parse;
mod production;
mod source;
mod source_backed;
mod store_adapter;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use lifecycle::{prepare_continue_discovery, ContinuePreparationStats};
pub(crate) use lifecycle::{prepare_continue_discovery_with_profile, ContinueSourceOutcome};
pub(crate) use normalize::{
    ContinueEventKind, ContinueEventRole, ContinueEventRow, ContinueGenerationAuthority,
    ContinueNativeProfile, ContinuePreparedPage, ContinuePreparedSource, ContinueSessionIdentity,
    ContinueSessionRow, ContinueTransientOutputPayload,
};
#[cfg(test)]
pub(crate) use normalize::{
    ContinueSourceCompleteness, CONTINUE_NATIVE_MAX_PAGE_BYTES, CONTINUE_NATIVE_MAX_PAGE_ROWS,
};
#[cfg(test)]
pub(crate) use parse::{ContinueOutputExclusionStats, ContinueSourceFailureKind};
pub(crate) use production::import_continue_nativepath_history;
#[cfg(test)]
pub(crate) use source::{
    clear_continue_io_failure, inject_continue_io_failure, observe_continue_pending_paths,
    ContinueDiscovery, ContinueIndexState, ContinueInjectedIoOperation,
};
pub(crate) use source::{
    discover_continue_root, ContinueIndexObservation, ContinueIndexSnapshot,
    ContinueSourceObservation,
};
pub(crate) use source_backed::{
    hydrate_continue_source_backed_record, ContinueSourceBackedOutcome, ContinueSourceBackedReader,
};
pub(crate) use store_adapter::{
    ContinueNativePageAdapter, ContinueNativeStoreCursor, ContinuePageFrontier,
};

use std::{io, path::PathBuf};

use thiserror::Error;

// Structural provider failures remain source-addressable, provider-owned I/O
// retains its OS classification, and ctx-owned spool I/O stays systemic.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Error)]
pub(crate) enum ContinueNativePathError {
    #[error("failed to access Continue source `{path}`: {message}")]
    SourceAccess { path: PathBuf, message: String },
    #[error(
        "Continue source I/O failed during {operation} for `{path}` ({kind:?}, os={raw_os_error:?}): {message}"
    )]
    SourceIo {
        path: PathBuf,
        operation: &'static str,
        kind: io::ErrorKind,
        raw_os_error: Option<i32>,
        message: String,
    },
    #[error("system I/O failed during {operation}: {source}")]
    SystemIo {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("Continue NativePath invariant failed: {message}")]
    Invariant { message: &'static str },
    #[error("Continue source `{path}` exceeds the {limit} byte limit ({observed} bytes)")]
    SourceTooLarge {
        path: PathBuf,
        limit: usize,
        observed: u64,
    },
    #[error("Continue source `{path}` changed while it was being read")]
    SourceChanged { path: PathBuf },
    #[cfg(test)]
    #[error("Continue pending page exceeds the {limit} path limit ({observed} paths)")]
    PendingPageTooLarge { limit: usize, observed: usize },
}
