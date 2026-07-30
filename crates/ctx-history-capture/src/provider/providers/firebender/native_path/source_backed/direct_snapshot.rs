use std::{
    ffi::{OsStr, OsString},
    io,
    path::Path,
};

use rusqlite::Connection;

use super::{FirebenderSourceBackedError, FirebenderSourceBackedResult};
use crate::{
    common::io::{OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot},
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceDirectoryAuthority, SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    CaptureError,
};

#[derive(Debug)]
pub(super) struct MissingLeafFence {
    root: ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    leaf: OsString,
}

impl MissingLeafFence {
    pub(super) fn fingerprint(&self) -> [u8; 32] {
        self.root.authority_fingerprint()
    }

    pub(super) fn revalidate(&self) -> bool {
        if self.root.revalidate().is_err() || self.directory.revalidate().is_err() {
            return false;
        }
        let missing = matches!(
            self.directory.open_child(&self.leaf),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound
        );
        missing && self.directory.revalidate().is_ok() && self.root.revalidate().is_ok()
    }
}

pub(super) enum OpenDatabaseLeaf {
    Present(Box<OpenedSnapshot>),
    Missing(MissingLeafFence),
}

pub(super) fn open_database_leaf(
    data_root: &Path,
    path: &Path,
) -> FirebenderSourceBackedResult<OpenDatabaseLeaf> {
    let parent = database_parent(path)?;
    let leaf = database_leaf(path)?;
    let root = ProviderSourceRoot::open(parent)?;
    let directory = root.directory()?;
    root.revalidate()?;
    directory.revalidate()?;
    match directory.open_child(leaf) {
        Ok(OpenedProviderSourcePath::File(file)) => {
            file.revalidate()?;
            directory.revalidate()?;
            root.revalidate()?;
            open_snapshot_from_authority(data_root, parent, leaf, root, directory)
                .map(Box::new)
                .map(OpenDatabaseLeaf::Present)
        }
        Ok(OpenedProviderSourcePath::Directory(_)) => Err(invalid_database_leaf(path).into()),
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            directory.revalidate()?;
            root.revalidate()?;
            Ok(OpenDatabaseLeaf::Missing(MissingLeafFence {
                root,
                directory,
                leaf: leaf.to_os_string(),
            }))
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) struct OpenedSnapshot {
    root: ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    authority: SqliteSourceDirectoryAuthority,
    snapshot: Option<SqliteSourceReadSnapshot>,
}

impl OpenedSnapshot {
    pub(super) fn connection(&self) -> FirebenderSourceBackedResult<&Connection> {
        self.snapshot
            .as_ref()
            .ok_or(FirebenderSourceBackedError::Capture(
                CaptureError::SystemInvariant("Firebender SQLite snapshot is inactive"),
            ))?
            .connection()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()).into())
    }

    pub(super) fn evidence(&self) -> FirebenderSourceBackedResult<&SqliteSourceEvidence> {
        Ok(self
            .snapshot
            .as_ref()
            .ok_or(FirebenderSourceBackedError::Capture(
                CaptureError::SystemInvariant("Firebender SQLite snapshot is inactive"),
            ))?
            .evidence())
    }

    pub(super) fn terminal_revalidator(
        &self,
    ) -> FirebenderSourceBackedResult<
        Box<
            dyn Fn() -> Result<(), crate::provider_sources::SqliteSourceAccessError>
                + Send
                + Sync
                + 'static,
        >,
    > {
        Ok(self
            .snapshot
            .as_ref()
            .ok_or(FirebenderSourceBackedError::Capture(
                CaptureError::SystemInvariant("Firebender SQLite snapshot is inactive"),
            ))?
            .terminal_revalidator())
    }

    pub(super) fn sqlite_authority(&self) -> SqliteSourceDirectoryAuthority {
        self.authority.clone()
    }

    pub(super) fn revalidate(&self) -> FirebenderSourceBackedResult<()> {
        self.snapshot
            .as_ref()
            .ok_or(FirebenderSourceBackedError::Capture(
                CaptureError::SystemInvariant("Firebender SQLite snapshot is inactive"),
            ))?
            .revalidate()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        self.directory.revalidate()?;
        self.root.revalidate()?;
        Ok(())
    }

    pub(super) fn finish(mut self) -> FirebenderSourceBackedResult<SqliteSourceEvidence> {
        let snapshot = self
            .snapshot
            .take()
            .ok_or(FirebenderSourceBackedError::Capture(
                CaptureError::SystemInvariant("Firebender SQLite snapshot is inactive"),
            ))?;
        let evidence = snapshot
            .finish()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        self.directory.revalidate()?;
        self.root.revalidate()?;
        Ok(evidence)
    }
}

fn open_snapshot_from_authority(
    data_root: &Path,
    parent: &Path,
    leaf: &OsStr,
    root: ProviderSourceRoot,
    directory: ProviderSourceDirectory,
) -> FirebenderSourceBackedResult<OpenedSnapshot> {
    let handle = directory
        .try_clone_authority_handle()
        .map_err(CaptureError::Io)?;
    let authority = retain_sqlite_source_directory_authority(data_root, &handle, parent)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let snapshot = open_root_handle_sqlite_source_snapshot(&authority, leaf)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    snapshot
        .revalidate()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    directory.revalidate()?;
    root.revalidate()?;
    Ok(OpenedSnapshot {
        root,
        directory,
        authority,
        snapshot: Some(snapshot),
    })
}

fn database_parent(path: &Path) -> FirebenderSourceBackedResult<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Firebender SQLite source must have a parent directory",
            }
            .into()
        })
}

fn database_leaf(path: &Path) -> FirebenderSourceBackedResult<&OsStr> {
    path.file_name().ok_or_else(|| {
        CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Firebender SQLite source must have a database leaf name",
        }
        .into()
    })
}

fn invalid_database_leaf(path: &Path) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "Firebender SQLite source must be a regular non-symlink file",
    }
}
