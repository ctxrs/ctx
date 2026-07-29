//! Provider-owned Cline/Roo source discovery, parsing, and exact hydration.

mod bounded;
mod normalize;
mod parse;
mod reader;
mod source;
mod source_backed;

#[cfg(test)]
mod source_backed_tests;

pub(super) use normalize::{
    ClineArrayCheckpoint, ClineCatalogCompletion, ClineCatalogIndex, ClineCertifiedPage,
    ClineEventComponent, ClineEventKind, ClineEventRole, ClineEventRow, ClineFileSourceIdentity,
    ClineMetadataCheckpoint, ClineNativeItemKey, ClinePageFrontier, ClineSessionRow,
    ClineTaskCheckpoint, ClineTaskIdentity, ClineTaskIdentityOrigin,
};
#[cfg(test)]
pub(super) use normalize::{
    ClineComponentFailureKind, ClineComponentReadOutcome, ClineComponentTransition,
    ClineItemRejectionKind, ClinePublicationStats, ClineTerminalEvidence,
    CLINE_NATIVE_PAGE_MAX_UNITS,
};
pub(super) use reader::ClineNativeReader;
#[cfg(test)]
pub(crate) use source::{
    clear_cline_io_failure, inject_cline_io_failure, ClineInjectedIoOperation,
};
pub(super) use source::{
    discover_cline_root, discover_roo_root, revalidate_cline_component_source, ClineComponent,
    ClineComponentObservation, ClineDiscovery, ClineFileStamp, ClineLiveTaskObservation,
    ClineObservedFileState, TaskJsonNativeDialect,
};
pub(crate) use source_backed::{
    cline_task_json_source_backed_adapter, cline_task_json_source_backed_resolver,
    roo_task_json_source_backed_adapter, roo_task_json_source_backed_resolver,
    TaskJsonSourceBackedCompletion, TaskJsonSourceBackedPage, TaskJsonSourceBackedResolver,
};

use std::{
    io,
    path::{Path, PathBuf},
};

use ctx_history_store::Store;
use thiserror::Error;

use crate::{
    CaptureError, ClineTaskJsonImportOptions, ProviderImportSummary, Result,
    RooTaskJsonImportOptions,
};

pub(crate) fn import_cline_nativepath_history(
    _path: &Path,
    _store: &mut Store,
    _options: ClineTaskJsonImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(
        "Cline Store ingestion was removed; use source-backed ingestion".to_owned(),
    ))
}

pub(crate) fn import_roo_nativepath_history(
    _path: &Path,
    _store: &mut Store,
    _options: RooTaskJsonImportOptions,
) -> Result<ProviderImportSummary> {
    Err(CaptureError::UnsupportedSchema(
        "Roo Code Store ingestion was removed; use source-backed ingestion".to_owned(),
    ))
}

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
