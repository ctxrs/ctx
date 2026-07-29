use super::*;

#[derive(Debug)]
pub(super) struct SqliteSourceFamily {
    authority: SqliteSourceDirectoryAuthority,
    pub(super) database: SqliteFamilyMember,
    pub(super) wal: Option<SqliteFamilyMember>,
    pub(super) shared_memory: Option<SqliteFamilyMember>,
    wal_path: PathBuf,
    shared_memory_path: PathBuf,
    journal_path: PathBuf,
}

impl SqliteSourceFamily {
    pub(super) fn open(
        authority: &SqliteSourceDirectoryAuthority,
        database_name: &OsStr,
    ) -> SqliteSourceAccessResult<Self> {
        validate_database_leaf(database_name)?;
        authority.revalidate()?;
        let retained_authority = SqliteSourceDirectoryAuthority {
            directory: authority.directory.try_clone().map_err(|source| {
                SqliteSourceAccessError::Io {
                    operation: "retaining the SQLite source parent",
                    path: authority.path.clone(),
                    source,
                }
            })?,
            path: authority.path.clone(),
            identity: authority.identity.clone(),
        };
        let database_path = authority.path.join(database_name);
        let database = SqliteFamilyMember::open(database_path, ExpectedObjectKind::RegularFile)?;
        let wal_path = authority.path.join(with_suffix(database_name, "-wal"));
        let shared_memory_path = authority.path.join(with_suffix(database_name, "-shm"));
        let journal_path = authority.path.join(with_suffix(database_name, "-journal"));
        let wal = SqliteFamilyMember::open_optional(wal_path.clone())?;
        let shared_memory = SqliteFamilyMember::open_optional(shared_memory_path.clone())?;
        if SqliteFamilyMember::path_exists(&journal_path)? {
            return Err(SqliteSourceAccessError::UnsupportedSidecarIdentity {
                component: SqliteSourceComponent::RollbackJournal,
                capability: "read-only provider snapshots do not perform rollback recovery",
            });
        }
        Ok(Self {
            authority: retained_authority,
            database,
            wal,
            shared_memory,
            wal_path,
            shared_memory_path,
            journal_path,
        })
    }

    pub(super) fn capture_evidence(&self) -> SqliteSourceAccessResult<SqliteFamilyEvidence> {
        Ok(SqliteFamilyEvidence {
            parent_identity: self.authority.identity.clone(),
            database: self.database.capture_state()?,
            wal: self
                .wal
                .as_ref()
                .map(SqliteFamilyMember::capture_state)
                .transpose()?,
            shared_memory: self
                .shared_memory
                .as_ref()
                .map(SqliteFamilyMember::capture_state)
                .transpose()?,
            wal_token: self
                .wal
                .as_ref()
                .map(SqliteFamilyMember::bounded_token)
                .transpose()?,
            shared_memory_token: self
                .shared_memory
                .as_ref()
                .map(SqliteFamilyMember::content_digest)
                .transpose()?,
        })
    }

