//! Logical-schema SQLite provider implementations.

use std::path::PathBuf;

use thiserror::Error;

pub mod providers;
pub mod registration;

pub use registration::{
    explicit_forgecode_route_plan, explicit_forgecode_route_plan_scoped, logical_sqlite_route_plan,
    logical_sqlite_route_plan_scoped, LogicalSqliteRegistrationError, LogicalSqliteRoutePlan,
};

pub use ctx_history_capture_model::{
    ProviderAdapterContext, ProviderImportFailure, ProviderSource, ProviderSourceFailureKind,
    RecordDigest, PROVIDER_MAX_PREVIEW_CHARS,
};
pub use ctx_history_source_sqlite::MAX_PROVIDER_SQLITE_VALUE_BYTES;

pub const DEEPAGENTS_SQLITE_SOURCE_FORMAT: &str = "deepagents_sessions_sqlite";
pub const FORGECODE_SQLITE_SOURCE_FORMAT: &str = "forgecode_sqlite";
pub const KILO_SQLITE_SOURCE_FORMAT: &str = "kilo_sqlite";
pub const MIMOCODE_SQLITE_SOURCE_FORMAT: &str = "mimocode_sqlite";
pub const OPENCODE_SQLITE_SOURCE_FORMAT: &str = "opencode_sqlite";
pub const ZED_THREADS_SQLITE_SOURCE_FORMAT: &str = "zed_threads_sqlite";
/// Compile-time binding supplied by the capture façade.
pub trait LogicalSqliteRuntimeBinding: Send + Sync + 'static {
    type Lifecycle: ctx_history_capture_runtime::CaptureLifecycleSink;
    type Spool: ctx_history_capture_runtime::DocumentRecordSpool;
    type RouteControl: Send + Sync + 'static;
}

pub type Result<T> = std::result::Result<T, CaptureError>;

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
    #[error("unsupported provider schema: {0}")]
    UnsupportedSchema(String),
    #[error("invalid capture payload: {0}")]
    InvalidPayload(String),
    #[error("invalid provider transcript path {path:?}: {reason}")]
    InvalidProviderTranscriptPath { path: PathBuf, reason: &'static str },
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
}

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
            SourceIoError::ProviderJsonlInventoryLimitExceeded { .. } => {
                Self::SystemInvariant("logical SQLite provider exceeded a source inventory limit")
            }
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
                primary: Box::new((*primary).into()),
                finalization: Box::new((*finalization).into()),
            },
        }
    }
}

pub fn compute_payload_hash(payload: &serde_json::Value) -> Result<String> {
    ctx_history_core::compute_payload_hash(payload).map_err(Into::into)
}

pub(crate) mod common {
    pub(crate) mod io {
        ctx_history_source_io::define_mapped_source_io_compat!(crate::CaptureError);
    }
}

pub(crate) mod provider_sources {
    pub(crate) use ctx_history_source_sqlite::{
        open_root_handle_sqlite_source_snapshot, resource_exhaustion_io_error,
        retain_sqlite_source_directory_authority, rusqlite_resource_failure, sqlite_retry_decision,
        SqliteArtifactKind, SqliteCleanupStatus, SqliteFailurePhase, SqliteLogicalSnapshot,
        SqliteRetryDecision, SqliteSourceAccessError, SqliteSourceDirectoryAuthority,
        SqliteSourceErrorComposition, SqliteSourceEvidence, SqliteSourceProgressError,
        SqliteSourceReadSnapshot, SqliteSourceTerminalFence,
    };
}

pub(crate) mod provider {
    pub(crate) mod native_ingestion {
        pub(crate) const NATIVE_INGESTION_PAGE_MAX_UNITS: usize = 64;
        pub(crate) const NATIVE_INGESTION_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
    }

    pub(crate) mod normalization {
        use chrono::{DateTime, Utc};

