//! Path admission and route-error helpers shared by the selected SQLite
//! providers.
//!
//! Every provider in this pack opens its database through a retained directory
//! authority, so each one has to answer the same three questions about its
//! source path — does it have a parent directory, does it have a leaf name, and
//! is the leaf a regular non-symlink file — and each one has to translate a
//! scan failure into the same two route-error kinds. The prose differs only in
//! the provider's name, which is why the reasons arrive as data.

use std::{ffi::OsStr, io, path::Path};

use ctx_history_capture_runtime::{SourceBackedRouteError, SourceBackedRouteErrorKind};

use crate::{provider_sources::SqliteSourceAccessError, CaptureError};

/// The provider-named prose for each way a SQLite source path can be
/// inadmissible.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SqliteSourcePathReasons {
    pub(crate) missing_parent: &'static str,
    pub(crate) missing_leaf: &'static str,
    pub(crate) not_regular_file: &'static str,
}

/// The directory that will hold the retained authority handle.
///
/// An empty parent is rejected rather than defaulted, because a relative
/// single-component path gives no directory to retain authority over.
pub(crate) fn database_parent<'a>(
    path: &'a Path,
    reasons: &SqliteSourcePathReasons,
) -> Result<&'a Path, CaptureError> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: reasons.missing_parent,
        })
}

/// The database file name, opened relative to the retained parent handle.
pub(crate) fn database_leaf<'a>(
    path: &'a Path,
    reasons: &SqliteSourcePathReasons,
) -> Result<&'a OsStr, CaptureError> {
    path.file_name()
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: reasons.missing_leaf,
        })
}

pub(crate) fn invalid_database_leaf(
    path: &Path,
    reasons: &SqliteSourcePathReasons,
) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: reasons.not_regular_file,
    }
}

/// The source moved or was rewritten while it was pinned, so the generation is
/// retried rather than published.
pub(crate) fn source_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

pub(crate) fn internal_error(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

/// Reports a snapshot-access failure as systemic IO, naming the provider
/// operation that was in flight.
pub(crate) fn sqlite_access_error(
    operation: &'static str,
    error: SqliteSourceAccessError,
) -> CaptureError {
    CaptureError::SystemIo {
        operation,
        source: io::Error::other(error),
    }
}
