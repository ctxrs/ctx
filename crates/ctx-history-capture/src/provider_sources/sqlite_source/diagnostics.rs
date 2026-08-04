use super::*;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteSourceComponent {
    RollbackJournal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteFailurePhase {
    SourceAcquisition,
    SourceValidation,
    OnlineBackup,
    BackupValidation,
    Schema,
    Projection,
    Cleanup,
}

impl SqliteFailurePhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SourceAcquisition => "source_acquisition",
            Self::SourceValidation => "source_validation",
            Self::OnlineBackup => "online_backup",
            Self::BackupValidation => "backup_validation",
            Self::Schema => "schema",
            Self::Projection => "projection",
            Self::Cleanup => "cleanup",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteArtifactKind {
    ProviderDatabase,
    ProviderWal,
    ProviderSharedMemory,
    PrivateSourceCopy,
    PrivateBackup,
    PrivateScratch,
}

impl SqliteArtifactKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderDatabase => "provider_database",
            Self::ProviderWal => "provider_wal",
            Self::ProviderSharedMemory => "provider_shm",
            Self::PrivateSourceCopy => "private_source_copy",
            Self::PrivateBackup => "private_backup",
            Self::PrivateScratch => "private_scratch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteRetryDecision {
    DoNotRetry,
    DoNotRetryCorrupt,
    RetryBusyOrLocked,
    RetrySourceTransition,
    RouteFatalResource,
}

impl SqliteRetryDecision {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DoNotRetry => "do_not_retry",
            Self::DoNotRetryCorrupt => "do_not_retry_corrupt",
            Self::RetryBusyOrLocked => "retry_busy_or_locked",
            Self::RetrySourceTransition => "retry_source_transition",
            Self::RouteFatalResource => "route_fatal_resource",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteCleanupStatus {
    NotRequired,
    Succeeded,
    Failed,
}

impl SqliteCleanupStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SqliteFailureDiagnostic {
    pub(crate) phase: SqliteFailurePhase,
    pub(crate) artifact: SqliteArtifactKind,
    pub(crate) sqlite_primary_code: Option<i32>,
    pub(crate) sqlite_extended_code: Option<i32>,
    pub(crate) copied_pages: u64,
    pub(crate) copied_bytes: u64,
    pub(crate) retry: SqliteRetryDecision,
    pub(crate) cleanup: SqliteCleanupStatus,
}

impl std::fmt::Display for SqliteFailureDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "sqlite_phase={} artifact_kind={} sqlite_primary_code={} sqlite_extended_code={} copied_pages={} copied_bytes={} retry_decision={} cleanup_status={}",
            self.phase.as_str(),
            self.artifact.as_str(),
            self.sqlite_primary_code.map_or_else(|| "none".to_owned(), |code| code.to_string()),
            self.sqlite_extended_code.map_or_else(|| "none".to_owned(), |code| code.to_string()),
            self.copied_pages,
            self.copied_bytes,
            self.retry.as_str(),
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
pub(crate) enum SqliteSourceAccessError {
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

#[derive(Debug)]
pub(crate) enum SqliteSourceProgressError<E> {
    Source(SqliteSourceAccessError),
    Progress(E),
}

impl<E> From<SqliteSourceAccessError> for SqliteSourceProgressError<E> {
    fn from(error: SqliteSourceAccessError) -> Self {
        Self::Source(error)
    }
}

impl SqliteSourceAccessError {
    pub(crate) fn acquisition_artifact(&self) -> SqliteArtifactKind {
        match self {
            Self::Diagnosed { source, .. } => source.acquisition_artifact(),
            Self::ScratchSqliteUnavailable { .. } | Self::ScratchIoUnavailable { .. } => {
                SqliteArtifactKind::PrivateSourceCopy
            }
            Self::Io { path, .. }
            | Self::ResourceUnavailable { path, .. }
            | Self::SnapshotTooLarge { path, .. } => {
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

    pub(crate) fn is_systemic_resource_failure(&self) -> bool {
        matches!(
            self,
            Self::ResourceUnavailable { .. }
                | Self::ScratchSqliteUnavailable { .. }
                | Self::ScratchIoUnavailable { .. }
                | Self::CleanupUnavailable { .. }
                | Self::SnapshotTooLarge { .. }
        ) || matches!(self, Self::Io { source, .. } if resource_exhaustion_io_error(source))
            || matches!(self, Self::Sqlite { source, .. } if rusqlite_resource_failure(source))
            || matches!(self, Self::SqliteControl { code, .. } if sqlite_resource_code(*code))
            || matches!(self, Self::Diagnosed { source, .. } if source.is_systemic_resource_failure())
    }

    pub(crate) fn is_ctx_owned_corruption(&self) -> bool {
        self.diagnostic().is_some_and(|diagnostic| {
            matches!(
                diagnostic.artifact,
                SqliteArtifactKind::PrivateSourceCopy | SqliteArtifactKind::PrivateBackup
            ) && matches!(
                diagnostic.sqlite_primary_code,
                Some(ffi::SQLITE_CORRUPT) | Some(ffi::SQLITE_NOTADB)
            )
        })
    }

    pub(crate) fn diagnostic(&self) -> Option<&SqliteFailureDiagnostic> {
        match self {
            Self::Diagnosed { diagnostic, .. } => Some(diagnostic),
            _ => None,
        }
    }

    pub(crate) fn is_source_changed(&self) -> bool {
        matches!(self, Self::SourceChanged)
            || matches!(self, Self::Diagnosed { source, .. } if source.is_source_changed())
    }

    pub(crate) fn with_diagnostic(
        self,
        phase: SqliteFailurePhase,
        artifact: SqliteArtifactKind,
        copied_pages: u64,
        copied_bytes: u64,
        cleanup: SqliteCleanupStatus,
    ) -> Self {
        let (primary, extended) = self.sqlite_codes();
        let retry = if self.is_systemic_resource_failure() {
            SqliteRetryDecision::RouteFatalResource
        } else if matches!(self, Self::SourceChanged) {
            SqliteRetryDecision::RetrySourceTransition
        } else if matches!(primary, Some(code) if code == ffi::SQLITE_CORRUPT || code == ffi::SQLITE_NOTADB)
        {
            SqliteRetryDecision::DoNotRetryCorrupt
        } else if matches!(primary, Some(code) if code == ffi::SQLITE_BUSY || code == ffi::SQLITE_LOCKED)
        {
            SqliteRetryDecision::RetryBusyOrLocked
        } else {
            SqliteRetryDecision::DoNotRetry
        };
        Self::Diagnosed {
            diagnostic: SqliteFailureDiagnostic {
                phase,
                artifact,
                sqlite_primary_code: primary,
                sqlite_extended_code: extended,
                copied_pages,
                copied_bytes,
                retry,
                cleanup,
            },
            source: Box::new(self),
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
            Self::CleanupUnavailable { source, .. } => return source.sqlite_codes(),
            Self::Diagnosed { source, .. } => return source.sqlite_codes(),
            _ => None,
        };
        (extended.map(|code| code & 0xff), extended)
    }

    pub(crate) fn private_scratch_sqlite(operation: &'static str, source: rusqlite::Error) -> Self {
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

pub(crate) fn rusqlite_resource_failure(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(error, _) if sqlite_resource_code(error.extended_code)
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

pub(crate) fn resource_exhaustion_io_error(error: &std::io::Error) -> bool {
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
