use std::path::PathBuf;

use ctx_history_capture::CaptureError;
use ctx_history_core::{CaptureProvider, CoreError};
use ctx_history_store::StoreError;
use thiserror::Error as ThisError;

/// SDK result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by the in-process SDK facade.
#[derive(Debug, ThisError)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Search(#[from] ctx_history_search::SearchError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("ctx store is not initialized at {0}")]
    StoreNotInitialized(PathBuf),
    #[error("no importable provider history sources were found")]
    NoImportableSources,
    #[error("{provider} native import is unsupported: {reason}")]
    UnsupportedProviderImport {
        provider: CaptureProvider,
        reason: String,
    },
    #[error("{provider} is not registered for provider history import")]
    UnregisteredProvider { provider: CaptureProvider },
}
