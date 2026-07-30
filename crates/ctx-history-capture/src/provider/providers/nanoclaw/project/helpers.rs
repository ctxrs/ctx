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

/// A root-bound view that freezes only the compound coordinates addressed by
/// one hydration batch. Unlike capture discovery, this does not inventory or
/// acquire every component database in the project.
pub(in crate::provider::providers::nanoclaw) struct NanoClawSelectedProject {
    root: ProviderSourceRoot,
    sessions: ProviderSourceDirectory,
    central_path: PathBuf,
    central_snapshot: NanoClawSqliteSnapshot,
    central_route: NanoClawRootBoundDatabase,
    central_opened: NanoClawOpenedSqliteFamily,
    central_guard: Option<SqliteSourceReadSnapshot>,
}

pub(in crate::provider::providers::nanoclaw) struct NanoClawSelectedDatabaseRead {
    snapshot: NanoClawProjectDatabaseSnapshot,
    read: NanoClawDatabaseRead,
}

impl NanoClawSelectedProject {
    pub(in crate::provider::providers::nanoclaw) fn open(
        data_root: &Path,
        path: &Path,
    ) -> Result<Self> {
        let requested_root = nanoclaw_requested_project_root(path)?;
        let root = ProviderSourceRoot::open(&requested_root)?;
        let sessions = root.open_directory(Path::new("data/v2-sessions"))?;
        sessions.revalidate()?;

        let central_relative = PathBuf::from("data/v2.db");
        let central_route =
            NanoClawRootBoundDatabase::bind(data_root, &root, central_relative.clone())?;
        let central_path = central_route.display_path();
        let central_opened = NanoClawOpenedSqliteFamily::open(&root, &central_relative)?;
        let central_snapshot = central_opened.snapshot()?;
        let central_guard = central_route.open_snapshot()?;
        central_opened.revalidate()?;
        central_route.revalidate_authority()?;
        sessions.revalidate()?;
        root.revalidate()?;
        Ok(Self {
            root,
            sessions,
            central_path,
            central_snapshot,
            central_route,
            central_opened,
            central_guard: Some(central_guard),
        })
    }

    pub(in crate::provider::providers::nanoclaw) fn connection(&self) -> Result<&Connection> {
        self.central_guard
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw selected central SQLite guard is no longer active",
            ))?
            .connection()
            .map_err(|error| nanoclaw_sqlite_access_error(&self.central_path, error))
    }

    pub(in crate::provider::providers::nanoclaw) fn open_component(
        &self,
        data_root: &Path,
        agent_group_id: &str,
        session_id: &str,
        source: NanoClawMessageSource,
    ) -> Result<Option<NanoClawSelectedDatabaseRead>> {
        if !provider_safe_path_segment(agent_group_id) || !provider_safe_path_segment(session_id) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let session_relative_path = PathBuf::from("data")
            .join("v2-sessions")
            .join(agent_group_id)
            .join(session_id);
        let snapshot = NanoClawProjectDatabaseSnapshot::read_root_bound(
            data_root,
            &self.root,
            &session_relative_path,
            source,
        )?;
        let Some(read) = snapshot.open_read()? else {
            self.sessions.revalidate()?;
            self.root.revalidate()?;
            return Ok(None);
        };
        Ok(Some(NanoClawSelectedDatabaseRead { snapshot, read }))
    }

    pub(in crate::provider::providers::nanoclaw) fn finish(&mut self) -> Result<()> {
        self.sessions.revalidate()?;
        if self.central_route.read()? != self.central_snapshot {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        self.central_guard
            .take()
            .ok_or(CaptureError::SystemInvariant(
                "NanoClaw selected central SQLite guard is no longer active",
            ))?
            .finish()
            .map_err(|error| nanoclaw_sqlite_access_error(&self.central_path, error))?;
        self.central_opened.revalidate()?;
        self.central_route.revalidate_authority()?;
        self.sessions.revalidate()?;
        if self.central_route.read()? != self.central_snapshot {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        self.root.revalidate()
    }
}

impl NanoClawSelectedDatabaseRead {
    pub(in crate::provider::providers::nanoclaw) fn connection(&self) -> Result<&Connection> {
        self.read.connection()
    }

    pub(in crate::provider::providers::nanoclaw) fn finish(self) -> Result<()> {
        self.read.finish(&self.snapshot)
    }
}
