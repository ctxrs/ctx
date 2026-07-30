use super::*;

/// Retained DB/WAL authority for a bounded physical-revision comparison.
///
/// SHM is intentionally neither opened nor represented: its reader marks are
/// volatile lock coordination. Revalidation relies on exact retained/named
/// DB and WAL metadata, so the bounded content tokens are read only once.
#[derive(Debug)]
pub(super) struct SqlitePhysicalRevisionFamily {
    authority: SqliteSourceDirectoryAuthority,
    database: SqliteFamilyMember,
    wal: Option<SqliteFamilyMember>,
    wal_name: OsString,
    journal_name: OsString,
    wal_path: PathBuf,
    journal_path: PathBuf,
}

impl SqlitePhysicalRevisionFamily {
    pub(super) fn open(
        authority: &SqliteSourceDirectoryAuthority,
        database_name: &OsStr,
    ) -> SqliteSourceAccessResult<Self> {
        validate_database_leaf(database_name)?;
        authority.revalidate()?;
        let retained_authority = authority.clone();
        let database = SqliteFamilyMember::open(
            &retained_authority,
            database_name.to_os_string(),
            authority.path.join(database_name),
        )?;
        let wal_name = with_suffix(database_name, "-wal");
        let journal_name = with_suffix(database_name, "-journal");
        let wal_path = authority.path.join(&wal_name);
        let journal_path = authority.path.join(&journal_name);
        let wal = SqliteFamilyMember::open_optional(
            &retained_authority,
            wal_name.clone(),
            wal_path.clone(),
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
                capability: "physical replay does not perform rollback recovery",
            });
        }
        Ok(Self {
            authority: retained_authority,
            database,
            wal,
            wal_name,
            journal_name,
            wal_path,
            journal_path,
        })
    }

    pub(super) fn capture_evidence(
        &self,
    ) -> SqliteSourceAccessResult<(SqliteFamilyEvidence, u64, u64)> {
        let database = self.database.capture_state()?;
        let (database_token, database_bytes_read) = self.database.bounded_token_with_bytes()?;
        let wal = self
            .wal
            .as_ref()
            .map(SqliteFamilyMember::capture_state)
            .transpose()?;
        let (wal_token, wal_bytes_read) = match self.wal.as_ref() {
            Some(wal) => {
                let (token, bytes_read) = wal.bounded_token_with_bytes()?;
                (Some(token), bytes_read)
            }
            None => (None, 0),
        };
        let evidence = SqliteFamilyEvidence {
            parent_identity: self.authority.identity.clone(),
            database,
            database_token,
            wal,
            shared_memory: None,
            wal_token,
            shared_memory_token: None,
        };
        self.revalidate(&evidence)?;
        Ok((evidence, database_bytes_read, wal_bytes_read))
    }

    pub(super) fn revalidate(
        &self,
        expected: &SqliteFamilyEvidence,
    ) -> SqliteSourceAccessResult<()> {
        self.authority.revalidate()?;
        if self.authority.identity != expected.parent_identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        self.database
            .revalidate(&self.authority, &expected.database)?;
        revalidate_optional_member(
            &self.authority,
            self.wal.as_ref(),
            expected.wal.as_ref(),
            &self.wal_name,
            &self.wal_path,
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
}
