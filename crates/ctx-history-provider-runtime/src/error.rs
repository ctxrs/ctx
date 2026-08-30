use std::path::PathBuf;

use ctx_history_capture_model::ProviderSourceFailureKind;
use ctx_history_jsonl::JsonlFamilyError;
use thiserror::Error;

pub type ProviderJsonlInventoryLimit = ctx_history_source_io::ProviderJsonlInventoryLimit;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("uuid parse error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("unsupported capture envelope schema version: {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("unsupported provider schema: {0}")]
    UnsupportedSchema(String),
    #[error("invalid capture payload: {0}")]
    InvalidPayload(String),
    #[error("invalid provider transcript path {path:?}: {reason}")]
    InvalidProviderTranscriptPath { path: PathBuf, reason: &'static str },
    #[error(
        "provider JSONL inventory exceeded {limit} limit: observed {observed}, maximum {maximum}"
    )]
    ProviderJsonlInventoryLimitExceeded {
        limit: ProviderJsonlInventoryLimit,
        maximum: usize,
        observed: usize,
    },
    #[error("{0} worker thread panicked")]
    WorkerPanicked(&'static str),
    #[error("system I/O error during {operation}: {source}")]
    SystemIo {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("system invariant failed: {0}")]
    SystemInvariant(&'static str),
    #[error("provider source changed during bounded capture")]
    SourceChangedDuringCapture,
    #[error("{primary}; additional SQLite finalization failure: {finalization}")]
    SqliteFinalization {
        primary: Box<CaptureError>,
        finalization: Box<CaptureError>,
    },
    #[error("{provider} source {path:?} failed ({kind}): {detail}")]
    ProviderSource {
        provider: &'static str,
        path: PathBuf,
        kind: ProviderSourceFailureKind,
        detail: String,
    },
    #[error("provider cursor changed during bounded capture")]
    ProviderCursorConflict,
    #[error("line {line} in {path:?} is not a valid capture envelope: {source}")]
    InvalidJsonLine {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, CaptureError>;

impl From<ctx_history_source_io::SourceIoError> for CaptureError {
    fn from(error: ctx_history_source_io::SourceIoError) -> Self {
        use ctx_history_source_io::SourceIoError;

        match error {
            SourceIoError::Io(error) => Self::Io(error),
            SourceIoError::Json(error) => Self::Json(error),
            SourceIoError::InvalidPayload(detail) => Self::InvalidPayload(detail),
            SourceIoError::InvalidProviderTranscriptPath { path, reason } => {
                Self::InvalidProviderTranscriptPath { path, reason }
            }
            SourceIoError::ProviderJsonlInventoryLimitExceeded {
                limit,
                maximum,
                observed,
            } => Self::ProviderJsonlInventoryLimitExceeded {
                limit,
                maximum,
                observed,
            },
            SourceIoError::SystemIo { operation, source } => Self::SystemIo { operation, source },
            SourceIoError::SystemInvariant(detail) => Self::SystemInvariant(detail),
            SourceIoError::SourceChangedDuringCapture => Self::SourceChangedDuringCapture,
        }
    }
}

impl From<ctx_history_source_sqlite::SqliteIoError> for CaptureError {
    fn from(error: ctx_history_source_sqlite::SqliteIoError) -> Self {
        use ctx_history_source_sqlite::SqliteIoError;

        match error {
            SqliteIoError::Io(error) => Self::Io(error),
            SqliteIoError::Sqlite(error) => Self::Sqlite(error),
            SqliteIoError::Json(error) => Self::Json(error),
            SqliteIoError::InvalidPayload(detail) => Self::InvalidPayload(detail),
            SqliteIoError::InvalidProviderTranscriptPath { path, reason } => {
                Self::InvalidProviderTranscriptPath { path, reason }
            }
            SqliteIoError::SystemIo { operation, source } => Self::SystemIo { operation, source },
            SqliteIoError::SystemInvariant(detail) => Self::SystemInvariant(detail),
            SqliteIoError::SourceChangedDuringCapture => Self::SourceChangedDuringCapture,
            SqliteIoError::SqliteFinalization {
                primary,
                finalization,
            } => Self::SqliteFinalization {
                primary: Box::new(Self::from(*primary)),
                finalization: Box::new(Self::from(*finalization)),
            },
        }
    }
}

impl JsonlFamilyError for CaptureError {
    fn invalid_payload(detail: String) -> Self {
        Self::InvalidPayload(detail)
    }

    fn system_invariant(detail: &'static str) -> Self {
        Self::SystemInvariant(detail)
    }

    fn worker_panicked(worker: &'static str) -> Self {
        Self::WorkerPanicked(worker)
    }

    fn source_changed() -> Self {
        Self::SourceChangedDuringCapture
    }

    fn is_not_found(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
            || matches!(self, Self::SystemIo { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }

    fn is_source_changed(&self) -> bool {
        matches!(self, Self::SourceChangedDuringCapture)
            || matches!(
                self,
                Self::InvalidProviderTranscriptPath { reason, .. }
                    if *reason == "provider source changed while its authority handle was retained"
            )
    }

    fn is_source_unavailable(&self) -> bool {
        matches!(
            self,
            Self::SystemIo { operation, source }
                if ctx_history_source_io::is_provider_source_unavailable_io(operation, source)
        )
    }

    fn is_resource_unavailable(&self) -> bool {
        matches!(self, Self::Io(_) | Self::SystemIo { .. })
            && !self.is_not_found()
            && !self.is_source_unavailable()
    }

    fn is_internal(&self) -> bool {
        matches!(self, Self::SystemInvariant(_) | Self::WorkerPanicked(_))
    }

    fn is_ignorable_membership_entry(&self) -> bool {
        matches!(
            self,
            Self::InvalidProviderTranscriptPath { reason, .. }
                if *reason == ctx_history_source_io::SYMLINK_PROVIDER_SOURCE_REASON
                    || *reason == ctx_history_source_io::REPARSE_PROVIDER_SOURCE_REASON
                    || *reason == ctx_history_source_io::NON_REGULAR_PROVIDER_SOURCE_REASON
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_io_mapping_preserves_provider_source_unavailability() {
        let mapped = CaptureError::from(ctx_history_source_io::SourceIoError::SystemIo {
            operation: "provider source target open",
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        });

        assert!(mapped.is_source_unavailable());
        assert!(!mapped.is_resource_unavailable());
    }

    #[test]
    fn source_io_mapping_preserves_tagged_resource_exhaustion() {
        let mapped = CaptureError::from(ctx_history_source_io::SourceIoError::SystemIo {
            operation: "provider source target open",
            source: std::io::Error::from_raw_os_error(24),
        });

        assert!(!mapped.is_source_unavailable());
        assert!(mapped.is_resource_unavailable());
    }

    #[test]
    fn source_io_mapping_preserves_inventory_limit_identity() {
        let mapped = CaptureError::from(
            ctx_history_source_io::SourceIoError::ProviderJsonlInventoryLimitExceeded {
                limit: ctx_history_source_io::ProviderJsonlInventoryLimit::EligiblePaths,
                maximum: 7,
                observed: 8,
            },
        );
        assert!(matches!(
            mapped,
            CaptureError::ProviderJsonlInventoryLimitExceeded {
                limit: ProviderJsonlInventoryLimit::EligiblePaths,
                maximum: 7,
                observed: 8,
            }
        ));
    }
}
