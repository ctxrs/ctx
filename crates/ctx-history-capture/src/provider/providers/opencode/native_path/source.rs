use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use rusqlite::{limits::Limit, Connection, OpenFlags};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::{CaptureError, Result};

use super::{model::OpenCodeNativePhysicalSourceIdentity, schema::hex_digest};

const OPENCODE_SNAPSHOT_ATTEMPTS: u64 = 4;
const OPENCODE_SNAPSHOT_DIGEST_DOMAIN: &[u8] = b"ctx-opencode-nativepath-snapshot-v1\0";

#[derive(Clone, Debug, PartialEq, Eq)]
struct ComponentSignature {
    length: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl ComponentSignature {
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
                reason: "OpenCode SQLite generation components must be regular non-symlink files",
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
struct SourceMetadata {
    database: ComponentSignature,
    wal: Option<ComponentSignature>,
    rollback_journal: Option<ComponentSignature>,
}

impl SourceMetadata {
    fn read(path: &Path) -> Result<Self> {
        let database = ComponentSignature::read(path, true)?.ok_or_else(|| {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "OpenCode SQLite database does not exist",
            }
        })?;
        Ok(Self {
            database,
            wal: ComponentSignature::read(&sidecar_path(path, "-wal"), false)?,
            rollback_journal: ComponentSignature::read(&sidecar_path(path, "-journal"), false)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SnapshotComponent {
    signature: ComponentSignature,
    digest: [u8; 32],
}

impl SnapshotComponent {
    fn read(path: &Path, required: bool) -> Result<Option<Self>> {
        let Some(expected) = ComponentSignature::read(path, required)? else {
            return Ok(None);
        };
        let mut file = match open_component(path) {
            Ok(file) => file,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            Err(error) => return Err(error),
        };
        let opened = ComponentSignature::from_metadata(&file.metadata()?)?;
        if opened != expected {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let digest = hash_reader(&mut file)?;
        let closed = ComponentSignature::from_metadata(&file.metadata()?)?;
        let current = match ComponentSignature::read(path, true) {
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
pub(super) struct OpenCodeLiveObservation {
    source_path: PathBuf,
    database: SnapshotComponent,
    wal: Option<SnapshotComponent>,
    rollback_journal: Option<SnapshotComponent>,
}

impl OpenCodeLiveObservation {
    fn read(path: &Path) -> Result<Self> {
        let database = SnapshotComponent::read(path, true)?.ok_or_else(|| {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "OpenCode SQLite database does not exist",
            }
        })?;
        Ok(Self {
            source_path: path.to_path_buf(),
            database,
            wal: SnapshotComponent::read(&sidecar_path(path, "-wal"), false)?,
            rollback_journal: SnapshotComponent::read(&sidecar_path(path, "-journal"), false)?,
        })
    }

    pub(super) fn generation_digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(OPENCODE_SNAPSHOT_DIGEST_DOMAIN);
        hash_observed_component(&mut hasher, b"database", Some(&self.database));
        hash_observed_component(&mut hasher, b"wal", self.wal.as_ref());
        hash_observed_component(
            &mut hasher,
            b"rollback-journal",
            self.rollback_journal.as_ref(),
        );
        hex_digest(hasher.finalize().into())
    }

    pub(super) fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub(super) fn physical_source_identity(&self) -> OpenCodeNativePhysicalSourceIdentity {
        #[cfg(unix)]
        {
            OpenCodeNativePhysicalSourceIdentity::Unix {
                device: self.database.signature.device,
                inode: self.database.signature.inode,
            }
        }
        #[cfg(not(unix))]
        {
            OpenCodeNativePhysicalSourceIdentity::UnsupportedPlatform
        }
    }
}

pub(super) struct OpenCodeSnapshotGeneration {
    observation: OpenCodeLiveObservation,
    #[cfg(test)]
    snapshot_path: PathBuf,
    connection: Connection,
    _snapshot_dir: TempDir,
    attempts: u64,
}

impl OpenCodeSnapshotGeneration {
    pub(super) fn acquire(selected_path: &Path) -> Result<Self> {
        let _ = SourceMetadata::read(selected_path)?;
        let source_path = fs::canonicalize(selected_path)?;
        for attempt in 1..=OPENCODE_SNAPSHOT_ATTEMPTS {
            match Self::acquire_once(&source_path, attempt) {
                Ok(snapshot) => return Ok(snapshot),
                Err(CaptureError::SourceChangedDuringCapture) => {}
                Err(error) => return Err(error),
            }
        }
        Err(CaptureError::SourceChangedDuringCapture)
    }

    fn acquire_once(source_path: &Path, attempts: u64) -> Result<Self> {
        Self::acquire_once_with_hook(source_path, attempts, |_| Ok(()))
    }

    fn acquire_once_with_hook(
        source_path: &Path,
        attempts: u64,
        before_certification: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<Self> {
        let before = SourceMetadata::read(source_path)?;
        let snapshot_dir = tempfile::Builder::new()
            .prefix("ctx-opencode-nativepath-")
            .tempdir()?;
        let file_name =
            source_path
                .file_name()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: source_path.to_path_buf(),
                    reason: "OpenCode SQLite path has no file name",
                })?;
        let snapshot_path = snapshot_dir.path().join(file_name);
        let copied_database = copy_component(source_path, &snapshot_path)?;
        let copied_wal =
            copy_optional_component(source_path, &snapshot_path, "-wal", before.wal.is_some())?;
        let copied_journal = copy_optional_component(
            source_path,
            &snapshot_path,
            "-journal",
            before.rollback_journal.is_some(),
        )?;
        let copied = OpenCodeLiveObservation {
            source_path: source_path.to_path_buf(),
            database: copied_database,
            wal: copied_wal,
            rollback_journal: copied_journal,
        };
        before_certification(source_path)?;
        let after = OpenCodeLiveObservation::read(source_path)?;
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
        // Metadata preflight uses octet_length before the only JSON visitor is invoked. Keep
        // SQLite able to step over an oversized provider cell so it can be rejected locally.
        connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, i32::MAX);
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "query_only", true)?;
        let integrity: String =
            connection.pragma_query_value(None, "quick_check", |row| row.get(0))?;
        if integrity != "ok" {
            return Err(CaptureError::InvalidPayload(format!(
                "OpenCode immutable SQLite generation failed quick_check: {integrity}"
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

    pub(super) fn observation(&self) -> &OpenCodeLiveObservation {
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
        match OpenCodeLiveObservation::read(self.observation.source_path()) {
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

fn copy_optional_component(
    source_path: &Path,
    snapshot_path: &Path,
    suffix: &str,
    expected: bool,
) -> Result<Option<SnapshotComponent>> {
    if !expected {
        return Ok(None);
    }
    let source = sidecar_path(source_path, suffix);
    let destination = sidecar_path(snapshot_path, suffix);
    match copy_component(&source, &destination) {
        Ok(component) => Ok(Some(component)),
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(CaptureError::SourceChangedDuringCapture)
        }
        Err(error) => Err(error),
    }
}

fn copy_component(source: &Path, destination: &Path) -> Result<SnapshotComponent> {
    let expected = ComponentSignature::read(source, true)?.ok_or_else(|| {
        CaptureError::InvalidProviderTranscriptPath {
            path: source.to_path_buf(),
            reason: "OpenCode SQLite generation component disappeared",
        }
    })?;
    let mut input = open_component(source)?;
    if ComponentSignature::from_metadata(&input.metadata()?)? != expected {
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
                CaptureError::SystemInvariant("OpenCode snapshot read size exceeds u64")
            })?)
            .ok_or(CaptureError::SystemInvariant(
                "OpenCode snapshot byte count overflowed",
            ))?;
    }
    output.flush()?;
    let closed = ComponentSignature::from_metadata(&input.metadata()?)?;
    let current = ComponentSignature::read(source, true)?;
    if copied != expected.length || closed != expected || current.as_ref() != Some(&expected) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(SnapshotComponent {
        signature: expected,
        digest: hasher.finalize().into(),
    })
}

fn hash_reader(reader: &mut File) -> Result<[u8; 32]> {
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

fn hash_observed_component(
    hasher: &mut Sha256,
    name: &[u8],
    component: Option<&SnapshotComponent>,
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

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

#[cfg(unix)]
fn open_component(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?)
}

#[cfg(target_os = "windows")]
fn open_component(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn open_component(path: &Path) -> Result<File> {
    Ok(File::open(path)?)
}