    pub(super) fn revalidate(
        &self,
        expected: &SqliteFamilyEvidence,
    ) -> SqliteSourceAccessResult<()> {
        self.authority.revalidate()?;
        if self.authority.identity != expected.parent_identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        self.database.revalidate(&expected.database)?;
        revalidate_optional_member(self.wal.as_ref(), expected.wal.as_ref(), &self.wal_path)?;
        revalidate_optional_member(
            self.shared_memory.as_ref(),
            expected.shared_memory.as_ref(),
            &self.shared_memory_path,
        )?;
        match (self.wal.as_ref(), expected.wal_token.as_ref()) {
            (Some(wal), Some(expected_token)) if wal.bounded_token()? == *expected_token => {}
            (None, None) => {}
            _ => return Err(SqliteSourceAccessError::SourceChanged),
        }
        match (
            self.shared_memory.as_ref(),
            expected.shared_memory_token.as_ref(),
        ) {
            (Some(shared_memory), Some(expected_token))
                if shared_memory.content_digest()? == *expected_token => {}
            (None, None) => {}
            _ => return Err(SqliteSourceAccessError::SourceChanged),
        }
        if SqliteFamilyMember::path_exists(&self.journal_path).map_err(map_revalidation_error)? {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct SqliteFamilyMember {
    pub(super) file: File,
    pub(super) path: PathBuf,
}

impl SqliteFamilyMember {
    fn open(path: PathBuf, kind: ExpectedObjectKind) -> SqliteSourceAccessResult<Self> {
        let file = open_nofollow(&path, kind)?;
        NativeFileState::read(&file, &path, kind)?;
        Ok(Self { file, path })
    }

    fn open_optional(path: PathBuf) -> SqliteSourceAccessResult<Option<Self>> {
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                validate_named_metadata(&path, &metadata, ExpectedObjectKind::RegularFile)?;
                Self::open(path, ExpectedObjectKind::RegularFile).map(Some)
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(SqliteSourceAccessError::Io {
                operation: "inspecting an optional SQLite source member",
                path,
                source,
            }),
        }
    }

    fn path_exists(path: &Path) -> SqliteSourceAccessResult<bool> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                validate_named_metadata(path, &metadata, ExpectedObjectKind::RegularFile)?;
                Ok(true)
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(SqliteSourceAccessError::Io {
                operation: "inspecting an optional SQLite source member",
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn capture_state(&self) -> SqliteSourceAccessResult<NativeFileState> {
        NativeFileState::read(&self.file, &self.path, ExpectedObjectKind::RegularFile)
    }

    fn revalidate(&self, expected: &NativeFileState) -> SqliteSourceAccessResult<()> {
        let retained = self.capture_state().map_err(map_revalidation_error)?;
        if &retained != expected {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        let named = open_nofollow(&self.path, ExpectedObjectKind::RegularFile)
            .map_err(map_revalidation_error)?;
        let named_state =
            NativeFileState::read(&named, &self.path, ExpectedObjectKind::RegularFile)
                .map_err(map_revalidation_error)?;
        if &named_state == expected {
            Ok(())
        } else {
            Err(SqliteSourceAccessError::SourceChanged)
        }
    }

    fn bounded_token(&self) -> SqliteSourceAccessResult<[u8; 32]> {
        let state = self.capture_state()?;
        let mut file = self
            .file
            .try_clone()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "retaining the SQLite WAL for bounded revision evidence",
                path: self.path.clone(),
                source,
            })?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "seeking the SQLite WAL for bounded revision evidence",
                path: self.path.clone(),
                source,
            })?;
        let prefix_len = usize::try_from(state.length.min(SQLITE_WAL_TOKEN_BYTES as u64))
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        let mut prefix = vec![0_u8; prefix_len];
        file.read_exact(&mut prefix)
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading the SQLite WAL prefix for bounded revision evidence",
                path: self.path.clone(),
                source,
            })?;
        let suffix_len = prefix_len;
        let mut suffix = vec![0_u8; suffix_len];
        if suffix_len > 0 {
            file.seek(SeekFrom::Start(state.length - suffix_len as u64))
                .map_err(|source| SqliteSourceAccessError::Io {
                    operation: "seeking the SQLite WAL suffix for bounded revision evidence",
                    path: self.path.clone(),
                    source,
                })?;
            file.read_exact(&mut suffix)
                .map_err(|source| SqliteSourceAccessError::Io {
                    operation: "reading the SQLite WAL suffix for bounded revision evidence",
                    path: self.path.clone(),
                    source,
                })?;
        }
        let mut digest = Sha256::new();
        digest.update(state.length.to_le_bytes());
        digest.update(prefix);
        digest.update(suffix);
        Ok(digest.finalize().into())
    }

    fn content_digest(&self) -> SqliteSourceAccessResult<[u8; 32]> {
        let state = self.capture_state()?;
        if state.length > SQLITE_SHM_MAX_BYTES {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: self.path.clone(),
                length: state.length,
                maximum: SQLITE_SHM_MAX_BYTES,
            });
        }
        let mut file = self
            .file
            .try_clone()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "retaining SQLite SHM for bounded content evidence",
                path: self.path.clone(),
                source,
            })?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "seeking SQLite SHM for bounded content evidence",
                path: self.path.clone(),
                source,
            })?;
        let mut remaining = state.length;
        let mut buffer = vec![0_u8; SQLITE_COPY_BUFFER_BYTES];
        let mut digest = Sha256::new();
        digest.update(state.length.to_le_bytes());
        while remaining > 0 {
            let requested = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
            file.read_exact(&mut buffer[..requested])
                .map_err(|source| SqliteSourceAccessError::Io {
                    operation: "reading SQLite SHM for bounded content evidence",
                    path: self.path.clone(),
                    source,
                })?;
            digest.update(&buffer[..requested]);
            remaining -= requested as u64;
        }
        Ok(digest.finalize().into())
    }
}

