//! Provider-owned Continue NativePath discovery and normalization.
//!
//! Provider-private DTOs retain Continue's schema authority while the
//! source-backed reader projects bounded, complete Core pages directly.

mod decode;
mod normalize;
mod parse;
mod source;
mod source_backed;

pub(crate) use normalize::{
    ContinueEventKind, ContinueEventRole, ContinueEventRow, ContinueGenerationAuthority,
};
pub(crate) use source_backed::{ContinueSourceBackedOutcome, ContinueSourceBackedReader};

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
    #[error("Continue source `{path}` exceeds the {limit} byte limit ({observed} bytes)")]
    SourceTooLarge {
        path: PathBuf,
        limit: usize,
        observed: u64,
    },
    #[error("Continue source `{path}` changed while it was being read")]
    SourceChanged { path: PathBuf },
}
