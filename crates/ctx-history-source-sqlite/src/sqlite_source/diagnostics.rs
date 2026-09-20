use super::*;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteSourceComponent {
    RollbackJournal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteFailurePhase {
    SourceAcquisition,
    SourceValidation,
    Schema,
    Projection,
    Cleanup,
}

impl SqliteFailurePhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceAcquisition => "source_acquisition",
            Self::SourceValidation => "source_validation",
            Self::Schema => "schema",
            Self::Projection => "projection",
            Self::Cleanup => "cleanup",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteArtifactKind {
    ProviderDatabase,
    ProviderWal,
    ProviderSharedMemory,
    PrivateSourceCopy,
    PrivateScratch,
}

impl SqliteArtifactKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderDatabase => "provider_database",
            Self::ProviderWal => "provider_wal",
            Self::ProviderSharedMemory => "provider_shm",
            Self::PrivateSourceCopy => "private_source_copy",
            Self::PrivateScratch => "private_scratch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteCleanupStatus {
    NotRequired,
    Succeeded,
    Failed,
}

impl SqliteCleanupStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    const fn combine(self, later: Self) -> Self {
        match (self, later) {
            (Self::Failed, _) | (_, Self::Failed) => Self::Failed,
            (Self::Succeeded, _) | (_, Self::Succeeded) => Self::Succeeded,
            (Self::NotRequired, Self::NotRequired) => Self::NotRequired,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteFailureDiagnostic {
    pub phase: SqliteFailurePhase,
    pub artifact: SqliteArtifactKind,
    pub sqlite_primary_code: Option<i32>,
    pub sqlite_extended_code: Option<i32>,
    pub copied_pages: u64,
    pub copied_bytes: u64,
    pub cleanup: SqliteCleanupStatus,
}

impl std::fmt::Display for SqliteFailureDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "sqlite_phase={} artifact_kind={} sqlite_primary_code={} sqlite_extended_code={} copied_pages={} copied_bytes={} cleanup_status={}",
            self.phase.as_str(),
            self.artifact.as_str(),
            self.sqlite_primary_code
                .map_or_else(|| "none".to_owned(), |code| code.to_string()),
            self.sqlite_extended_code
                .map_or_else(|| "none".to_owned(), |code| code.to_string()),
            self.copied_pages,
            self.copied_bytes,
            self.cleanup.as_str(),
        )
    }
}

impl std::fmt::Display for SqliteSourceComponent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::RollbackJournal => "rollback journal",
        })
    }
}

#[derive(Debug, Error)]
pub enum SqliteSourceAccessError {
    #[error("{diagnostic}: {source}")]
    Diagnosed {
        diagnostic: SqliteFailureDiagnostic,
        #[source]
        source: Box<SqliteSourceAccessError>,
    },
    #[error("unsafe SQLite source file {path:?}: {reason}")]
    UnsafeFile { path: PathBuf, reason: &'static str },
    #[error("SQLite source I/O failed during {operation} for {path:?}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("SQLite source open failed during {operation}: {source}")]
    Sqlite {
        operation: &'static str,
        #[source]
        source: rusqlite::Error,
    },
    #[error("SQLite source resource is unavailable during {operation} for {path:?}: {source}")]
    ResourceUnavailable {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("private SQLite scratch resource is unavailable during {operation}: {source}")]
    ScratchSqliteUnavailable {
        operation: &'static str,
        #[source]
        source: rusqlite::Error,
    },
    #[error(
        "private SQLite scratch resource is unavailable during {operation} for {path:?}: {source}"
    )]
    ScratchIoUnavailable {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("ctx-owned SQLite cleanup failed during {operation}: {source}")]
    CleanupUnavailable {
        operation: &'static str,
        #[source]
        source: Box<SqliteSourceAccessError>,
    },
    #[error("{primary}; SQLite snapshot cleanup also failed: {cleanup}")]
    Finalization {
        primary: Box<SqliteSourceAccessError>,
        cleanup: Box<SqliteSourceAccessError>,
    },
    #[error("certified provider SQLite content is corrupt: {source}")]
    ProviderContentCorruption {
        #[source]
        source: Box<SqliteSourceAccessError>,
    },
    #[error("SQLite source control {operation} failed with code {code}")]
    SqliteControl { operation: &'static str, code: i32 },
    #[error("SQLite source connection is not read-only")]
    ConnectionNotReadOnly,
    #[error("SQLite source connection is not query-only")]
    ConnectionNotQueryOnly,
    #[error("SQLite source connection does not match the approved path")]
    ConnectionIdentityMismatch,
    #[error("SQLite source file changed while its read snapshot was active")]
    SourceChanged,
    #[error("SQLite source snapshot exceeds the bounded limit for {path:?}: {length} > {maximum}")]
    SnapshotTooLarge {
        path: PathBuf,
        length: u64,
        maximum: u64,
    },
    #[error(
        "provider SQLite scratch has insufficient free-space headroom for {path:?}: required {required}, available {available}"
    )]
    InsufficientScratchSpace {
        path: PathBuf,
        required: u64,
        available: u64,
    },
    #[error("SQLite source snapshot is unavailable: {reason}")]
    SnapshotUnavailable { reason: String },
    #[error("SQLite {component} is unavailable: {capability}")]
    UnsupportedSidecarIdentity {
        component: SqliteSourceComponent,
        capability: &'static str,
    },
    #[error("SQLite source snapshot transaction is no longer active")]
    SnapshotNotActive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteRetryDecision {
    DoNotRetry,
    DoNotRetryCorrupt,
    RetryBusyOrLocked,
    RetrySourceTransition,
    RouteFatalResource,
}