fn revalidate_optional_member(
    member: Option<&SqliteFamilyMember>,
    expected: Option<&NativeFileState>,
    path: &Path,
) -> SqliteSourceAccessResult<()> {
    match (member, expected) {
        (Some(member), Some(expected)) => member.revalidate(expected),
        (None, None) => {
            if SqliteFamilyMember::path_exists(path).map_err(map_revalidation_error)? {
                Err(SqliteSourceAccessError::SourceChanged)
            } else {
                Ok(())
            }
        }
        _ => Err(SqliteSourceAccessError::SourceChanged),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SqliteFamilyEvidence {
    parent_identity: NativeFileIdentity,
    pub(super) database: NativeFileState,
    pub(super) wal: Option<NativeFileState>,
    shared_memory: Option<NativeFileState>,
    wal_token: Option<[u8; 32]>,
    shared_memory_token: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SqliteSnapshotEvidence {
    schema: SqliteSchemaEvidence,
    source: SqliteConnectionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SqliteSchemaEvidence {
    schema_version: i64,
    user_version: i64,
    application_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SqliteConnectionEvidence {
    data_version: i64,
    page_count: i64,
    freelist_count: i64,
}

impl SqliteSourceEvidence {
    pub(super) fn from_snapshot(
        native: &SqliteFamilyEvidence,
        sqlite: &SqliteSnapshotEvidence,
    ) -> Self {
        let identity = native.database.identity.digest();
        let mut revision = Sha256::new();
        revision.update(EVIDENCE_DOMAIN);
        revision.update(b"revision\0");
        native.hash_into(&mut revision);
        sqlite.hash_into(&mut revision);
        Self {
            identity,
            length: native.database.length,
            wal_length: native.wal.as_ref().map(|state| state.length),
            shared_memory_length: native.shared_memory.as_ref().map(|state| state.length),
            schema: sqlite.schema.clone(),
            source: sqlite.source.clone(),
            revision: revision.finalize().into(),
        }
    }
}

impl SqliteFamilyEvidence {
    fn hash_into(&self, digest: &mut Sha256) {
        self.parent_identity.hash_into(digest);
        self.database.hash_into(digest);
        hash_optional_state(digest, self.wal.as_ref());
        // SHM is SQLite's volatile lock coordination, not provider content.
        // Stock read-only WAL readers may update its reader marks, so source
        // revisions intentionally derive from the DB, WAL, and SQLite evidence.
        match self.wal_token {
            Some(wal_token) => {
                digest.update([1]);
                digest.update(wal_token);
            }
            None => digest.update([0]),
        }
    }
}

impl SqliteSnapshotEvidence {
    fn hash_into(&self, digest: &mut Sha256) {
        digest.update(self.schema.schema_version.to_le_bytes());
        digest.update(self.schema.user_version.to_le_bytes());
        digest.update(self.schema.application_id.to_le_bytes());
        digest.update(self.source.data_version.to_le_bytes());
        digest.update(self.source.page_count.to_le_bytes());
        digest.update(self.source.freelist_count.to_le_bytes());
    }
}

fn hash_optional_state(digest: &mut Sha256, state: Option<&NativeFileState>) {
    match state {
        Some(state) => {
            digest.update([1]);
            state.hash_into(digest);
        }
        None => digest.update([0]),
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ExpectedObjectKind {
    Directory,
    RegularFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeFileState {
    pub(super) identity: NativeFileIdentity,
    pub(super) length: u64,
    platform: PlatformFileState,
}

impl NativeFileState {
    pub(super) fn read(
        file: &File,
        path: &Path,
        expected_kind: ExpectedObjectKind,
    ) -> SqliteSourceAccessResult<Self> {
        let metadata = file
            .metadata()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading retained SQLite source metadata",
                path: path.to_path_buf(),
                source,
            })?;
        validate_opened_metadata(path, &metadata, expected_kind)?;
        let (identity, platform) =
            platform_file_state(file, &metadata).map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading native SQLite source identity",
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            identity,
            length: metadata.len(),
            platform,
        })
    }

    fn hash_into(&self, digest: &mut Sha256) {
        self.identity.hash_into(digest);
        digest.update(self.length.to_le_bytes());
        self.platform.hash_into(digest);
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeFileIdentity {
    volume_serial_number: u64,
    file_id: [u8; 16],
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NativeFileIdentity;

impl NativeFileIdentity {
    fn digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(EVIDENCE_DOMAIN);
        digest.update(b"identity\0");
        self.hash_into(&mut digest);
        digest.finalize().into()
    }

    fn hash_into(&self, digest: &mut Sha256) {
        #[cfg(unix)]
        {
            digest.update(self.device.to_le_bytes());
            digest.update(self.inode.to_le_bytes());
        }
        #[cfg(windows)]
        {
            digest.update(self.volume_serial_number.to_le_bytes());
            digest.update(self.file_id);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = digest;
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlatformFileState {
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlatformFileState {
    creation_time: i64,
    last_write_time: i64,
    change_time: i64,
    attributes: u32,
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlatformFileState;

impl PlatformFileState {
    fn hash_into(&self, digest: &mut Sha256) {
        #[cfg(unix)]
        {
            digest.update(self.mode.to_le_bytes());
            digest.update(self.modified_seconds.to_le_bytes());
            digest.update(self.modified_nanoseconds.to_le_bytes());
            digest.update(self.changed_seconds.to_le_bytes());
            digest.update(self.changed_nanoseconds.to_le_bytes());
        }
        #[cfg(windows)]
        {
            digest.update(self.creation_time.to_le_bytes());
            digest.update(self.last_write_time.to_le_bytes());
            digest.update(self.change_time.to_le_bytes());
            digest.update(self.attributes.to_le_bytes());
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = digest;
        }
    }
}

#[cfg(unix)]
fn platform_file_state(
    _file: &File,
    metadata: &Metadata,
) -> std::io::Result<(NativeFileIdentity, PlatformFileState)> {
    use std::os::unix::fs::MetadataExt;

    Ok((
        NativeFileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        PlatformFileState {
            mode: metadata.mode(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        },
    ))
}

#[cfg(windows)]
fn platform_file_state(
    file: &File,
    _metadata: &Metadata,
) -> std::io::Result<(NativeFileIdentity, PlatformFileState)> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle();
    let mut basic = FILE_BASIC_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut id = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok((
        NativeFileIdentity {
            volume_serial_number: id.VolumeSerialNumber,
            file_id: id.FileId.Identifier,
        },
        PlatformFileState {
            creation_time: basic.CreationTime,
            last_write_time: basic.LastWriteTime,
            change_time: basic.ChangeTime,
            attributes: basic.FileAttributes,
        },
    ))
}

#[cfg(not(any(unix, windows)))]
fn platform_file_state(
    _file: &File,
    _metadata: &Metadata,
) -> std::io::Result<(NativeFileIdentity, PlatformFileState)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "native SQLite source identity is unsupported on this platform",
    ))
}

pub(super) fn validate_approved_parent_path(path: &Path) -> SqliteSourceAccessResult<()> {
    if !path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: "the approved SQLite parent path must be absolute and traversal-free",
        });
    }
    Ok(())
}

fn validate_database_leaf(name: &OsStr) -> SqliteSourceAccessResult<()> {
    let path = Path::new(name);
    if name.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: "the SQLite database name must be one normal leaf component",
        });
    }
    Ok(())
}

fn with_suffix(name: &OsStr, suffix: &str) -> OsString {
    let mut value = name.to_os_string();
    value.push(suffix);
    value
}

pub(super) fn open_nofollow(
    path: &Path,
    expected_kind: ExpectedObjectKind,
) -> SqliteSourceAccessResult<File> {
    let metadata = fs::symlink_metadata(path).map_err(|source| SqliteSourceAccessError::Io {
        operation: "inspecting a named SQLite source object",
        path: path.to_path_buf(),
        source,
    })?;
    validate_named_metadata(path, &metadata, expected_kind)?;
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow_open(&mut options, expected_kind);
    options
        .open(path)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "opening a named SQLite source object without following",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn configure_nofollow_open(options: &mut OpenOptions, _expected_kind: ExpectedObjectKind) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
}

#[cfg(windows)]
fn configure_nofollow_open(options: &mut OpenOptions, expected_kind: ExpectedObjectKind) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
    if matches!(expected_kind, ExpectedObjectKind::Directory) {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    options
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags);
}

#[cfg(not(any(unix, windows)))]
fn configure_nofollow_open(_options: &mut OpenOptions, _expected_kind: ExpectedObjectKind) {}

fn validate_named_metadata(
    path: &Path,
    metadata: &Metadata,
    expected_kind: ExpectedObjectKind,
) -> SqliteSourceAccessResult<()> {
    validate_opened_metadata(path, metadata, expected_kind)
}

fn validate_opened_metadata(
    path: &Path,
    metadata: &Metadata,
    expected_kind: ExpectedObjectKind,
) -> SqliteSourceAccessResult<()> {
    if metadata.file_type().is_symlink() || metadata_is_reparse_point(metadata) {
        return Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: "symlink and reparse-point SQLite source objects are not allowed",
        });
    }
    let valid = match expected_kind {
        ExpectedObjectKind::Directory => metadata.is_dir(),
        ExpectedObjectKind::RegularFile => metadata.file_type().is_file(),
    };
    if valid {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: match expected_kind {
                ExpectedObjectKind::Directory => "the approved SQLite parent must be a directory",
                ExpectedObjectKind::RegularFile => {
                    "SQLite source family members must be regular files"
                }
            },
        })
    }
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &Metadata) -> bool {
    false
}