        pub(crate) fn provider_required_timestamp_millis(
            value: i64,
            field: &'static str,
        ) -> crate::Result<DateTime<Utc>> {
            DateTime::<Utc>::from_timestamp_millis(value).ok_or_else(|| {
                crate::CaptureError::InvalidPayload(format!(
                    "{field} is outside representable timestamp range: {value}"
                ))
            })
        }
    }

    pub(crate) mod providers {
        pub(crate) use crate::providers::*;
    }

    pub(crate) mod sqlite {
        use std::collections::BTreeSet;

        use rusqlite::Connection;

        pub(crate) use ctx_history_source_sqlite::{
            optional_column_expr, SqliteLengthPreflightGuard,
        };

        pub(crate) fn sqlite_table_exists(conn: &Connection, table: &str) -> crate::Result<bool> {
            ctx_history_source_sqlite::sqlite_table_exists(conn, table).map_err(Into::into)
        }

        pub(crate) fn sqlite_table_columns(
            conn: &Connection,
            table: &str,
        ) -> crate::Result<BTreeSet<String>> {
            ctx_history_source_sqlite::sqlite_table_columns(conn, table).map_err(Into::into)
        }

        pub(crate) fn ensure_sqlite_table_columns(
            columns: &BTreeSet<String>,
            label: &str,
            required: &[&str],
        ) -> crate::Result<()> {
            ctx_history_source_sqlite::ensure_sqlite_table_columns(columns, label, required)
                .map_err(Into::into)
        }

        pub(crate) fn sqlite_schema_fingerprint(conn: &Connection) -> crate::Result<String> {
            ctx_history_source_sqlite::sqlite_schema_fingerprint(conn).map_err(Into::into)
        }
    }

    pub(crate) mod source_backed {
        pub(crate) use ctx_history_capture_runtime::{
            combine_primary_and_cleanup_route_errors, SourceBackedCurrentSourceProgress,
            SourceBackedCurrentSourceProgressStage, SourceBackedRouteError,
            SourceBackedRouteErrorKind, SourceBackedRouteResult,
        };

        pub(crate) fn route_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::InvalidSource,
                error.to_string(),
            )
        }

        pub(crate) fn sqlite_source_progress(
            progress: ctx_history_source_sqlite::SqliteSourceProgress,
        ) -> SourceBackedCurrentSourceProgress {
            SourceBackedCurrentSourceProgress {
                stage: SourceBackedCurrentSourceProgressStage::SourceFamilyCopy,
                snapshot_pages_completed: progress.snapshot_pages_completed,
                snapshot_pages_total: progress.snapshot_pages_total,
                snapshot_bytes_completed: progress.snapshot_bytes_completed,
                snapshot_bytes_total: progress.snapshot_bytes_total,
                logical_rows_scanned: None,
                logical_certified_bytes: None,
            }
        }

        pub(crate) mod family {
            pub(crate) mod document {
                pub(crate) use ctx_history_capture_runtime::{
                    ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
                    DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
                };
            }
        }
    }
}

pub(crate) mod record_evidence {
    pub(crate) use ctx_history_capture_model::RecordDigest;

    use ctx_history_source_sqlite::NativeSqliteValue;

    pub(crate) fn sqlite_logical_record_digest(values: &[NativeSqliteValue]) -> RecordDigest {
        RecordDigest::from_sha256(
            ctx_history_source_sqlite::sqlite_logical_record_digest_bytes(values),
        )
    }
}

pub(crate) mod native_source {
    pub(crate) use ctx_history_source_sqlite::NativeSqliteValue;
}

#[cfg(test)]
pub(crate) use ctx_history_source_sqlite::fail_next_opened_snapshot_cleanup_for_test;

#[cfg(test)]
pub(crate) fn test_provider_sqlite_data_root() -> &'static std::path::Path {
    use std::sync::OnceLock;
    static ROOT: OnceLock<tempfile::TempDir> = OnceLock::new();
    ROOT.get_or_init(|| test_support_paths::tempdir().expect("provider SQLite test root"))
        .path()
}

#[cfg(test)]
pub(crate) mod test_support_paths {
    pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
        tempfile::Builder::new().prefix("ctx-test-").tempdir()
    }
}