pub fn sqlite_retry_decision(error: &SqliteSourceAccessError) -> SqliteRetryDecision {
    if error.is_systemic_resource_failure() {
        SqliteRetryDecision::RouteFatalResource
    } else if error.is_source_changed() {
        SqliteRetryDecision::RetrySourceTransition
    } else if error.is_provider_corruption() || error.is_ctx_owned_corruption() {
        SqliteRetryDecision::DoNotRetryCorrupt
    } else if error.is_busy_or_locked() {
        SqliteRetryDecision::RetryBusyOrLocked
    } else {
        SqliteRetryDecision::DoNotRetry
    }
}

#[derive(Debug)]
pub enum SqliteSourceProgressError<E> {
    Source(SqliteSourceAccessError),
    Progress(E),
    ProgressAndFinalization {
        primary: E,
        finalization: SqliteSourceAccessError,
    },
}

impl<E> From<SqliteSourceAccessError> for SqliteSourceProgressError<E> {
    fn from(error: SqliteSourceAccessError) -> Self {
        Self::Source(error)
    }
}

impl<E> SqliteSourceProgressError<E> {
    pub(crate) fn with_finalization(self, finalization: SqliteSourceAccessError) -> Self {
        match self {
            Self::Source(primary) => Self::Source(SqliteSourceAccessError::Finalization {
                primary: Box::new(primary),
                cleanup: Box::new(finalization),
            }),
            Self::Progress(primary) => Self::ProgressAndFinalization {
                primary,
                finalization,
            },
            Self::ProgressAndFinalization {
                primary,
                finalization: earlier,
            } => Self::ProgressAndFinalization {
                primary,
                finalization: SqliteSourceAccessError::Finalization {
                    primary: Box::new(earlier),
                    cleanup: Box::new(finalization),
                },
            },
        }
    }
}

/// Allows a provider-owned callback error to preserve a later physical
/// SQLite cleanup failure without moving provider semantics into source I/O.
pub trait SqliteSourceErrorComposition: From<SqliteSourceAccessError> {
    fn compose_sqlite_source_finalization(self, finalization: SqliteSourceAccessError) -> Self;
}

impl SqliteSourceErrorComposition for SqliteSourceAccessError {
    fn compose_sqlite_source_finalization(self, finalization: SqliteSourceAccessError) -> Self {
        Self::Finalization {
            primary: Box::new(self),
            cleanup: Box::new(finalization),
        }
    }
}

