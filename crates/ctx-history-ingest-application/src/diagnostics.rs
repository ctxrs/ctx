use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
};

use ctx_history_refresh::ExplicitSourcePathMissing;

/// A user-supplied import path was absent at the authoritative admission
/// boundary.
#[derive(Debug)]
pub struct ImportPathNotFound {
    path: PathBuf,
    source: anyhow::Error,
}

/// Final-host marker for an explicit path that disappeared while Core was
/// revalidating refresh admission.
#[derive(Debug)]
pub struct ImportPathMissingDuringRefresh;

impl fmt::Display for ImportPathMissingDuringRefresh {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("explicit import path disappeared during refresh admission")
    }
}

impl ImportPathNotFound {
    pub(crate) fn new(path: &Path, source: anyhow::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            source,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Display for ImportPathNotFound {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "import path does not exist: {}",
            self.path.display()
        )
    }
}

impl Error for ImportPathNotFound {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

pub(crate) fn classify_import_path_admission_error(
    path: &Path,
    source: anyhow::Error,
) -> anyhow::Error {
    classify_import_path_marker(path, path, source)
}

pub(crate) fn classify_owned_import_path_admission_error(
    requested_path: &Path,
    owned_path: &Path,
    source: anyhow::Error,
) -> anyhow::Error {
    classify_import_path_marker(requested_path, owned_path, source)
}

/// Classifies an application-owned operation that directly targeted the exact
/// requested path. The caller supplies the original contextual error so the
/// diagnostic does not replace its operation or OS source.
pub(crate) fn classify_exact_import_path_operation_error(
    requested_path: &Path,
    source: anyhow::Error,
) -> anyhow::Error {
    if source
        .downcast_ref::<io::Error>()
        .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
    {
        anyhow::Error::new(ImportPathNotFound::new(requested_path, source))
    } else {
        source
    }
}

pub(crate) fn classify_import_path_refresh_error(
    requested_path: &Path,
    source: anyhow::Error,
) -> anyhow::Error {
    if source
        .downcast_ref::<ImportPathMissingDuringRefresh>()
        .is_some()
    {
        anyhow::Error::new(ImportPathNotFound::new(requested_path, source))
    } else {
        source
    }
}

fn classify_import_path_marker(
    requested_path: &Path,
    owned_path: &Path,
    source: anyhow::Error,
) -> anyhow::Error {
    if source
        .downcast_ref::<ExplicitSourcePathMissing>()
        .is_some_and(|missing| missing.path() == owned_path)
    {
        anyhow::Error::new(ImportPathNotFound::new(requested_path, source))
    } else {
        source
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use anyhow::Context as _;
    use ctx_history_refresh::explicit_source_path_symlink_metadata;

    use super::*;

    #[test]
    fn lower_layer_missing_marker_becomes_typed_without_discarding_its_source() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing.jsonl");
        let source = explicit_source_path_symlink_metadata(&path)
            .context("approve explicit source path")
            .unwrap_err();

        let error = classify_import_path_admission_error(&path, source);
        let diagnostic = error.downcast_ref::<ImportPathNotFound>().unwrap();

        assert_eq!(diagnostic.path(), path);
        assert_eq!(
            diagnostic.to_string(),
            format!("import path does not exist: {}", path.display())
        );
        let marker = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<ExplicitSourcePathMissing>())
            .unwrap();
        assert_eq!(marker.path(), path);
        assert_eq!(marker.source_error().kind(), io::ErrorKind::NotFound);
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<io::Error>()
                .is_some_and(|source| source.kind() == io::ErrorKind::NotFound)
        }));
    }

    #[test]
    fn unmarked_not_found_keeps_its_original_context() {
        let source = Err::<(), _>(io::Error::new(io::ErrorKind::NotFound, "missing data root"))
            .context("open unrelated data root")
            .unwrap_err();

        let error = classify_import_path_admission_error(Path::new("history.jsonl"), source);

        assert!(!error.is::<ImportPathNotFound>());
        assert_eq!(error.to_string(), "open unrelated data root");
    }

    #[test]
    fn missing_marker_for_another_path_is_not_reclassified() {
        let temp = tempfile::tempdir().unwrap();
        let requested = temp.path().join("requested.jsonl");
        let unrelated = temp.path().join("unrelated.jsonl");
        let source = explicit_source_path_symlink_metadata(&unrelated).unwrap_err();

        let error = classify_import_path_admission_error(&requested, source);

        assert!(!error.is::<ImportPathNotFound>());
        assert_eq!(
            error
                .downcast_ref::<ExplicitSourcePathMissing>()
                .unwrap()
                .path(),
            unrelated
        );
    }

    #[test]
    fn owned_missing_marker_reports_the_original_requested_path() {
        let temp = tempfile::tempdir().unwrap();
        let requested = temp.path().join("relative-request.jsonl");
        let owned = temp.path().join("canonical-source.jsonl");
        let source = explicit_source_path_symlink_metadata(&owned).unwrap_err();

        let error = classify_owned_import_path_admission_error(&requested, &owned, source);
        let diagnostic = error.downcast_ref::<ImportPathNotFound>().unwrap();

        assert_eq!(diagnostic.path(), requested);
    }

    #[test]
    fn admission_errors_other_than_missing_keep_their_original_context() {
        let source = Err::<(), _>(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
            .context("approve explicit source path")
            .unwrap_err();

        let error = classify_import_path_admission_error(Path::new("private.jsonl"), source);

        assert!(!error.is::<ImportPathNotFound>());
        assert_eq!(error.to_string(), "approve explicit source path");
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<io::Error>()
                .is_some_and(|source| source.kind() == io::ErrorKind::PermissionDenied)
        }));
    }

    #[test]
    fn exact_application_operation_retains_its_context_and_io_source() {
        let path = Path::new("missing-plugins");
        let source = Err::<(), _>(io::Error::new(io::ErrorKind::NotFound, "gone"))
            .context("read explicit plugin root")
            .unwrap_err();

        let error = classify_exact_import_path_operation_error(path, source);
        let diagnostic = error.downcast_ref::<ImportPathNotFound>().unwrap();

        assert_eq!(diagnostic.path(), path);
        assert!(error
            .chain()
            .any(|cause| cause.to_string() == "read explicit plugin root"));
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<io::Error>()
                .is_some_and(|source| source.kind() == io::ErrorKind::NotFound)
        }));
    }

    #[test]
    fn late_refresh_marker_reports_the_original_requested_path() {
        let requested = Path::new("relative-request.jsonl");
        let source =
            anyhow::anyhow!("daemon terminal detail").context(ImportPathMissingDuringRefresh);

        let error = classify_import_path_refresh_error(requested, source);
        let diagnostic = error.downcast_ref::<ImportPathNotFound>().unwrap();

        assert_eq!(diagnostic.path(), requested);
        assert!(error.chain().any(|cause| {
            cause.to_string() == "explicit import path disappeared during refresh admission"
        }));
        assert!(error
            .chain()
            .any(|cause| cause.to_string() == "daemon terminal detail"));
    }
}
