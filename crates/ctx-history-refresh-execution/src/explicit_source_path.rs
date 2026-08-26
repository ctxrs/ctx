use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use anyhow::Result;

/// An authoritative filesystem operation found that the requested explicit
/// source path is absent.
#[derive(Debug)]
pub struct ExplicitSourcePathMissing {
    path: PathBuf,
    source: io::Error,
}

impl ExplicitSourcePathMissing {
    fn new(path: &Path, source: io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            source,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn source_error(&self) -> &io::Error {
        &self.source
    }
}

impl fmt::Display for ExplicitSourcePathMissing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "requested explicit source path is missing: {}",
            self.path.display()
        )
    }
}

impl Error for ExplicitSourcePathMissing {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

fn classify_explicit_source_path_io<T>(path: &Path, result: io::Result<T>) -> Result<T> {
    result.map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            anyhow::Error::new(ExplicitSourcePathMissing::new(path, source))
        } else {
            source.into()
        }
    })
}

/// Reads the requested path entry without following its final symlink.
pub fn explicit_source_path_symlink_metadata(path: &Path) -> Result<fs::Metadata> {
    classify_explicit_source_path_io(path, fs::symlink_metadata(path))
}

/// Returns whether no-follow metadata identifies an explicit source root as a
/// symlink or, on Windows, any reparse point.
pub fn explicit_source_path_is_symlink_or_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    #[cfg(not(windows))]
    {
        false
    }
}

/// Reads the requested path target, following a final symlink when present.
pub fn explicit_source_path_metadata(path: &Path) -> Result<fs::Metadata> {
    classify_explicit_source_path_io(path, fs::metadata(path))
}

/// Canonicalizes the exact requested path and classifies only that operation's
/// own `NotFound`; callers must not replace its source with a later probe.
pub(crate) fn canonicalize_explicit_source_path(path: &Path) -> Result<PathBuf> {
    classify_explicit_source_path_io(path, fs::canonicalize(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_missing_observation_retains_the_path_and_io_source() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing-history.jsonl");

        let error = explicit_source_path_symlink_metadata(&path).unwrap_err();
        let missing = error.downcast_ref::<ExplicitSourcePathMissing>().unwrap();

        assert_eq!(missing.path(), path);
        assert_eq!(missing.source_error().kind(), io::ErrorKind::NotFound);
        assert_eq!(
            error
                .chain()
                .filter_map(|cause| cause.downcast_ref::<io::Error>())
                .map(io::Error::kind)
                .collect::<Vec<_>>(),
            vec![io::ErrorKind::NotFound]
        );
    }

    #[test]
    fn non_missing_io_is_not_retyped() {
        let path = Path::new("history.jsonl");
        let error = classify_explicit_source_path_io::<()>(
            path,
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
        )
        .unwrap_err();

        assert!(!error.is::<ExplicitSourcePathMissing>());
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn canonicalize_missing_retains_the_canonicalize_io_source() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing-history.jsonl");

        let error = canonicalize_explicit_source_path(&path).unwrap_err();
        let missing = error.downcast_ref::<ExplicitSourcePathMissing>().unwrap();

        assert_eq!(missing.path(), path);
        assert_eq!(missing.source_error().kind(), io::ErrorKind::NotFound);
    }
}