impl SqliteSourceAccessError {
    pub fn acquisition_artifact(&self) -> SqliteArtifactKind {
        match self {
            Self::Diagnosed { source, .. } | Self::ProviderContentCorruption { source } => {
                source.acquisition_artifact()
            }
            Self::Finalization { primary, .. } => primary.acquisition_artifact(),
            Self::ScratchSqliteUnavailable { .. } | Self::ScratchIoUnavailable { .. } => {
                SqliteArtifactKind::PrivateSourceCopy
            }
            Self::Io { path, .. }
            | Self::ResourceUnavailable { path, .. }
            | Self::SnapshotTooLarge { path, .. }
            | Self::InsufficientScratchSpace { path, .. } => {
                let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
                if name.ends_with("-wal") {
                    SqliteArtifactKind::ProviderWal
                } else if name.ends_with("-shm") {
                    SqliteArtifactKind::ProviderSharedMemory
                } else {
                    SqliteArtifactKind::ProviderDatabase
                }
            }
            _ => SqliteArtifactKind::ProviderDatabase,
        }
    }

    pub fn is_systemic_resource_failure(&self) -> bool {
        matches!(
            self,
            Self::ResourceUnavailable { .. }
                | Self::ScratchSqliteUnavailable { .. }
                | Self::ScratchIoUnavailable { .. }
                | Self::CleanupUnavailable { .. }
                | Self::SnapshotTooLarge { .. }
                | Self::InsufficientScratchSpace { .. }
        ) || matches!(self, Self::Io { source, .. } if resource_exhaustion_io_error(source))
            || matches!(self, Self::Sqlite { source, .. } if rusqlite_resource_failure(source))
            || matches!(self, Self::SqliteControl { code, .. } if sqlite_resource_code(*code))
            || matches!(self, Self::Diagnosed { source, .. } | Self::ProviderContentCorruption { source } if source.is_systemic_resource_failure())
            || matches!(self, Self::Finalization { primary, cleanup } if primary.is_systemic_resource_failure() || cleanup.is_systemic_resource_failure())
    }

    /// A deterministic admission failure for one SQLite route. Provider
    /// coordinators can isolate this route and continue publishing healthy
    /// peers, unlike an I/O failure encountered after scratch writes begin.
    pub fn is_snapshot_capacity_failure(&self) -> bool {
        match self {
            Self::SnapshotTooLarge { .. } | Self::InsufficientScratchSpace { .. } => true,
            Self::Diagnosed { source, .. } => source.is_snapshot_capacity_failure(),
            Self::Finalization { primary, cleanup } => {
                primary.is_snapshot_capacity_failure() && !cleanup.is_systemic_resource_failure()
            }
            _ => false,
        }
    }

    pub fn is_ctx_owned_corruption(&self) -> bool {
        match self {
            Self::ProviderContentCorruption { .. } => false,
            Self::Diagnosed { diagnostic, source } => {
                !source.is_provider_corruption()
                    && matches!(diagnostic.artifact, SqliteArtifactKind::PrivateSourceCopy)
                    && matches!(
                        diagnostic.sqlite_primary_code,
                        Some(ffi::SQLITE_CORRUPT) | Some(ffi::SQLITE_NOTADB)
                    )
            }
            Self::Finalization { primary, cleanup } => {
                primary.is_ctx_owned_corruption() || cleanup.is_ctx_owned_corruption()
            }
            _ => false,
        }
    }

    pub fn is_provider_corruption(&self) -> bool {
        match self {
            Self::ProviderContentCorruption { source } => matches!(
                source.sqlite_codes().0,
                Some(ffi::SQLITE_CORRUPT) | Some(ffi::SQLITE_NOTADB)
            ),
            Self::Diagnosed { diagnostic, source } => {
                source.is_provider_corruption()
                    || (matches!(
                        diagnostic.artifact,
                        SqliteArtifactKind::ProviderDatabase
                            | SqliteArtifactKind::ProviderWal
                            | SqliteArtifactKind::ProviderSharedMemory
                    ) && matches!(
                        diagnostic.sqlite_primary_code,
                        Some(ffi::SQLITE_CORRUPT) | Some(ffi::SQLITE_NOTADB)
                    ))
            }
            Self::Finalization { primary, cleanup } => {
                primary.is_provider_corruption() || cleanup.is_provider_corruption()
            }
            _ => false,
        }
    }

