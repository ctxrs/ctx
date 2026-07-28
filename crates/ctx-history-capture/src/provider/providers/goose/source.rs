use std::{
    cell::RefCell,
    io::{self, Read},
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider_sources::{
        open_sqlite_source_snapshot, SqliteSourceAccessError, SqliteSourceComponent,
        SqliteSourceReadSnapshot,
    },
    CaptureError, Result,
};

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

impl GooseComponentSignature {
    fn from_opened(file: &OpenedProviderSourceFile) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        let metadata = file.metadata();
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum GooseNativePhysicalSourceIdentity {
    Unix { device: u64, inode: u64 },
    UnsupportedPlatform,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GooseSnapshotComponent {
    signature: GooseComponentSignature,
    digest: [u8; 32],
}

impl GooseSnapshotComponent {
    fn read(file: &OpenedProviderSourceFile) -> Result<Self> {
        let signature = GooseComponentSignature::from_opened(file)?;
        let mut reader = file.bounded_reader(file.len())?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        file.revalidate()?;
        Ok(Self {
            signature,
            digest: hasher.finalize().into(),
        })
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
    pub(super) fn generation_digest(&self) -> String {
        goose_hex_digest(self.generation_digest_bytes())
    }

    pub(super) fn generation_digest_bytes(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(GOOSE_SNAPSHOT_HASH_DOMAIN);
        goose_hash_observed_component(&mut hasher, b"database", Some(&self.database));
        goose_hash_observed_component(&mut hasher, b"wal", self.wal.as_ref());
        goose_hash_observed_component(
            &mut hasher,
            b"rollback-journal",
            self.rollback_journal.as_ref(),
        );
        hasher.finalize().into()
    }

    pub(super) fn certified_bytes(&self) -> Result<u64> {
        std::iter::once(&self.database)
            .chain(self.wal.iter())
            .chain(self.rollback_journal.iter())
            .try_fold(0_u64, |total, component| {
                total
                    .checked_add(component.signature.length)
                    .ok_or(CaptureError::SystemInvariant(
                        "Goose certified source byte count overflowed",
                    ))
            })
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

#[derive(Debug)]
struct GooseAdmittedSqliteComponent {
    file: OpenedProviderSourceFile,
    observation: GooseSnapshotComponent,
}

impl GooseAdmittedSqliteComponent {
    fn open(root: &ProviderSourceRoot, relative_path: &Path) -> Result<Self> {
        let file = root.open_file(relative_path)?;
        let observation = GooseSnapshotComponent::read(&file)?;
        Ok(Self { file, observation })
    }

    fn open_optional(root: &ProviderSourceRoot, relative_path: &Path) -> Result<Option<Self>> {
        match Self::open(root, relative_path) {
            Ok(component) => Ok(Some(component)),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug)]
struct GooseAdmittedSqliteFamily {
    root: ProviderSourceRoot,
    database: GooseAdmittedSqliteComponent,
    wal: Option<GooseAdmittedSqliteComponent>,
    shared_memory: Option<GooseAdmittedSqliteComponent>,
    rollback_journal: Option<GooseAdmittedSqliteComponent>,
}

impl GooseAdmittedSqliteFamily {
    fn open(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Goose SQLite path has no authority parent",
            })?;
        let filename =
            path.file_name()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "Goose SQLite path has no file name",
                })?;
        let root = ProviderSourceRoot::open(parent)?;
        let database = GooseAdmittedSqliteComponent::open(&root, Path::new(filename))?;
        let wal = GooseAdmittedSqliteComponent::open_optional(
            &root,
            &goose_sidecar_relative_path(filename, "-wal"),
        )?;
        let shared_memory = GooseAdmittedSqliteComponent::open_optional(
            &root,
            &goose_sidecar_relative_path(filename, "-shm"),
        )?;
        let rollback_journal = GooseAdmittedSqliteComponent::open_optional(
            &root,
            &goose_sidecar_relative_path(filename, "-journal"),
        )?;
        let family = Self {
            root,
            database,
            wal,
            shared_memory,
            rollback_journal,
        };
        if !family.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(family)
    }

    fn observation(&self, source_path: PathBuf) -> GooseLiveObservation {
        GooseLiveObservation {
            source_path,
            database: self.database.observation.clone(),
            wal: self.wal.as_ref().map(|value| value.observation.clone()),
            rollback_journal: self
                .rollback_journal
                .as_ref()
                .map(|value| value.observation.clone()),
        }
    }

    fn unsupported_stock_vfs_sidecar(&self) -> Option<SqliteSourceAccessError> {
        let (component, capability) = if self.wal.is_some() {
            (
                SqliteSourceComponent::Wal,
                "the stock Unix VFS cannot bind the opened WAL to the admitted Goose WAL handle",
            )
        } else if self.rollback_journal.is_some() {
            (
                SqliteSourceComponent::RollbackJournal,
                "rollback recovery is not permitted without a root-handle SQLite VFS",
            )
        } else if self.shared_memory.is_some() {
            (
                SqliteSourceComponent::SharedMemory,
                "the stock Unix VFS exposes no native SHM identity",
            )
        } else {
            return None;
        };
        Some(SqliteSourceAccessError::UnsupportedSidecarIdentity {
            component,
            capability,
        })
    }

    fn revalidate(&self) -> Result<bool> {
        let result = (|| -> Result<()> {
            self.database.file.revalidate()?;
            for component in self
                .wal
                .iter()
                .chain(self.shared_memory.iter())
                .chain(self.rollback_journal.iter())
            {
                component.file.revalidate()?;
            }
            self.root.revalidate()
        })();
        match result {
            Ok(()) => Ok(true),
            Err(CaptureError::InvalidProviderTranscriptPath { .. })
            | Err(CaptureError::SourceChangedDuringCapture) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

pub(super) struct GooseSnapshotGeneration {
    observation: GooseLiveObservation,
    authority: GooseAdmittedSqliteFamily,
    initial_connection: RefCell<Option<SqliteSourceReadSnapshot>>,
    attempts: u64,
}

impl GooseSnapshotGeneration {
    pub(super) fn acquire(selected_path: &Path) -> Result<Self> {
        let source_path = goose_absolute_authority_path(selected_path)?;
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
        let authority = GooseAdmittedSqliteFamily::open(source_path)?;
        if let Some(error) = authority.unsupported_stock_vfs_sidecar() {
            return Err(goose_sqlite_access_error(error));
        }
        let connection =
            match open_sqlite_source_snapshot(source_path, authority.database.file.file()) {
                Ok(connection) => connection,
                Err(
                    SqliteSourceAccessError::SourceChanged
                    | SqliteSourceAccessError::ConnectionIdentityMismatch,
                ) => return Err(CaptureError::SourceChangedDuringCapture),
                Err(error) => return Err(goose_sqlite_access_error(error)),
            };
        if !authority.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let observation = authority.observation(source_path.to_path_buf());
        Ok(Self {
            observation,
            authority,
            initial_connection: RefCell::new(Some(connection)),
            attempts,
        })
    }

    pub(super) fn connection(&self) -> Result<SqliteSourceReadSnapshot> {
        let connection = self
            .initial_connection
            .borrow_mut()
            .take()
            .map(Ok)
            .unwrap_or_else(|| {
                open_sqlite_source_snapshot(
                    self.observation.source_path(),
                    self.authority.database.file.file(),
                )
                .map_err(|error| match error {
                    SqliteSourceAccessError::SourceChanged
                    | SqliteSourceAccessError::ConnectionIdentityMismatch => {
                        CaptureError::SourceChangedDuringCapture
                    }
                    error => goose_sqlite_access_error(error),
                })
            })?;
        if !self.authority.revalidate()? {
            drop(connection);
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(connection)
    }

    pub(super) fn connection_ref<'a>(
        &self,
        connection: &'a SqliteSourceReadSnapshot,
    ) -> Result<&'a rusqlite::Connection> {
        connection.connection().map_err(goose_sqlite_access_error)
    }

    pub(super) fn finish_connection(&self, connection: SqliteSourceReadSnapshot) -> Result<bool> {
        match connection.finish() {
            Ok(_) => {}
            Err(
                SqliteSourceAccessError::SourceChanged
                | SqliteSourceAccessError::ConnectionIdentityMismatch,
            ) => return Ok(false),
            Err(error) => return Err(goose_sqlite_access_error(error)),
        }
        self.authority.revalidate()
    }

    pub(super) fn observation(&self) -> &GooseLiveObservation {
        &self.observation
    }

    #[cfg(test)]
    pub(super) fn snapshot_path(&self) -> &Path {
        self.observation.source_path()
    }

    pub(super) fn attempts(&self) -> u64 {
        self.attempts
    }

    pub(super) fn revalidate_live(&self) -> Result<bool> {
        self.authority.revalidate()
    }
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

fn goose_sidecar_relative_path(filename: &std::ffi::OsStr, suffix: &str) -> PathBuf {
    let mut sidecar = filename.to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn goose_absolute_authority_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(super) fn goose_sqlite_access_error(error: SqliteSourceAccessError) -> CaptureError {
    CaptureError::SystemIo {
        operation: "accessing a root-authorized Goose SQLite source",
        source: io::Error::other(error),
    }
}

fn goose_hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
