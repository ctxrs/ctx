use super::*;

mod native_file;

pub(super) use native_file::{
    validate_approved_parent_path, ExpectedObjectKind, NativeFileIdentity, NativeFileState,
};
use native_file::{validate_database_leaf, with_suffix};

#[cfg(test)]
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug)]
pub(super) struct SqliteSourceFamily {
    authority: SqliteSourceDirectoryAuthority,
    pub(super) database: SqliteFamilyMember,
    pub(super) wal: Option<SqliteFamilyMember>,
    pub(super) shared_memory: Option<SqliteFamilyMember>,
    wal_name: OsString,
    shared_memory_name: OsString,
    journal_name: OsString,
    wal_path: PathBuf,
    shared_memory_path: PathBuf,
    journal_path: PathBuf,
    #[cfg(test)]
    revalidation_count: AtomicU32,
}

impl SqliteSourceFamily {
    pub(super) fn approved_parent_path(&self) -> &Path {
        &self.authority.path
    }

    pub(super) fn database_name(&self) -> &OsStr {
        &self.database.name
    }

    pub(super) fn open(
        authority: &SqliteSourceDirectoryAuthority,
        database_name: &OsStr,
        after_parent_certification: impl FnOnce(),
    ) -> SqliteSourceAccessResult<Self> {
        validate_database_leaf(database_name)?;
        authority.revalidate()?;
        after_parent_certification();
        let retained_authority = authority.clone();
        let database_path = authority.path.join(database_name);
        let database = SqliteFamilyMember::open(
            &retained_authority,
            database_name.to_os_string(),
            database_path,
        )?;
        let wal_name = with_suffix(database_name, "-wal");
        let shared_memory_name = with_suffix(database_name, "-shm");
        let journal_name = with_suffix(database_name, "-journal");
        let wal_path = authority.path.join(&wal_name);
        let shared_memory_path = authority.path.join(&shared_memory_name);
        let journal_path = authority.path.join(&journal_name);
        let wal = SqliteFamilyMember::open_optional(
            &retained_authority,
            wal_name.clone(),
            wal_path.clone(),
        )?;
        let shared_memory = SqliteFamilyMember::open_optional(
            &retained_authority,
            shared_memory_name.clone(),
            shared_memory_path.clone(),
        )?;
        if SqliteFamilyMember::open_optional(
            &retained_authority,
            journal_name.clone(),
            journal_path.clone(),
        )?
        .is_some()
        {
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
            wal_name,
            shared_memory_name,
            journal_name,
            wal_path,
            shared_memory_path,
            journal_path,
            #[cfg(test)]
            revalidation_count: AtomicU32::new(0),
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

    /// Captures the bounded physical revision used only as conservative
    /// admitted-snapshot routing evidence. SHM content is deliberately not
    /// hashed because it is volatile reader coordination and is not part of
    /// the revision digest.
    pub(super) fn capture_revision_evidence(
        &self,
    ) -> SqliteSourceAccessResult<SqliteFamilyEvidence> {
        let shared_memory = self
            .shared_memory
            .as_ref()
            .map(SqliteFamilyMember::capture_state)
            .transpose()?;
        if let Some(state) = &shared_memory {
            enforce_member_length_bound(
                &self.shared_memory_path,
                state.length,
                SQLITE_SHM_MAX_BYTES,
            )?;
        }
        Ok(SqliteFamilyEvidence {
            parent_identity: self.authority.identity.clone(),
            database: self.database.capture_state()?,
            wal: self
                .wal
                .as_ref()
                .map(SqliteFamilyMember::capture_state)
                .transpose()?,
            shared_memory,
            wal_token: self
                .wal
                .as_ref()
                .map(SqliteFamilyMember::bounded_token)
                .transpose()?,
            shared_memory_token: None,
        })
    }

    /// Revalidates the authorized SQLite family topology without treating
    /// ordinary writes to retained members as source replacement.
    ///
    /// Logical-online-backup snapshots admit a SQLite read view, so DB/WAL/SHM
    /// metadata and bytes may advance after that view is pinned. The pathname
    /// topology may not: every member that existed at admission must still
    /// resolve to the same retained object, absent members must remain absent,
    /// and rollback journals remain unavailable. This closes named-open and
    /// checkpoint replacement races while allowing same-object WAL growth.
    pub(super) fn revalidate_logical_identity(
        &self,
        expected: &SqliteFamilyEvidence,
    ) -> SqliteSourceAccessResult<()> {
        #[cfg(test)]
        let _ =
            self.revalidation_count
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                    Some(count.saturating_add(1))
                });
        self.authority.revalidate()?;
        if self.authority.identity != expected.parent_identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        self.database
            .revalidate_identity(&self.authority, &expected.database.identity)?;
        revalidate_optional_member_identity(
            &self.authority,
            self.wal.as_ref(),
            expected.wal.as_ref(),
            &self.wal_name,
            &self.wal_path,
            None,
        )?;
        revalidate_optional_member_identity(
            &self.authority,
            self.shared_memory.as_ref(),
            expected.shared_memory.as_ref(),
            &self.shared_memory_name,
            &self.shared_memory_path,
            Some(SQLITE_SHM_MAX_BYTES),
        )?;
        if SqliteFamilyMember::open_optional(
            &self.authority,
            self.journal_name.clone(),
            self.journal_path.clone(),
        )
        .map_err(map_revalidation_error)?
        .is_some()
        {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        Ok(())
    }

    /// Revalidates the exact bounded DB/WAL revision used to admit a durable
    /// no-op replay. SHM bytes remain excluded because SQLite mutates reader
    /// coordination there, but its object identity and size bound remain
    /// certified.
    pub(super) fn revalidate_revision(
        &self,
        expected: &SqliteFamilyEvidence,
    ) -> SqliteSourceAccessResult<()> {
        self.authority.revalidate()?;
        if self.authority.identity != expected.parent_identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        self.database
            .revalidate(&self.authority, &expected.database)?;
        self.revalidate_wal(expected)?;
        revalidate_optional_member_identity(
            &self.authority,
            self.shared_memory.as_ref(),
            expected.shared_memory.as_ref(),
            &self.shared_memory_name,
            &self.shared_memory_path,
            Some(SQLITE_SHM_MAX_BYTES),
        )?;
        if SqliteFamilyMember::open_optional(
            &self.authority,
            self.journal_name.clone(),
            self.journal_path.clone(),
        )
        .map_err(map_revalidation_error)?
        .is_some()
        {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        Ok(())
    }

    /// Revalidates only the durable source identity after a private logical
    /// backup has been certified. WAL/SHM creation, checkpoint removal, and
    /// recreation are normal writer lifecycle after that point and cannot
    /// change the retained backup that will be published.
    pub(super) fn revalidate_logical_database_identity(
        &self,
        expected: &SqliteFamilyEvidence,
    ) -> SqliteSourceAccessResult<()> {
        #[cfg(test)]
        let _ =
            self.revalidation_count
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                    Some(count.saturating_add(1))
                });
        self.authority.revalidate()?;
        if self.authority.identity != expected.parent_identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        self.database
            .revalidate_identity(&self.authority, &expected.database.identity)
    }

