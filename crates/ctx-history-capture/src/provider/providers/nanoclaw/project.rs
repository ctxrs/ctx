use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::provider::provider_safe_path_segment;
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::{sqlite_component_change_token, with_sqlite_read_snapshot};
use crate::provider_sources::observe_ordinary_file;
use crate::{CaptureError, Result};

use super::position::{nanoclaw_ordered_i64, NanoClawMessageSource};
use super::rows::{
    nanoclaw_observed_bytes, nanoclaw_retained_length_expr, nanoclaw_session_columns,
    nanoclaw_session_projection, NANOCLAW_NATIVE_MAX_RECORD_BYTES,
};
use super::{NANOCLAW_CAPTURE_REVISION, NANOCLAW_POLICY_REVISION};

const NANOCLAW_INVENTORY_PAGE_ENTRIES: usize = 64;
const NANOCLAW_INVENTORY_MIN_INTERVAL: Duration = Duration::from_millis(5);
const NANOCLAW_INVENTORY_HASH_DOMAIN: &[u8] = b"ctx-nanoclaw-inventory-sha256-v1\0";

#[derive(Clone, PartialEq, Eq)]
struct NanoClawFrozenFileMetadata {
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
    change_token: [u8; 32],
}

impl NanoClawFrozenFileMetadata {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "NanoClaw SQLite component must be a regular non-symlink file",
            });
        }
        let observation = observe_ordinary_file(path)?;
        if metadata.len() != observation.len()
            || metadata.modified().ok() != Some(observation.modified_at())
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let change_token = sqlite_component_change_token(path, &observation)?;
        Self::from_metadata(&metadata, change_token)
    }

    fn read_optional(path: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                Self::read(path).map(Some)
            }
            Ok(_) => Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "NanoClaw SQLite sidecar must be a regular non-symlink file",
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(CaptureError::Io(error)),
        }
    }

    fn from_metadata(metadata: &fs::Metadata, change_token: [u8; 32]) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            #[cfg(unix)]
            device: Some(metadata.dev()),
            #[cfg(not(unix))]
            device: None,
            #[cfg(unix)]
            inode: Some(metadata.ino()),
            #[cfg(not(unix))]
            inode: None,
            change_token,
        })
    }

    fn update_hash(&self, hasher: &mut Sha256) {
        nanoclaw_hash_u64(hasher, self.length);
        let (sign, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => (1_u8, duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                (0_u8, duration.as_secs(), duration.subsec_nanos())
            }
        };
        nanoclaw_hash_bytes(hasher, &[sign]);
        nanoclaw_hash_u64(hasher, seconds);
        nanoclaw_hash_u64(hasher, u64::from(nanos));
        nanoclaw_hash_bytes(hasher, &[u8::from(self.readonly)]);
        nanoclaw_hash_optional_u64(hasher, self.device);
        nanoclaw_hash_optional_u64(hasher, self.inode);
        nanoclaw_hash_bytes(hasher, &self.change_token);
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct NanoClawSqliteSnapshot {
    database: NanoClawFrozenFileMetadata,
    wal: Option<NanoClawFrozenFileMetadata>,
    shared_memory: Option<NanoClawFrozenFileMetadata>,
    rollback_journal: Option<NanoClawFrozenFileMetadata>,
}

impl NanoClawSqliteSnapshot {
    pub(super) fn read(path: &Path) -> Result<Self> {
        Ok(Self {
            database: NanoClawFrozenFileMetadata::read(path)?,
            wal: NanoClawFrozenFileMetadata::read_optional(&nanoclaw_sidecar_path(path, "-wal"))?,
            shared_memory: NanoClawFrozenFileMetadata::read_optional(&nanoclaw_sidecar_path(
                path, "-shm",
            ))?,
            rollback_journal: NanoClawFrozenFileMetadata::read_optional(&nanoclaw_sidecar_path(
                path, "-journal",
            ))?,
        })
    }

    pub(super) fn read_optional(path: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                Self::read(path).map(Some)
            }
            Ok(_) => Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "NanoClaw message store must be a regular non-symlink file",
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(CaptureError::Io(error)),
        }
    }

    fn update_hash(&self, hasher: &mut Sha256) {
        self.database.update_hash(hasher);
        nanoclaw_hash_optional_file(hasher, self.wal.as_ref());
        nanoclaw_hash_optional_file(hasher, self.shared_memory.as_ref());
        nanoclaw_hash_optional_file(hasher, self.rollback_journal.as_ref());
    }

    pub(super) fn revalidate(&self, path: &Path) -> Result<bool> {
        match Self::read(path) {
            Ok(current) => Ok(current == *self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"ctx-nanoclaw-sqlite-snapshot-sha256-v1\0");
        self.update_hash(&mut hasher);
        hasher.finalize().into()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct NanoClawProjectDatabaseSnapshot {
    source: NanoClawMessageSource,
    path: PathBuf,
    sqlite: Option<NanoClawSqliteSnapshot>,
}

impl NanoClawProjectDatabaseSnapshot {
    pub(super) fn read(session_dir: &Path, source: NanoClawMessageSource) -> Result<Self> {
        let path = session_dir.join(source.file_name());
        let sqlite = NanoClawSqliteSnapshot::read_optional(&path)?;
        Ok(Self {
            source,
            path,
            sqlite,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn is_present(&self) -> bool {
        self.sqlite.is_some()
    }

    pub(super) fn revalidate(&self) -> Result<bool> {
        match NanoClawSqliteSnapshot::read_optional(&self.path) {
            Ok(current) => Ok(current == self.sqlite),
            Err(CaptureError::SourceChangedDuringCapture)
            | Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn update_hash(&self, hasher: &mut Sha256) {
        nanoclaw_hash_bytes(hasher, &[self.source.tag()]);
        nanoclaw_hash_optional_sqlite(hasher, self.sqlite.as_ref());
    }
}

#[derive(Clone, PartialEq, Eq)]
struct NanoClawSessionDatabaseSnapshot {
    rowid: i64,
    agent_group_id: String,
    session_id: String,
    inbound: NanoClawProjectDatabaseSnapshot,
    outbound: NanoClawProjectDatabaseSnapshot,
}

impl NanoClawSessionDatabaseSnapshot {
    fn database(&self, source: NanoClawMessageSource) -> &NanoClawProjectDatabaseSnapshot {
        match source {
            NanoClawMessageSource::Inbound => &self.inbound,
            NanoClawMessageSource::Outbound => &self.outbound,
        }
    }

    fn revalidate(&self) -> Result<bool> {
        if !self.inbound.revalidate()? {
            return Ok(false);
        }
        self.outbound.revalidate()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct NanoClawProjectInventory {
    digest: [u8; 32],
    session_count: u64,
    // These remain distinct database observations. The project snapshot coordinates their
    // lifetime and revalidation; it does not merge their connections or read transactions.
    session_databases: Vec<NanoClawSessionDatabaseSnapshot>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct NanoClawProjectSnapshot {
    central_path: PathBuf,
    central: NanoClawSqliteSnapshot,
    inventory: NanoClawProjectInventory,
}

impl NanoClawProjectSnapshot {
    pub(super) fn read(project_root: &Path, central_path: &Path) -> Result<Self> {
        Self::read_inner(project_root, central_path, None)
    }

    /// Freezes only the explicitly selected project and rejects component
    /// databases that resolve outside its `data/v2-sessions` authority.
    pub(super) fn read_source_backed(project_root: &Path, central_path: &Path) -> Result<Self> {
        let canonical_root = fs::canonicalize(project_root)?;
        let canonical_central = fs::canonicalize(central_path)?;
        if !canonical_central.starts_with(&canonical_root) {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: central_path.to_path_buf(),
                reason: "NanoClaw data/v2.db escapes the selected project root",
            });
        }
        let sessions_root = canonical_root.join("data").join("v2-sessions");
        let sessions_metadata = fs::symlink_metadata(&sessions_root)?;
        if sessions_metadata.file_type().is_symlink() || !sessions_metadata.is_dir() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: sessions_root,
                reason: "NanoClaw data/v2-sessions must be a real directory",
            });
        }
        Self::read_inner(
            &canonical_root,
            &canonical_central,
            Some(canonical_root.join("data").join("v2-sessions")),
        )
    }

    fn read_inner(
        project_root: &Path,
        central_path: &Path,
        contained_sessions_root: Option<PathBuf>,
    ) -> Result<Self> {
        let central = NanoClawSqliteSnapshot::read(central_path)?;
        let conn = open_provider_sqlite_readonly(central_path)?;
        let inventory = with_sqlite_read_snapshot(&conn, || {
            nanoclaw_stream_inventory(project_root, &conn, contained_sessions_root.as_deref())
        })?;
        let snapshot = Self {
            central_path: central_path.to_path_buf(),
            central,
            inventory,
        };
        if !snapshot.revalidate_frozen_inventory()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(snapshot)
    }

    pub(super) fn source_backed_revision_evidence(
        &self,
        user_version: i64,
        schema_fingerprint: &str,
    ) -> Result<Vec<u8>> {
        let component_databases = self
            .inventory
            .session_databases
            .iter()
            .map(|session| {
                u64::from(session.inbound.is_present()) + u64::from(session.outbound.is_present())
            })
            .sum();
        Ok(serde_json::to_vec(&NanoClawCompoundRevisionEvidence {
            version: 1,
            capture_revision: NANOCLAW_CAPTURE_REVISION,
            policy_revision: NANOCLAW_POLICY_REVISION,
            user_version,
            schema_fingerprint,
            central_sha256: nanoclaw_hex(&self.central.digest()),
            session_inventory_sha256: nanoclaw_hex(&self.inventory.digest),
            sessions: self.inventory.session_count,
            component_databases,
        })?)
    }

    pub(super) fn source_revision(&self, user_version: i64, schema_fingerprint: &str) -> String {
        format!(
            "nanoclaw-project-snapshot-v1:capture={NANOCLAW_CAPTURE_REVISION};policy={NANOCLAW_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint};sessions={};inventory={};central={}",
            self.inventory.session_count,
            nanoclaw_hex(&self.inventory.digest),
            nanoclaw_hex(&self.central.digest()),
        )
    }

    pub(super) fn database(
        &self,
        rowid: i64,
        agent_group_id: &str,
        session_id: &str,
        source: NanoClawMessageSource,
    ) -> Result<&NanoClawProjectDatabaseSnapshot> {
        let index = self
            .inventory
            .session_databases
            .binary_search_by_key(&rowid, |session| session.rowid)
            .map_err(|_| CaptureError::SourceChangedDuringCapture)?;
        let session = &self.inventory.session_databases[index];
        if session.agent_group_id != agent_group_id || session.session_id != session_id {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(session.database(source))
    }

    pub(super) fn revalidate(&self) -> Result<bool> {
        self.revalidate_frozen_inventory()
    }

    pub(super) fn revalidate_before_commit(&self) -> Result<bool> {
        self.revalidate_frozen_inventory()
    }

    fn revalidate_frozen_inventory(&self) -> Result<bool> {
        if !self.central.revalidate(&self.central_path)? {
            return Ok(false);
        }
        for session in &self.inventory.session_databases {
            if !session.revalidate()? {
                return Ok(false);
            }
        }
        self.central.revalidate(&self.central_path)
    }
}

struct NanoClawInventoryPacer {
    entries: usize,
    window_started: Instant,
}

impl NanoClawInventoryPacer {
    fn new() -> Self {
        Self {
            entries: 0,
            window_started: Instant::now(),
        }
    }

    fn observe(&mut self) {
        self.entries = self.entries.saturating_add(1);
        if self.entries < NANOCLAW_INVENTORY_PAGE_ENTRIES {
            return;
        }
        let elapsed = self.window_started.elapsed();
        if elapsed < NANOCLAW_INVENTORY_MIN_INTERVAL {
            thread::sleep(NANOCLAW_INVENTORY_MIN_INTERVAL - elapsed);
        }
        self.entries = 0;
        self.window_started = Instant::now();
    }
}

fn nanoclaw_stream_inventory(
    project_root: &Path,
    conn: &Connection,
    contained_sessions_root: Option<&Path>,
) -> Result<NanoClawProjectInventory> {
    let columns = nanoclaw_session_columns(conn)?;
    let retained = nanoclaw_retained_length_expr(&nanoclaw_session_projection(conn, &columns)?);
    let mut candidates = conn.prepare(&format!(
        "select s.rowid, {retained} from sessions s order by s.rowid"
    ))?;
    let mut hydrate = conn.prepare(
        "select CAST(id AS TEXT), CAST(agent_group_id AS TEXT) from sessions where rowid = ?1",
    )?;
    let mut rows = candidates.query([])?;
    let mut hasher = Sha256::new();
    hasher.update(NANOCLAW_INVENTORY_HASH_DOMAIN);
    let mut count = 0_u64;
    let mut session_databases = Vec::new();
    let mut pacer = NanoClawInventoryPacer::new();
    while let Some(row) = rows.next()? {
        let rowid: i64 = row.get(0)?;
        let retained_bytes: i64 = row.get(1)?;
        let observed_bytes = nanoclaw_observed_bytes(retained_bytes)?;
        nanoclaw_hash_u64(&mut hasher, nanoclaw_ordered_i64(rowid));
        nanoclaw_hash_u64(&mut hasher, observed_bytes);
        if observed_bytes <= NANOCLAW_NATIVE_MAX_RECORD_BYTES {
            let (session_id, agent_group_id): (String, String) =
                hydrate.query_row([rowid], |row| Ok((row.get(0)?, row.get(1)?)))?;
            nanoclaw_hash_bytes(&mut hasher, session_id.as_bytes());
            nanoclaw_hash_bytes(&mut hasher, agent_group_id.as_bytes());
            if provider_safe_path_segment(&agent_group_id)
                && provider_safe_path_segment(&session_id)
            {
                let session_dir = project_root
                    .join("data")
                    .join("v2-sessions")
                    .join(&agent_group_id)
                    .join(&session_id);
                let read_component = |source: NanoClawMessageSource| {
                    let component_path = session_dir.join(source.file_name());
                    if let Some(contained_sessions_root) = contained_sessions_root {
                        match fs::symlink_metadata(&component_path) {
                            Ok(_) => {
                                let canonical_path = fs::canonicalize(&component_path)?;
                                if !canonical_path.starts_with(contained_sessions_root) {
                                    return Err(CaptureError::InvalidProviderTranscriptPath {
                                        path: component_path,
                                        reason:
                                            "NanoClaw message database escapes data/v2-sessions",
                                    });
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(error) => return Err(CaptureError::Io(error)),
                        }
                    }
                    NanoClawProjectDatabaseSnapshot::read(&session_dir, source)
                };
                let inbound = read_component(NanoClawMessageSource::Inbound)?;
                let outbound = read_component(NanoClawMessageSource::Outbound)?;
                inbound.update_hash(&mut hasher);
                outbound.update_hash(&mut hasher);
                session_databases.push(NanoClawSessionDatabaseSnapshot {
                    rowid,
                    agent_group_id,
                    session_id,
                    inbound,
                    outbound,
                });
            } else {
                nanoclaw_hash_bytes(&mut hasher, b"unsafe-session-path");
            }
        } else {
            nanoclaw_hash_bytes(&mut hasher, b"oversize-session-row");
        }
        count = count.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "NanoClaw inventory session count overflowed",
        ))?;
        pacer.observe();
    }
    Ok(NanoClawProjectInventory {
        digest: hasher.finalize().into(),
        session_count: count,
        session_databases,
    })
}

#[derive(Serialize)]
struct NanoClawCompoundRevisionEvidence<'a> {
    version: u32,
    capture_revision: u32,
    policy_revision: u32,
    user_version: i64,
    schema_fingerprint: &'a str,
    central_sha256: String,
    session_inventory_sha256: String,
    sessions: u64,
    component_databases: u64,
}

fn nanoclaw_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn nanoclaw_hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}

fn nanoclaw_hash_u64(hasher: &mut Sha256, value: u64) {
    nanoclaw_hash_bytes(hasher, &value.to_be_bytes());
}

fn nanoclaw_hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            nanoclaw_hash_bytes(hasher, &[1]);
            nanoclaw_hash_u64(hasher, value);
        }
        None => nanoclaw_hash_bytes(hasher, &[0]),
    }
}

fn nanoclaw_hash_optional_file(hasher: &mut Sha256, value: Option<&NanoClawFrozenFileMetadata>) {
    match value {
        Some(value) => {
            nanoclaw_hash_bytes(hasher, &[1]);
            value.update_hash(hasher);
        }
        None => nanoclaw_hash_bytes(hasher, &[0]),
    }
}

fn nanoclaw_hash_optional_sqlite(hasher: &mut Sha256, snapshot: Option<&NanoClawSqliteSnapshot>) {
    match snapshot {
        Some(snapshot) => {
            nanoclaw_hash_bytes(hasher, &[1]);
            snapshot.update_hash(hasher);
        }
        None => nanoclaw_hash_bytes(hasher, &[0]),
    }
}

fn nanoclaw_hex(value: &[u8]) -> String {
    use std::fmt::Write;

    let mut encoded = String::with_capacity(value.len().saturating_mul(2));
    for byte in value {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

pub(super) fn nanoclaw_project_root(path: &Path) -> Result<PathBuf> {
    if path.is_dir() && path.join("data").join("v2.db").is_file() {
        return Ok(path.to_path_buf());
    }
    if path.file_name().and_then(|name| name.to_str()) == Some("v2.db") {
        if let Some(data_dir) = path.parent() {
            if let Some(root) = data_dir.parent() {
                return Ok(root.to_path_buf());
            }
        }
    }
    Err(CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "NanoClaw import path must be a project root or data/v2.db",
    })
}
