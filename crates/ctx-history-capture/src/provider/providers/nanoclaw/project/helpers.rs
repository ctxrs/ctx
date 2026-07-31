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

const NANOCLAW_SQLITE_COMPONENT_TOKEN_DOMAIN: &[u8] =
    b"ctx-nanoclaw-root-bound-sqlite-component-v2\0";
const NANOCLAW_SQLITE_HEADER_BYTES: usize = 100;
const NANOCLAW_SQLITE_WAL_HEADER_BYTES: usize = 32;
const NANOCLAW_SQLITE_WAL_FRAME_HEADER_BYTES: usize = 24;

pub(super) fn nanoclaw_root_bound_component_token(
    file: &OpenedProviderSourceFile,
    is_wal: bool,
) -> Result<[u8; 32]> {
    let prefix_len = usize::try_from(file.len().min(NANOCLAW_SQLITE_HEADER_BYTES as u64))
        .map_err(|_| CaptureError::SourceChangedDuringCapture)?;
    let prefix = file.read_exact_range(0, prefix_len, NANOCLAW_SQLITE_HEADER_BYTES)?;
    let mut hasher = Sha256::new();
    hasher.update(NANOCLAW_SQLITE_COMPONENT_TOKEN_DOMAIN);
    hasher.update(file.len().to_le_bytes());
    hasher.update(file.ordinary_file_token());
    hasher.update(&prefix);
    if is_wal {
        if let Some(frame_header) = nanoclaw_wal_last_frame_header(file, &prefix)? {
            hasher.update(frame_header);
        }
    }
    file.revalidate()?;
    Ok(hasher.finalize().into())
}

fn nanoclaw_wal_last_frame_header(
    file: &OpenedProviderSourceFile,
    prefix: &[u8],
) -> Result<Option<Vec<u8>>> {
    if prefix.len() < NANOCLAW_SQLITE_WAL_HEADER_BYTES {
        return Ok(None);
    }
    let raw_page_size = u32::from_be_bytes(prefix[8..12].try_into().map_err(|_| {
        CaptureError::InvalidPayload("invalid NanoClaw SQLite WAL page-size header".to_owned())
    })?);
    let page_size = match raw_page_size {
        1 => 65_536_u64,
        512..=65_536 if raw_page_size.is_power_of_two() => u64::from(raw_page_size),
        _ => return Ok(None),
    };
    let frame_size = page_size.saturating_add(NANOCLAW_SQLITE_WAL_FRAME_HEADER_BYTES as u64);
    let frames_bytes = file
        .len()
        .saturating_sub(NANOCLAW_SQLITE_WAL_HEADER_BYTES as u64);
    if frames_bytes < frame_size || !frames_bytes.is_multiple_of(frame_size) {
        return Ok(None);
    }
    file.read_exact_range(
        file.len().saturating_sub(frame_size),
        NANOCLAW_SQLITE_WAL_FRAME_HEADER_BYTES,
        NANOCLAW_SQLITE_WAL_FRAME_HEADER_BYTES,
    )
    .map(Some)
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
