use super::*;

pub(super) fn nanoclaw_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

pub(super) fn nanoclaw_open_optional_root_file(
    root: &ProviderSourceRoot,
    relative_path: &Path,
) -> Result<Option<OpenedProviderSourceFile>> {
    match root.open_file(relative_path) {
        Ok(opened) => Ok(Some(opened)),
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn nanoclaw_root_bound_component_token(metadata: &fs::Metadata) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-nanoclaw-root-bound-component-v1\0");
    hasher.update(metadata.len().to_le_bytes());
    hasher.update([u8::from(metadata.permissions().readonly())]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        hasher.update(metadata.dev().to_le_bytes());
        hasher.update(metadata.ino().to_le_bytes());
        hasher.update(metadata.mtime().to_le_bytes());
        hasher.update(metadata.mtime_nsec().to_le_bytes());
        hasher.update(metadata.ctime().to_le_bytes());
        hasher.update(metadata.ctime_nsec().to_le_bytes());
    }
    #[cfg(not(unix))]
    {
        let (seconds, nanos) = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map_or((0, 0), |duration| {
                (duration.as_secs(), duration.subsec_nanos())
            });
        hasher.update(seconds.to_le_bytes());
        hasher.update(nanos.to_le_bytes());
    }
    hasher.finalize().into()
}

pub(super) fn nanoclaw_sqlite_access_error(
    path: &Path,
    error: SqliteSourceAccessError,
) -> CaptureError {
    if matches!(error, SqliteSourceAccessError::SourceChanged) {
        return CaptureError::SourceChangedDuringCapture;
    }
    CaptureError::ProviderSource {
        provider: "nanoclaw",
        path: path.to_path_buf(),
        kind: ProviderSourceFailureKind::SourceDatabase,
        detail: error.to_string(),
    }
}

pub(super) fn nanoclaw_hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

pub(super) fn nanoclaw_hash_u64(hasher: &mut Sha256, value: u64) {
    nanoclaw_hash_bytes(hasher, &value.to_be_bytes());
}

pub(super) fn nanoclaw_hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            nanoclaw_hash_bytes(hasher, &[1]);
            nanoclaw_hash_u64(hasher, value);
        }
        None => nanoclaw_hash_bytes(hasher, &[0]),
    }
}

pub(super) fn nanoclaw_hash_optional_file(
    hasher: &mut Sha256,
    value: Option<&NanoClawFrozenFileMetadata>,
) {
    match value {
        Some(value) => {
            nanoclaw_hash_bytes(hasher, &[1]);
            value.update_hash(hasher);
        }
        None => nanoclaw_hash_bytes(hasher, &[0]),
    }
}

pub(super) fn nanoclaw_hash_optional_sqlite(
    hasher: &mut Sha256,
    snapshot: Option<&NanoClawSqliteSnapshot>,
) {
    match snapshot {
        Some(snapshot) => {
            nanoclaw_hash_bytes(hasher, &[1]);
            snapshot.update_hash(hasher);
        }
        None => nanoclaw_hash_bytes(hasher, &[0]),
    }
}

pub(super) fn nanoclaw_hex(value: &[u8]) -> String {
    use std::fmt::Write;

    let mut encoded = String::with_capacity(value.len().saturating_mul(2));
    for byte in value {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}
