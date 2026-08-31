use std::{
    ffi::OsStr,
    io,
    path::{Component, Path, PathBuf},
};

use crate::{Result, SourceIoError};

use super::AuthorityOpenError;

pub(super) fn ensure_absolute_traversal_free(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid_path(
            path,
            "provider source authority paths must be absolute and traversal-free",
        ));
    }
    Ok(())
}

pub(super) fn validate_relative_path(path: &Path) -> Result<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(invalid_path(
            path,
            "provider source descendants must be traversal-free relative paths",
        ));
    }
    Ok(())
}

pub(super) fn validate_child_name(name: &OsStr, path: &Path) -> Result<()> {
    if name.is_empty()
        || name == OsStr::new(".")
        || name == OsStr::new("..")
        || Path::new(name).components().count() != 1
        || !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(invalid_path(
            path,
            "provider source child names must be single normal components",
        ));
    }
    Ok(())
}

pub(super) fn map_open_error(path: &Path, error: AuthorityOpenError) -> SourceIoError {
    match error {
        AuthorityOpenError::Io(source) => source.into(),
        AuthorityOpenError::SystemIo { operation, source } => {
            provider_source_system_io(path, operation, source)
        }
        AuthorityOpenError::Rejected(reason) => invalid_path(path, reason),
    }
}

pub(super) fn map_changed_open_error(path: &Path, error: AuthorityOpenError) -> SourceIoError {
    match error {
        AuthorityOpenError::Io(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::InvalidData
                    | io::ErrorKind::PermissionDenied
            ) =>
        {
            changed_path(path)
        }
        AuthorityOpenError::SystemIo { source, .. }
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::InvalidData
                    | io::ErrorKind::PermissionDenied
            ) =>
        {
            changed_path(path)
        }
        AuthorityOpenError::Rejected(_) => changed_path(path),
        AuthorityOpenError::Io(source) => source.into(),
        AuthorityOpenError::SystemIo { operation, source } => {
            provider_source_system_io(path, operation, source)
        }
    }
}

#[derive(Debug)]
struct ProviderSourceIoContext {
    path: PathBuf,
    source: io::Error,
}

impl std::fmt::Display for ProviderSourceIoContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "provider source path {:?}: {}",
            self.path, self.source
        )
    }
}

impl std::error::Error for ProviderSourceIoContext {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn provider_source_system_io(
    path: &Path,
    operation: &'static str,
    source: io::Error,
) -> SourceIoError {
    let kind = source.kind();
    SourceIoError::SystemIo {
        operation,
        source: io::Error::new(
            kind,
            ProviderSourceIoContext {
                path: path.to_path_buf(),
                source,
            },
        ),
    }
}

pub(super) fn provider_source_io_result<T>(
    path: &Path,
    operation: &'static str,
    result: io::Result<T>,
) -> Result<T> {
    result.map_err(|source| provider_source_system_io(path, operation, source))
}

pub(super) fn invalid_path(path: &Path, reason: &'static str) -> SourceIoError {
    SourceIoError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason,
    }
}

pub(super) fn changed_path(path: &Path) -> SourceIoError {
    SourceIoError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "provider source changed while its authority handle was retained",
    }
}
