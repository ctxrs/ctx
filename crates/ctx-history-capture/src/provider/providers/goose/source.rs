use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use rusqlite::{limits::Limit, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

const GOOSE_SNAPSHOT_ATTEMPTS: u64 = 4;
const GOOSE_SNAPSHOT_HASH_DOMAIN: &[u8] = b"ctx-goose-nativepath-snapshot-v1\0";

#[derive(Clone, Debug, PartialEq, Eq)]
struct GooseComponentSignature {
    length: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum GooseNativePhysicalSourceIdentity {
    Unix { device: u64, inode: u64 },
    UnsupportedPlatform,
}

impl GooseComponentSignature {
    fn read(path: &Path, required: bool) -> Result<Option<Self>> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(CaptureError::Io(error)),
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Goose SQLite generation components must be regular non-symlink files",
            });
        }
        Ok(Some(Self::from_metadata(&metadata)?))
    }

    fn from_metadata(metadata: &fs::Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GooseSourceMetadata {
    database: GooseComponentSignature,
    wal: Option<GooseComponentSignature>,
    rollback_journal: Option<GooseComponentSignature>,
}

impl GooseSourceMetadata {
    fn read(path: &Path) -> Result<Self> {
        let database = GooseComponentSignature::read(path, true)?.ok_or_else(|| {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Goose SQLite database does not exist",
            }
        })?;
        Ok(Self {
            database,
            wal: GooseComponentSignature::read(&goose_sidecar_path(path, "-wal"), false)?,
            rollback_journal: GooseComponentSignature::read(
                &goose_sidecar_path(path, "-journal"),
                false,
            )?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GooseSnapshotComponent {
    signature: GooseComponentSignature,
    digest: [u8; 32],
}

impl GooseSnapshotComponent {
    fn read(path: &Path, required: bool) -> Result<Option<Self>> {
        let Some(expected) = GooseComponentSignature::read(path, required)? else {
            return Ok(None);
        };
        let mut file = match open_goose_component(path) {
            Ok(file) => file,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            Err(error) => return Err(error),
        };
        let opened = GooseComponentSignature::from_metadata(&file.metadata()?)?;
        if opened != expected {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let digest = goose_hash_reader(&mut file)?;
        let closed = GooseComponentSignature::from_metadata(&file.metadata()?)?;
        let current = match GooseComponentSignature::read(path, true) {
            Ok(current) => current,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            Err(error) => return Err(error),
        };
        if closed != expected || current.as_ref() != Some(&expected) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(Some(Self {
            signature: expected,
            digest,
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GooseLiveObservation {
    source_path: PathBuf,
    database: GooseSnapshotComponent,
    wal: Option<GooseSnapshotComponent>,
    rollback_journal: Option<GooseSnapshotComponent>,
}

impl GooseLiveObservation {
    pub(super) fn read(path: &Path) -> Result<Self> {
        let database = GooseSnapshotComponent::read(path, true)?.ok_or_else(|| {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Goose SQLite database does not exist",
            }
        })?;
        Ok(Self {
            source_path: path.to_path_buf(),
            database,
            wal: GooseSnapshotComponent::read(&goose_sidecar_path(path, "-wal"), false)?,
            rollback_journal: GooseSnapshotComponent::read(
                &goose_sidecar_path(path, "-journal"),
                false,
            )?,
        })
    }

    pub(super) fn generation_digest(&self) -> String {
        // This hashes immutable source components only as a control-plane fence.
        // It is never an event/output hash and is not publication content.
        let mut hasher = Sha256::new();
        hasher.update(GOOSE_SNAPSHOT_HASH_DOMAIN);
        goose_hash_observed_component(&mut hasher, b"database", Some(&self.database));
        goose_hash_observed_component(&mut hasher, b"wal", self.wal.as_ref());
        goose_hash_observed_component(
            &mut hasher,
            b"rollback-journal",
            self.rollback_journal.as_ref(),
        );
        goose_hex_digest(hasher.finalize().into())
    }

    pub(super) fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub(super) fn physical_source_identity(&self) -> GooseNativePhysicalSourceIdentity {
        #[cfg(unix)]
        {
            GooseNativePhysicalSourceIdentity::Unix {
                device: self.database.signature.device,
                inode: self.database.signature.inode,
            }
        }
        #[cfg(not(unix))]
        {
            GooseNativePhysicalSourceIdentity::UnsupportedPlatform
        }
    }
}

pub(super) struct GooseSnapshotGeneration {
    observation: GooseLiveObservation,
    #[cfg(test)]
    snapshot_path: PathBuf,
    connection: Connection,
    _snapshot_dir: TempDir,
    attempts: u64,
}

impl GooseSnapshotGeneration {
    pub(super) fn acquire(selected_path: &Path) -> Result<Self> {
        // Reject a selected symlink before canonicalization can hide it.
        let _ = GooseSourceMetadata::read(selected_path)?;
        let source_path = fs::canonicalize(selected_path)?;
        let mut last_changed = false;
        for attempt in 1..=GOOSE_SNAPSHOT_ATTEMPTS {
            match Self::acquire_once(&source_path, attempt) {
                Ok(snapshot) => return Ok(snapshot),
                Err(CaptureError::SourceChangedDuringCapture) => last_changed = true,
                Err(error) => return Err(error),
            }
        }
        if last_changed {
            Err(CaptureError::SourceChangedDuringCapture)
        } else {
            Err(CaptureError::SystemInvariant(
                "Goose snapshot acquisition exhausted without an error",
            ))
        }
    }

    fn acquire_once(source_path: &Path, attempts: u64) -> Result<Self> {
        let before = GooseSourceMetadata::read(source_path)?;
        let snapshot_dir = tempfile::Builder::new()
            .prefix("ctx-goose-nativepath-")
            .tempdir()?;
        let file_name =
            source_path
                .file_name()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: source_path.to_path_buf(),
                    reason: "Goose SQLite path has no file name",
                })?;
        let snapshot_path = snapshot_dir.path().join(file_name);
        let copied_database = goose_copy_component(source_path, &snapshot_path)?;
        let copied_wal = goose_copy_optional_component(
            source_path,
            &snapshot_path,
            "-wal",
            before.wal.is_some(),
        )?;
        let copied_journal = goose_copy_optional_component(
            source_path,
            &snapshot_path,
            "-journal",
            before.rollback_journal.is_some(),
        )?;
        let copied = GooseLiveObservation {
            source_path: source_path.to_path_buf(),
            database: copied_database,
            wal: copied_wal,
            rollback_journal: copied_journal,
        };
        let after = GooseLiveObservation::read(source_path)?;
        if before.database != after.database.signature
            || before.wal.as_ref() != after.wal.as_ref().map(|value| &value.signature)
            || before.rollback_journal.as_ref()
                != after
                    .rollback_journal
                    .as_ref()
                    .map(|value| &value.signature)
            || copied.database.digest != after.database.digest
            || copied.wal.as_ref().map(|value| value.digest)
                != after.wal.as_ref().map(|value| value.digest)
            || copied.rollback_journal.as_ref().map(|value| value.digest)
                != after.rollback_journal.as_ref().map(|value| value.digest)
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }

        let connection = Connection::open_with_flags(
            &snapshot_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
            .map_err(|_| CaptureError::SystemInvariant("Goose SQLite value limit exceeds i32"))?;
        connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let integrity: String =
            connection.pragma_query_value(None, "quick_check", |row| row.get(0))?;
        if integrity != "ok" {
            return Err(CaptureError::InvalidPayload(format!(
                "Goose immutable SQLite generation failed quick_check: {integrity}"
            )));
        }

        Ok(Self {
            observation: after,
            #[cfg(test)]
            snapshot_path,
            connection,
            _snapshot_dir: snapshot_dir,
            attempts,
        })
    }

    pub(super) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(super) fn observation(&self) -> &GooseLiveObservation {
        &self.observation
    }

    #[cfg(test)]
    pub(super) fn snapshot_path(&self) -> &Path {
        &self.snapshot_path
    }

    pub(super) fn attempts(&self) -> u64 {
        self.attempts
    }

    pub(super) fn revalidate_live(&self) -> Result<bool> {
        match GooseLiveObservation::read(self.observation.source_path()) {
            Ok(current) => Ok(current == self.observation),
            Err(CaptureError::SourceChangedDuringCapture) => Ok(false),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

fn goose_copy_optional_component(
    source_path: &Path,
    snapshot_path: &Path,
    suffix: &str,
    expected: bool,
) -> Result<Option<GooseSnapshotComponent>> {
    if !expected {
        return Ok(None);
    }
    let source = goose_sidecar_path(source_path, suffix);
    let destination = goose_sidecar_path(snapshot_path, suffix);
    match goose_copy_component(&source, &destination) {
        Ok(component) => Ok(Some(component)),
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(CaptureError::SourceChangedDuringCapture)
        }
        Err(error) => Err(error),
    }
}

fn goose_copy_component(source: &Path, destination: &Path) -> Result<GooseSnapshotComponent> {
    let expected = GooseComponentSignature::read(source, true)?.ok_or_else(|| {
        CaptureError::InvalidProviderTranscriptPath {
            path: source.to_path_buf(),
            reason: "Goose SQLite generation component disappeared",
        }
    })?;
    let mut input = open_goose_component(source)?;
    if GooseComponentSignature::from_metadata(&input.metadata()?)? != expected {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
        copied = copied
            .checked_add(u64::try_from(read).map_err(|_| {
                CaptureError::SystemInvariant("Goose snapshot read size exceeds u64")
            })?)
            .ok_or(CaptureError::SystemInvariant(
                "Goose snapshot byte count overflowed",
            ))?;
    }
    output.flush()?;
    let closed = GooseComponentSignature::from_metadata(&input.metadata()?)?;
    let current = GooseComponentSignature::read(source, true)?;
    if copied != expected.length || closed != expected || current.as_ref() != Some(&expected) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(GooseSnapshotComponent {
        signature: expected,
        digest: hasher.finalize().into(),
    })
}

fn goose_hash_reader(reader: &mut File) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn goose_hash_observed_component(
    hasher: &mut Sha256,
    name: &[u8],
    component: Option<&GooseSnapshotComponent>,
) {
    hasher.update((name.len() as u64).to_le_bytes());
    hasher.update(name);
    match component {
        Some(component) => {
            hasher.update([1]);
            hasher.update(component.signature.length.to_le_bytes());
            hasher.update(component.digest);
        }
        None => hasher.update([0]),
    }
}

fn goose_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn goose_hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(unix)]
fn open_goose_component(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?)
}

#[cfg(target_os = "windows")]
fn open_goose_component(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn open_goose_component(path: &Path) -> Result<File> {
    Ok(File::open(path)?)
}