pub(super) fn map_revalidation_error(error: SqliteSourceAccessError) -> SqliteSourceAccessError {
    let _ = error;
    SqliteSourceAccessError::SourceChanged
}

pub(super) fn configure_and_pin_snapshot(connection: &Connection) -> SqliteSourceAccessResult<()> {
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)
        .map_err(|source| sqlite_error("disabling trusted provider schemas", source))?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)
        .map_err(|source| sqlite_error("disabling provider triggers", source))?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .map_err(|source| sqlite_error("disabling provider WAL checkpoint-on-close", source))?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(|source| sqlite_error("enabling provider query-only mode", source))?;
    connection
        .pragma_update(None, "temp_store", "MEMORY")
        .map_err(|source| sqlite_error("forcing in-memory SQLite temporary storage", source))?;
    connection
        .pragma_update(None, "mmap_size", 0_i64)
        .map_err(|source| sqlite_error("disabling provider database mmap", source))?;
    let query_only: i64 = connection
        .pragma_query_value(None, "query_only", |row| row.get(0))
        .map_err(|source| sqlite_error("verifying provider query-only mode", source))?;
    if query_only != 1 {
        return Err(SqliteSourceAccessError::ConnectionNotQueryOnly);
    }
    connection
        .execute_batch("BEGIN DEFERRED")
        .map_err(|source| sqlite_error("starting the provider read snapshot", source))?;
    connection
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|_| ())
        .map_err(|source| sqlite_error("pinning the provider read snapshot", source))?;
    install_snapshot_authorizer(connection)
}

