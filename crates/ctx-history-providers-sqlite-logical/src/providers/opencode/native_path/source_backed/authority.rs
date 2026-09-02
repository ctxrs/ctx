use std::{ffi::OsString, path::Path};

use super::{OpenCodeSourceBackedResult, SQLITE_SOURCE_INVALID_REASON};
use crate::{
    common::io::ProviderSourceRoot,
    provider_sources::{retain_sqlite_source_directory_authority, SqliteSourceDirectoryAuthority},
    CaptureError,
};

#[derive(Debug)]
pub(super) struct OpenCodeRetainedSource {
    pub(super) source_root: ProviderSourceRoot,
    pub(super) sqlite_authority: SqliteSourceDirectoryAuthority,
    pub(super) database_leaf: OsString,
}

pub(super) fn retain_root_authorized_source(
    data_root: &Path,
    path: &Path,
) -> OpenCodeSourceBackedResult<OpenCodeRetainedSource> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let database_leaf =
        path.file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: SQLITE_SOURCE_INVALID_REASON,
            })?;
    let source_root = ProviderSourceRoot::open(parent)?;
    let source_directory = source_root.directory()?;
    let parent_handle = source_directory
        .try_clone_authority_handle()
        .map_err(CaptureError::from)?;
    let sqlite_authority =
        retain_sqlite_source_directory_authority(data_root, &parent_handle, parent)?;
    Ok(OpenCodeRetainedSource {
        source_root,
        sqlite_authority,
        database_leaf: database_leaf.to_os_string(),
    })
}