    pub fn is_provider_path_unavailable(&self) -> bool {
        match self {
            Self::Io { source, .. }
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                true
            }
            Self::Diagnosed { source, .. } | Self::ProviderContentCorruption { source } => {
                source.is_provider_path_unavailable()
            }
            Self::Finalization { primary, cleanup } => {
                primary.is_provider_path_unavailable() || cleanup.is_provider_path_unavailable()
            }
            _ => false,
        }
    }

    pub fn is_busy_or_locked(&self) -> bool {
        matches!(
            self.sqlite_codes().0,
            Some(ffi::SQLITE_BUSY) | Some(ffi::SQLITE_LOCKED)
        )
    }

    pub fn is_operational_failure(&self) -> bool {
        matches!(
            self,
            Self::Io { .. }
                | Self::Sqlite { .. }
                | Self::ResourceUnavailable { .. }
                | Self::ScratchSqliteUnavailable { .. }
                | Self::ScratchIoUnavailable { .. }
                | Self::CleanupUnavailable { .. }
                | Self::SqliteControl { .. }
                | Self::ConnectionNotReadOnly
                | Self::ConnectionNotQueryOnly
                | Self::ConnectionIdentityMismatch
                | Self::SnapshotUnavailable { .. }
                | Self::SnapshotNotActive
        ) || matches!(self, Self::Diagnosed { source, .. } | Self::ProviderContentCorruption { source } if source.is_operational_failure())
            || matches!(self, Self::Finalization { primary, cleanup } if primary.is_operational_failure() || cleanup.is_operational_failure())
    }

    pub fn diagnostic(&self) -> Option<&SqliteFailureDiagnostic> {
        match self {
            Self::Diagnosed { diagnostic, .. } => Some(diagnostic),
            Self::ProviderContentCorruption { source } => source.diagnostic(),
            Self::Finalization { primary, cleanup } => {
                cleanup.diagnostic().or_else(|| primary.diagnostic())
            }
            _ => None,
        }
    }

    pub fn is_source_changed(&self) -> bool {
        matches!(self, Self::SourceChanged)
            || matches!(self, Self::Diagnosed { source, .. } | Self::ProviderContentCorruption { source } if source.is_source_changed())
            || matches!(self, Self::Finalization { primary, cleanup } if primary.is_source_changed() || cleanup.is_source_changed())
    }

    pub fn is_snapshot_unavailable(&self) -> bool {
        matches!(self, Self::SnapshotUnavailable { .. })
            || matches!(self, Self::Diagnosed { source, .. } | Self::ProviderContentCorruption { source } if source.is_snapshot_unavailable())
            || matches!(self, Self::Finalization { primary, cleanup } if primary.is_snapshot_unavailable() || cleanup.is_snapshot_unavailable())
    }

    pub fn with_diagnostic(
        self,
        phase: SqliteFailurePhase,
        artifact: SqliteArtifactKind,
        copied_pages: u64,
        copied_bytes: u64,
        cleanup: SqliteCleanupStatus,
    ) -> Self {
        let (primary, extended) = self.sqlite_codes();
        Self::Diagnosed {
            diagnostic: SqliteFailureDiagnostic {
                phase,
                artifact,
                sqlite_primary_code: primary,
                sqlite_extended_code: extended,
                copied_pages,
                copied_bytes,
                cleanup,
            },
            source: Box::new(self),
        }
    }

    pub fn with_cleanup_status(self, cleanup: SqliteCleanupStatus) -> Self {
        match self {
            Self::Diagnosed {
                mut diagnostic,
                source,
            } => {
                diagnostic.cleanup = diagnostic.cleanup.combine(cleanup);
                Self::Diagnosed { diagnostic, source }
            }
            Self::ProviderContentCorruption { source } => Self::ProviderContentCorruption {
                source: Box::new(source.with_cleanup_status(cleanup)),
            },
            Self::Finalization {
                primary,
                cleanup: cleanup_error,
            } => Self::Finalization {
                primary,
                cleanup: Box::new(cleanup_error.with_cleanup_status(cleanup)),
            },
            error => {
                let artifact = error.acquisition_artifact();
                error.with_diagnostic(SqliteFailurePhase::Cleanup, artifact, 0, 0, cleanup)
            }
        }
    }

    pub fn with_exact_provider_content_provenance(self) -> Self {
        if matches!(
            self.sqlite_codes().0,
            Some(ffi::SQLITE_CORRUPT) | Some(ffi::SQLITE_NOTADB)
        ) {
            Self::ProviderContentCorruption {
                source: Box::new(self),
            }
        } else {
            self
        }
    }

    fn sqlite_codes(&self) -> (Option<i32>, Option<i32>) {
        let extended = match self {
            Self::Sqlite {
                source: rusqlite::Error::SqliteFailure(error, _),
                ..
            }
            | Self::ScratchSqliteUnavailable {
                source: rusqlite::Error::SqliteFailure(error, _),
                ..
            } => Some(error.extended_code),
            Self::SqliteControl { code, .. } => Some(*code),
            Self::CleanupUnavailable { source, .. }
            | Self::ProviderContentCorruption { source } => return source.sqlite_codes(),
            Self::Finalization { primary, cleanup } => {
                let primary_codes = primary.sqlite_codes();
                return if primary_codes.0.is_some() {
                    primary_codes
                } else {
                    cleanup.sqlite_codes()
                };
            }
            Self::Diagnosed { source, .. } => return source.sqlite_codes(),
            _ => None,
        };
        (extended.map(|code| code & 0xff), extended)
    }

    pub fn private_scratch_sqlite(operation: &'static str, source: rusqlite::Error) -> Self {
        let resource_failure = matches!(
            &source,
            rusqlite::Error::SqliteFailure(error, _)
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DiskFull
                        | rusqlite::ErrorCode::OutOfMemory
                        | rusqlite::ErrorCode::SystemIoFailure
                        | rusqlite::ErrorCode::CannotOpen
                        | rusqlite::ErrorCode::PermissionDenied
                )
        );
        if resource_failure || operation.starts_with("closing") {
            Self::ScratchSqliteUnavailable { operation, source }
        } else {
            Self::Sqlite { operation, source }
        }
    }
}

