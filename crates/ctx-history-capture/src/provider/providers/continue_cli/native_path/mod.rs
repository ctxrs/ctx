//! Provider-owned Continue NativePath discovery and normalization.
//!
//! Provider-private DTOs retain Continue's schema authority while the
//! source-backed reader projects bounded lexical pages and exact locators.

mod decode;
mod lifecycle;
mod normalize;
mod parse;
mod source;
mod source_backed;

pub(crate) use normalize::{
    ContinueEventKind, ContinueEventRole, ContinueEventRow, ContinueGenerationAuthority,
    ContinueSessionRow,
};
pub(crate) use source::{
    discover_continue_root, ContinueIndexObservation, ContinueSourceObservation,
};
pub(crate) use source_backed::{
    hydrate_continue_source_backed_record, ContinueSourceBackedOutcome, ContinueSourceBackedReader,
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