    pub(super) fn revalidate(
        &self,
        expected: &SqliteFamilyEvidence,
    ) -> SqliteSourceAccessResult<()> {
        #[cfg(test)]
        let _ =
            self.revalidation_count
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                    Some(count.saturating_add(1))
                });
        self.authority.revalidate()?;
        if self.authority.identity != expected.parent_identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        self.database
            .revalidate(&self.authority, &expected.database)?;
        self.revalidate_wal(expected)?;
        revalidate_optional_member(
            &self.authority,
            self.shared_memory.as_ref(),
            expected.shared_memory.as_ref(),
            &self.shared_memory_name,
            &self.shared_memory_path,
        )?;
        match (
            self.shared_memory.as_ref(),
            expected.shared_memory_token.as_ref(),
        ) {
            (Some(shared_memory), Some(expected_token))
                if shared_memory
                    .content_digest()
                    .map_err(map_revalidation_error)?
                    == *expected_token => {}
            (None, None) => {}
            _ => return Err(SqliteSourceAccessError::SourceChanged),
        }
        if SqliteFamilyMember::open_optional(
            &self.authority,
            self.journal_name.clone(),
            self.journal_path.clone(),
        )
        .map_err(map_revalidation_error)?
        .is_some()
        {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        Ok(())
    }

    fn revalidate_wal(&self, expected: &SqliteFamilyEvidence) -> SqliteSourceAccessResult<()> {
        match expected.wal.as_ref() {
            None | Some(NativeFileState { length: 0, .. }) => {
                revalidate_empty_or_absent_wal(&self.authority, &self.wal_name, &self.wal_path)
            }
            Some(expected_state) => {
                let wal = self
                    .wal
                    .as_ref()
                    .ok_or(SqliteSourceAccessError::SourceChanged)?;
                wal.revalidate(&self.authority, expected_state)?;
                let expected_token = expected
                    .wal_token
                    .as_ref()
                    .ok_or(SqliteSourceAccessError::SourceChanged)?;
                let token = wal.bounded_token().map_err(map_revalidation_error)?;
                if &token == expected_token {
                    Ok(())
                } else {
                    Err(SqliteSourceAccessError::SourceChanged)
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn revalidation_count(&self) -> u32 {
        self.revalidation_count.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub(super) struct SqliteFamilyMember {
    opened: OpenedProviderSourceFile,
    name: OsString,
    pub(super) path: PathBuf,
}

impl SqliteFamilyMember {
    pub(super) fn open(
        authority: &SqliteSourceDirectoryAuthority,
        name: OsString,
        path: PathBuf,
    ) -> SqliteSourceAccessResult<Self> {
        match authority.directory.open_child(&name) {
            Ok(OpenedProviderSourcePath::File(opened)) => {
                NativeFileState::read(opened.file(), &path, ExpectedObjectKind::RegularFile)?;
                Ok(Self { opened, name, path })
            }
            Ok(OpenedProviderSourcePath::Directory(_)) => {
                Err(SqliteSourceAccessError::UnsafeFile {
                    path,
                    reason: "SQLite source family members must be regular files",
                })
            }
            Err(error) => Err(map_provider_source_error(
                error,
                "opening a SQLite source family member relative to retained authority",
                &path,
            )),
        }
    }

    fn open_optional(
        authority: &SqliteSourceDirectoryAuthority,
        name: OsString,
        path: PathBuf,
    ) -> SqliteSourceAccessResult<Option<Self>> {
        match Self::open(authority, name, path) {
            Ok(member) => Ok(Some(member)),
            Err(SqliteSourceAccessError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn file(&self) -> &File {
        self.opened.file()
    }

    pub(super) fn capture_state(&self) -> SqliteSourceAccessResult<NativeFileState> {
        NativeFileState::read(
            self.opened.file(),
            &self.path,
            ExpectedObjectKind::RegularFile,
        )
    }

    fn revalidate(
        &self,
        authority: &SqliteSourceDirectoryAuthority,
        expected: &NativeFileState,
    ) -> SqliteSourceAccessResult<()> {
        let retained = self.capture_state().map_err(map_revalidation_error)?;
        if &retained != expected {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        let named = Self::open(authority, self.name.clone(), self.path.clone())
            .map_err(map_revalidation_error)?;
        let named_state = named.capture_state().map_err(map_revalidation_error)?;
        if &named_state == expected {
            Ok(())
        } else {
            Err(SqliteSourceAccessError::SourceChanged)
        }
    }

    fn revalidate_identity(
        &self,
        authority: &SqliteSourceDirectoryAuthority,
        expected: &NativeFileIdentity,
    ) -> SqliteSourceAccessResult<()> {
        let retained = self.capture_state().map_err(map_revalidation_error)?;
        if &retained.identity != expected {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        let named = Self::open(authority, self.name.clone(), self.path.clone())
            .map_err(map_revalidation_error)?;
        let named_state = named.capture_state().map_err(map_revalidation_error)?;
        if &named_state.identity == expected {
            Ok(())
        } else {
            Err(SqliteSourceAccessError::SourceChanged)
        }
    }

    fn bounded_token(&self) -> SqliteSourceAccessResult<[u8; 32]> {
        let state = self.capture_state()?;
        let mut file =
            self.opened
                .file()
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
        enforce_member_length_bound(&self.path, state.length, SQLITE_SHM_MAX_BYTES)?;
        let mut file =
            self.opened
                .file()
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

fn enforce_member_length_bound(
    path: &Path,
    length: u64,
    maximum: u64,
) -> SqliteSourceAccessResult<()> {
    if length > maximum {
        Err(SqliteSourceAccessError::SnapshotTooLarge {
            path: path.to_path_buf(),
            length,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn revalidate_optional_member_identity(
    authority: &SqliteSourceDirectoryAuthority,
    member: Option<&SqliteFamilyMember>,
    expected: Option<&NativeFileState>,
    name: &OsStr,
    path: &Path,
    maximum_length: Option<u64>,
) -> SqliteSourceAccessResult<()> {
    match (member, expected) {
        (Some(member), Some(expected)) => {
            let retained = member.capture_state().map_err(map_revalidation_error)?;
            if let Some(maximum) = maximum_length {
                enforce_member_length_bound(path, retained.length, maximum)?;
            }
            if retained.identity != expected.identity {
                return Err(SqliteSourceAccessError::SourceChanged);
            }
            let named =
                SqliteFamilyMember::open(authority, name.to_os_string(), path.to_path_buf())
                    .map_err(map_revalidation_error)?;
            let named_state = named.capture_state().map_err(map_revalidation_error)?;
            if let Some(maximum) = maximum_length {
                enforce_member_length_bound(path, named_state.length, maximum)?;
            }
            if named_state.identity == expected.identity {
                Ok(())
            } else {
                Err(SqliteSourceAccessError::SourceChanged)
            }
        }
        (None, None) => {
            if SqliteFamilyMember::open_optional(authority, name.to_os_string(), path.to_path_buf())
                .map_err(map_revalidation_error)?
                .is_some()
            {
                Err(SqliteSourceAccessError::SourceChanged)
            } else {
                Ok(())
            }
        }
        _ => Err(SqliteSourceAccessError::SourceChanged),
    }
}

fn revalidate_optional_member(
    authority: &SqliteSourceDirectoryAuthority,
    member: Option<&SqliteFamilyMember>,
    expected: Option<&NativeFileState>,
    name: &OsStr,
    path: &Path,
) -> SqliteSourceAccessResult<()> {
    match (member, expected) {
        (Some(member), Some(expected)) => member.revalidate(authority, expected),
        (None, None) => {
            if SqliteFamilyMember::open_optional(authority, name.to_os_string(), path.to_path_buf())
                .map_err(map_revalidation_error)?
                .is_some()
            {
                Err(SqliteSourceAccessError::SourceChanged)
            } else {
                Ok(())
            }
        }
        _ => Err(SqliteSourceAccessError::SourceChanged),
    }
}

fn revalidate_empty_or_absent_wal(
    authority: &SqliteSourceDirectoryAuthority,
    name: &OsStr,
    path: &Path,
) -> SqliteSourceAccessResult<()> {
    let Some(wal) =
        SqliteFamilyMember::open_optional(authority, name.to_os_string(), path.to_path_buf())
            .map_err(map_revalidation_error)?
    else {
        return Ok(());
    };
    let state = wal.capture_state().map_err(map_revalidation_error)?;
    if state.length != 0 {
        return Err(SqliteSourceAccessError::SourceChanged);
    }
    wal.revalidate(authority, &state)
        .map_err(map_revalidation_error)
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
            wal_length: native
                .wal
                .as_ref()
                .and_then(|state| (state.length != 0).then_some(state.length)),
            shared_memory_length: native.shared_memory.as_ref().map(|state| state.length),
            schema: sqlite.schema.clone(),
            source: sqlite.source.clone(),
            revision: revision.finalize().into(),
        }
    }
}

impl SqliteFamilyEvidence {
    pub(super) fn revision_token(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(EVIDENCE_DOMAIN);
        digest.update(b"admitted-revision\0");
        self.hash_into(&mut digest);
        digest.finalize().into()
    }

    fn hash_into(&self, digest: &mut Sha256) {
        self.parent_identity.hash_into(digest);
        self.database.hash_into(digest);
        let committed_wal = self.wal.as_ref().filter(|state| state.length != 0);
        hash_optional_state(digest, committed_wal);
        // SHM is SQLite's volatile lock coordination, not provider content.
        // Stock read-only WAL readers may update its reader marks, so source
        // revisions intentionally derive from the DB, WAL, and SQLite evidence.
        match committed_wal.and(self.wal_token) {
            Some(wal_token) => {
                digest.update([1]);
                digest.update(wal_token);
            }
            None => digest.update([0]),
        }
    }
}

impl SqliteSnapshotEvidence {
    pub(super) fn same_database_view(&self, other: &Self) -> bool {
        // SQLite's backup API may advance the destination schema cookie so an
        // already-open destination connection reloads its schema. The provider
        // user/application versions and exact page inventory remain stable.
        self.schema.user_version == other.schema.user_version
            && self.schema.application_id == other.schema.application_id
            && self.source.page_count == other.source.page_count
            && self.source.freelist_count == other.source.freelist_count
    }

    fn hash_into(&self, digest: &mut Sha256) {
        digest.update(self.schema.schema_version.to_le_bytes());
        digest.update(self.schema.user_version.to_le_bytes());
        digest.update(self.schema.application_id.to_le_bytes());
        // data_version is connection-local and can differ solely because an
        // equivalent source used the immutable-main versus copied-family path.
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

pub(super) fn map_provider_source_error(
    error: CaptureError,
    operation: &'static str,
    path: &Path,
) -> SqliteSourceAccessError {
    match error {
        CaptureError::Io(source) => SqliteSourceAccessError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        },
        CaptureError::InvalidProviderTranscriptPath { reason, .. } => {
            SqliteSourceAccessError::UnsafeFile {
                path: path.to_path_buf(),
                reason,
            }
        }
        CaptureError::SourceChangedDuringCapture => SqliteSourceAccessError::SourceChanged,
        error => SqliteSourceAccessError::SnapshotUnavailable {
            reason: format!("{operation} for {path:?} failed: {error}"),
        },
    }
}

pub(super) fn map_provider_source_revalidation_error(
    error: CaptureError,
    operation: &'static str,
    path: &Path,
) -> SqliteSourceAccessError {
    map_revalidation_error(map_provider_source_error(error, operation, path))
}

pub(super) fn map_revalidation_io_error(
    source: std::io::Error,
    operation: &'static str,
    path: &Path,
) -> SqliteSourceAccessError {
    map_revalidation_error(SqliteSourceAccessError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    })
}

pub(super) fn map_revalidation_error(error: SqliteSourceAccessError) -> SqliteSourceAccessError {
    match error {
        SqliteSourceAccessError::Io {
            operation,
            path,
            source,
        } if resource_exhaustion_io_error(&source) => {
            SqliteSourceAccessError::ResourceUnavailable {
                operation,
                path,
                source,
            }
        }
        error if error.is_systemic_resource_failure() => error,
        _ => SqliteSourceAccessError::SourceChanged,
    }
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
    // SQLite has no connection-local filesystem temp-directory authority.
    // Shared snapshots therefore permit only memory temp state for their
    // bounded/indexed queries. The one unindexed OpenCode corpus ordering path
    // uses its own size-capped ordinary database under the ctx data root.
    connection
        .pragma_update(None, "temp_store", "MEMORY")
        .map_err(|source| sqlite_error("disabling provider SQLite temporary files", source))?;
    connection
        .pragma_update(None, "mmap_size", 0_i64)
        .map_err(|source| sqlite_error("disabling provider database mmap", source))?;
    let query_only: i64 = connection
        .pragma_query_value(None, "query_only", |row| row.get(0))
        .map_err(|source| sqlite_error("verifying provider query-only mode", source))?;
    if query_only != 1 {
        return Err(SqliteSourceAccessError::ConnectionNotQueryOnly);
    }
    let temp_store: i64 = connection
        .pragma_query_value(None, "temp_store", |row| row.get(0))
        .map_err(|source| sqlite_error("verifying provider temporary storage", source))?;
    if temp_store != 2 {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "provider SQLite temporary storage is not memory-only".to_owned(),
        });
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