impl SqliteSourceReadSnapshot {
    pub fn diagnose_provider_query_error(
        &self,
        operation: &'static str,
        source: rusqlite::Error,
        phase: SqliteFailurePhase,
    ) -> SqliteSourceAccessError {
        let artifact = if self._snapshot_directory.is_some() {
            SqliteArtifactKind::PrivateSourceCopy
        } else {
            SqliteArtifactKind::ProviderDatabase
        };
        let copied_bytes = self.copied_bytes;
        let error = SqliteSourceAccessError::Sqlite { operation, source }.with_diagnostic(
            phase,
            artifact,
            0,
            copied_bytes,
            SqliteCleanupStatus::NotRequired,
        );
        match self.policy {
            SqliteSourceSnapshotPolicy::ExactRevision
            | SqliteSourceSnapshotPolicy::PinnedReadOnlyWal => {
                error.with_exact_provider_content_provenance()
            }
            SqliteSourceSnapshotPolicy::StablePrivateCopy
            | SqliteSourceSnapshotPolicy::SelectivePrivateCopy(_) => error,
        }
    }
}

pub fn rusqlite_resource_failure(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(error, _) if sqlite_resource_code(error.extended_code)
    )
}

pub fn rusqlite_busy_or_locked(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(error, _)
            if matches!(
                error.extended_code & 0xff,
                ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED
            )
    )
}

fn sqlite_resource_code(code: i32) -> bool {
    matches!(
        code & 0xff,
        ffi::SQLITE_FULL
            | ffi::SQLITE_NOMEM
            | ffi::SQLITE_IOERR
            | ffi::SQLITE_CANTOPEN
            | ffi::SQLITE_PERM
            | ffi::SQLITE_READONLY
    )
}

pub fn resource_exhaustion_io_error(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        std::io::ErrorKind::OutOfMemory
            | std::io::ErrorKind::StorageFull
            | std::io::ErrorKind::QuotaExceeded
    ) {
        return true;
    }
    #[cfg(unix)]
    if error.raw_os_error().is_some_and(|code| {
        matches!(
            code,
            libc::EMFILE | libc::ENFILE | libc::ENOMEM | libc::ENOSPC | libc::EDQUOT
        )
    }) {
        return true;
    }
    // Win32 ERROR_TOO_MANY_OPEN_FILES, ERROR_NOT_ENOUGH_MEMORY,
    // ERROR_OUTOFMEMORY, and ERROR_DISK_FULL. Keep the numeric mapping local
    // so this crate does not need a Windows-only dependency for classification.
    #[cfg(windows)]
    if error
        .raw_os_error()
        .is_some_and(|code| matches!(code, 4 | 8 | 14 | 112))
    {
        return true;
    }
    false
}
