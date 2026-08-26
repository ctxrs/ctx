//! Provider-owned Cline/Roo source discovery, parsing, and Core projection.

mod bounded;
mod normalize;
mod parse;
mod reader;
mod source;
mod source_backed;

#[cfg(test)]
mod source_backed_tests;

pub(super) use reader::ClineNativeReader;
pub(super) use source::{discover_cline_root, discover_roo_root};
pub use source_backed::{
    cline_task_json_source_backed_adapter, cline_task_json_source_backed_adapter_scoped,
    roo_task_json_source_backed_adapter, roo_task_json_source_backed_adapter_scoped,
};

use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum ClineNativePathError {
    #[error("failed to access Cline source `{path}`: {message}")]
    SourceAccess { path: PathBuf, message: String },
    #[error(
        "Cline source I/O failed during {operation} for `{path}` ({kind:?}, os={raw_os_error:?}): {message}"
    )]
    SourceIo {
        path: PathBuf,
        operation: &'static str,
        kind: io::ErrorKind,
        raw_os_error: Option<i32>,
        message: String,
    },
    #[error("systemic Cline source failure for `{path}`: {message}")]
    SystemicSource { path: PathBuf, message: String },
    #[error("Cline source `{path}` changed before its page could be certified")]
    SourceChanged { path: PathBuf },
    #[error("Cline source `{path}` is not a supported task root or component")]
    UnsupportedRoot { path: PathBuf },
    #[error("Cline task identity or native order exceeds supported bounds: {message}")]
    InvalidNativeIdentity { message: String },
    #[error("Cline NativePath invariant failed: {message}")]
    Invariant { message: String },
}