pub(super) fn capture_sqlite_evidence(
    connection: &Connection,
) -> SqliteSourceAccessResult<SqliteSnapshotEvidence> {
    Ok(SqliteSnapshotEvidence {
        schema: SqliteSchemaEvidence {
            schema_version: pragma_i64(connection, "schema_version")?,
            user_version: pragma_i64(connection, "user_version")?,
            application_id: pragma_i64(connection, "application_id")?,
        },
        source: SqliteConnectionEvidence {
            data_version: pragma_i64(connection, "data_version")?,
            page_count: pragma_i64(connection, "page_count")?,
            freelist_count: pragma_i64(connection, "freelist_count")?,
        },
    })
}

fn pragma_i64(connection: &Connection, name: &'static str) -> SqliteSourceAccessResult<i64> {
    connection
        .pragma_query_value(None, name, |row| row.get(0))
        .map_err(|source| sqlite_error("capturing provider SQLite evidence", source))
}

unsafe extern "C" fn deny_snapshot_transaction_control(
    _context: *mut c_void,
    action: i32,
    _argument_one: *const c_char,
    _argument_two: *const c_char,
    _database: *const c_char,
    _trigger: *const c_char,
) -> i32 {
    if matches!(action, ffi::SQLITE_TRANSACTION | ffi::SQLITE_SAVEPOINT) {
        ffi::SQLITE_DENY
    } else {
        ffi::SQLITE_OK
    }
}

fn install_snapshot_authorizer(connection: &Connection) -> SqliteSourceAccessResult<()> {
    let code = unsafe {
        ffi::sqlite3_set_authorizer(
            connection.handle(),
            Some(deny_snapshot_transaction_control),
            ptr::null_mut(),
        )
    };
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SqliteControl {
            operation: "installing the provider snapshot transaction guard",
            code,
        })
    }
}

pub(super) fn clear_snapshot_authorizer(connection: &Connection) -> SqliteSourceAccessResult<()> {
    let code = unsafe { ffi::sqlite3_set_authorizer(connection.handle(), None, ptr::null_mut()) };
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SqliteControl {
            operation: "clearing the provider snapshot transaction guard",
            code,
        })
    }
}

pub(super) fn verify_snapshot_active(connection: &Connection) -> SqliteSourceAccessResult<()> {
    if unsafe { ffi::sqlite3_get_autocommit(connection.handle()) } == 0 {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SnapshotNotActive)
    }
}

pub(super) fn verify_connection_read_only(connection: &Connection) -> SqliteSourceAccessResult<()> {
    let readonly = unsafe { ffi::sqlite3_db_readonly(connection.handle(), c"main".as_ptr()) };
    if readonly == 1 {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::ConnectionNotReadOnly)
    }
}

pub(super) fn sqlite_error(
    operation: &'static str,
    source: rusqlite::Error,
) -> SqliteSourceAccessError {
    SqliteSourceAccessError::Sqlite { operation, source }
}
