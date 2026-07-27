use super::*;

pub(super) fn valid_identity_string(value: &str, maximum_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum_bytes && !value.chars().any(char::is_control)
}

pub(super) fn valid_metadata_string(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

pub(super) fn sha256_hex(domain: &[u8], bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    digest_to_hex(hasher.finalize())
}

pub(super) fn digest_to_hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) trait IntoContinueSourceError {
    fn into_continue_source_error(
        self,
        path: &Path,
        operation: &'static str,
    ) -> ContinueNativePathError;
}

impl IntoContinueSourceError for io::Error {
    fn into_continue_source_error(
        self,
        path: &Path,
        operation: &'static str,
    ) -> ContinueNativePathError {
        source_io(path, operation, self)
    }
}

impl IntoContinueSourceError for CaptureError {
    fn into_continue_source_error(
        self,
        path: &Path,
        operation: &'static str,
    ) -> ContinueNativePathError {
        capture_source_error(path, operation, self)
    }
}

pub(super) fn source_access(
    path: &Path,
    error: impl IntoContinueSourceError,
) -> ContinueNativePathError {
    error.into_continue_source_error(path, "access Continue source")
}

pub(super) fn capture_source_error(
    path: &Path,
    operation: &'static str,
    error: CaptureError,
) -> ContinueNativePathError {
    match error {
        CaptureError::Io(error) => source_io(path, operation, error),
        CaptureError::SystemIo { source, .. } => source_io(path, operation, source),
        CaptureError::SourceChangedDuringCapture => ContinueNativePathError::SourceChanged {
            path: path.to_path_buf(),
        },
        error => ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: error.to_string(),
        },
    }
}

pub(super) fn source_io(
    path: &Path,
    operation: &'static str,
    error: io::Error,
) -> ContinueNativePathError {
    ContinueNativePathError::SourceIo {
        path: path.to_path_buf(),
        operation,
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
        message: error.to_string(),
    }
}

pub(super) fn os_order_key(name: &OsStr) -> Vec<u8> {
    encode_os_string(name).unwrap_or_default()
}

pub(super) fn encode_path(path: &Path) -> Option<Vec<u8>> {
    encode_os_string(path.as_os_str())
}

#[cfg(unix)]
pub(super) fn encode_os_string(value: &OsStr) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;

    Some(value.as_bytes().to_vec())
}

#[cfg(unix)]
pub(super) fn decode_path(value: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    Some(PathBuf::from(OsString::from_vec(value)))
}

#[cfg(windows)]
pub(super) fn encode_os_string(value: &OsStr) -> Option<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;

    Some(
        value
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )
}

#[cfg(windows)]
pub(super) fn decode_path(value: Vec<u8>) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    if !value.len().is_multiple_of(2) {
        return None;
    }
    let units = value
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    Some(PathBuf::from(OsString::from_wide(&units)))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn encode_os_string(value: &OsStr) -> Option<Vec<u8>> {
    value.to_str().map(|value| value.as_bytes().to_vec())
}

#[cfg(not(any(unix, windows)))]
pub(super) fn decode_path(value: Vec<u8>) -> Option<PathBuf> {
    String::from_utf8(value).ok().map(PathBuf::from)
}
