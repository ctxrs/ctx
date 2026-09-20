//! Pinned read-only access to a Devin database.
//!
//! The file is opened through a retained directory authority and read from a
//! snapshot, so a session Devin writes mid-import cannot change what this
//! import publishes. Devin writes with WAL enabled and may be running, which
//! is exactly the case the shared snapshot machinery exists for.
//!
//! Unlike the other providers in this pack, no private scratch database is
//! taken: Devin's own `UNIQUE(session_id, node_id)` index already yields every
//! ordered read the projection needs, so there is no sort to materialize.

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    time::Duration,
};

use rusqlite::{limits::Limit, Connection};

use crate::{
    common::io::{open_provider_source_file, ProviderSourceRoot},
    provider::source_backed::SourceBackedRouteError,
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteFailurePhase, SqliteSourceAccessError, SqliteSourceDirectoryAuthority,
        SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    CaptureError, Result, DEVIN_CLI_SESSIONS_SQLITE_SOURCE_FORMAT,
};
use ctx_history_source_sqlite::MAX_PROVIDER_SQLITE_VALUE_BYTES;

use super::source_backed::{
    devin_route_error, DevinResult, DevinSourceBackedError, DEVIN_SOURCE_PATH_REASONS,
};

const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

#[derive(Debug)]
pub(super) struct DevinSqliteDatabase {
    root: ProviderSourceRoot,
    authority: SqliteSourceDirectoryAuthority,
    snapshot: SqliteSourceReadSnapshot,
}

impl DevinSqliteDatabase {
    pub(super) fn open(data_root: &Path, path: &Path) -> DevinResult<Self> {
        let parent_path = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let database_name = crate::sqlite_common::database_leaf(path, &DEVIN_SOURCE_PATH_REASONS)?;
        let root = ProviderSourceRoot::open(parent_path)?;
        let parent = root.directory()?;
        let authority_handle = parent.try_clone_authority_handle()?;
        let authority =
            retain_sqlite_source_directory_authority(data_root, &authority_handle, parent_path)?;
        let snapshot = open_root_handle_sqlite_source_snapshot(&authority, database_name)?;
        let configure = (|| {
            snapshot.revalidate()?;
            parent.revalidate()?;
            root.revalidate()?;
            let connection = snapshot.connection()?;
            let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
                CaptureError::SystemInvariant("Devin SQLite value limit is invalid")
            })?;
            connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(|source| {
                    snapshot.diagnose_provider_query_error(
                        "setting the private Devin SQLite busy timeout",
                        source,
                        SqliteFailurePhase::SourceValidation,
                    )
                })?;
            Ok(())
        })();
        if let Err(error) = configure {
            return Err(abort_devin_snapshot(snapshot, error));
        }
        Ok(Self {
            root,
            authority,
            snapshot,
        })
    }

    pub(super) fn connection(&self) -> DevinResult<&Connection> {
        Ok(self.snapshot.connection()?)
    }

    pub(super) fn evidence(&self) -> &SqliteSourceEvidence {
        self.snapshot.evidence()
    }

    pub(super) fn revalidate(&self) -> DevinResult<()> {
        self.snapshot.revalidate()?;
        self.root.revalidate()?;
        Ok(())
    }

    pub(super) fn terminal_revalidator(
        &self,
    ) -> Box<dyn Fn() -> std::result::Result<(), SqliteSourceAccessError> + Send + Sync + 'static>
    {
        self.snapshot.terminal_revalidator()
    }

    pub(super) fn sqlite_authority(&self) -> SqliteSourceDirectoryAuthority {
        self.authority.clone()
    }

    /// Re-reads a SQLite error against the snapshot so the failure is
    /// attributed to the right phase rather than surfacing as a bare driver
    /// error.
    pub(super) fn diagnose_provider_query_error(
        &self,
        error: DevinSourceBackedError,
        phase: SqliteFailurePhase,
    ) -> DevinSourceBackedError {
        let source = match error {
            DevinSourceBackedError::Sqlite(source)
            | DevinSourceBackedError::Capture(CaptureError::Sqlite(source)) => source,
            error => return error,
        };
        self.snapshot
            .diagnose_provider_query_error(
                "querying the private Devin provider copy",
                source,
                phase,
            )
            .into()
    }

    pub(super) fn abort(self, primary: SourceBackedRouteError) -> SourceBackedRouteError {
        match self.snapshot.abort() {
            Ok(()) => primary,
            Err(cleanup) => {
                crate::provider::source_backed::combine_primary_and_cleanup_route_errors(
                    primary,
                    devin_route_error(cleanup.into()),
                )
            }
        }
    }

    pub(super) fn finish(self) -> DevinResult<SqliteSourceEvidence> {
        let Self { root, snapshot, .. } = self;
        let evidence = snapshot.finish()?;
        root.revalidate()?;
        Ok(evidence)
    }
}

fn abort_devin_snapshot(
    snapshot: SqliteSourceReadSnapshot,
    primary: DevinSourceBackedError,
) -> DevinSourceBackedError {
    match snapshot.abort() {
        Ok(()) => primary,
        Err(cleanup) => DevinSourceBackedError::Route(
            crate::provider::source_backed::combine_primary_and_cleanup_route_errors(
                devin_route_error(primary),
                devin_route_error(cleanup.into()),
            ),
        ),
    }
}

/// Refuses anything that is not a plain SQLite file at the exact path.
///
/// A symlink is rejected rather than followed: the retained authority is over
/// the parent directory, and a link could point outside it.
pub(super) fn require_devin_sqlite_format(
    source_path: &Path,
    source_format: &str,
) -> DevinResult<()> {
    if source_format != DEVIN_CLI_SESSIONS_SQLITE_SOURCE_FORMAT {
        return Err(DevinSourceBackedError::UnsupportedFormat(
            "only the Devin sessions SQLite store is importable",
        ));
    }
    let metadata = fs::symlink_metadata(source_path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(crate::sqlite_common::invalid_database_leaf(
            source_path,
            &DEVIN_SOURCE_PATH_REASONS,
        )
        .into());
    }
    let opened = open_provider_source_file(source_path)?;
    let mut file = opened.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0_u8; SQLITE_HEADER.len()];
    let read = file.read(&mut header)?;
    opened.revalidate()?;
    if read != SQLITE_HEADER.len() || &header != SQLITE_HEADER {
        return Err(DevinSourceBackedError::UnsupportedFormat(
            "the Devin source is not a SQLite database",
        ));
    }
    Ok(())
}

/// Normalizes a route path without resolving symlinks.
pub(super) fn absolute_devin_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}
