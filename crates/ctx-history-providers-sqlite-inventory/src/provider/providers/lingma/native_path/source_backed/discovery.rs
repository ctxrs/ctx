use std::path::{Path, PathBuf};

#[cfg(test)]
use std::ffi::OsString;

use ctx_history_core::{
    CaptureProvider, SourceAnchorScope, SourceInventoryObservation, SourceKey, TypedKey,
};
use sha2::{Digest, Sha256};

#[cfg(test)]
use crate::{
    common::io::ProviderSourceRoot,
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot,
    },
    CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::{
    lingma_source_key_scoped, LingmaSourceBackedErrorV0, LingmaSourceBackedResultV0,
    INVENTORY_AUTHORITY_NAMESPACE, INVENTORY_REVISION_DOMAIN, INVENTORY_REVISION_KIND,
    MAX_INVENTORY_DATABASES,
};

#[cfg(test)]
pub(super) struct LingmaRootAuthorizedSource {
    pub(super) source_root: ProviderSourceRoot,
    sqlite_authority: SqliteSourceDirectoryAuthority,
    database_name: OsString,
}

#[cfg(test)]
impl LingmaRootAuthorizedSource {
    pub(super) fn retain(data_root: &Path, path: &Path) -> LingmaSourceBackedResultV0<Self> {
        let parent = path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("Lingma SQLite source has no parent directory".to_owned())
        })?;
        let database_name = path.file_name().map(OsString::from).ok_or_else(|| {
            CaptureError::InvalidPayload("Lingma SQLite source has no leaf name".to_owned())
        })?;
        let source_root = ProviderSourceRoot::open(parent)?;
        let directory = source_root.directory()?;
        let authority_handle = directory
            .try_clone_authority_handle()
            .map_err(CaptureError::from)?;
        let sqlite_authority =
            retain_sqlite_source_directory_authority(data_root, &authority_handle, parent)?;
        source_root.revalidate()?;
        Ok(Self {
            source_root,
            sqlite_authority,
            database_name,
        })
    }

    pub(super) fn open_snapshot(&self) -> LingmaSourceBackedResultV0<SqliteSourceReadSnapshot> {
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&self.sqlite_authority, &self.database_name)?;
        let connection = snapshot.connection()?;
        let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
            .map_err(|_| LingmaSourceBackedErrorV0::CountOverflow)?;
        connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(CaptureError::from)?;
        self.source_root.revalidate()?;
        Ok(snapshot)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LingmaDatabaseSourceV0 {
    pub(super) path: PathBuf,
    catalog_lineage: TypedKey,
    source_scope: SourceAnchorScope,
}

impl LingmaDatabaseSourceV0 {
    #[cfg(test)]
    pub(crate) fn new(
        path: impl Into<PathBuf>,
        catalog_lineage: TypedKey,
    ) -> LingmaSourceBackedResultV0<Self> {
        Self::new_scoped(path, catalog_lineage, SourceAnchorScope::Unqualified)
    }

    pub(crate) fn new_scoped(
        path: impl Into<PathBuf>,
        catalog_lineage: TypedKey,
        source_scope: SourceAnchorScope,
    ) -> LingmaSourceBackedResultV0<Self> {
        let source = Self {
            path: path.into(),
            catalog_lineage,
            source_scope,
        };
        source.source_key()?;
        Ok(source)
    }

    pub(crate) fn source_key(&self) -> LingmaSourceBackedResultV0<SourceKey> {
        lingma_source_key_scoped(self.catalog_lineage.clone(), self.source_scope)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// A complete, finite inventory supplied by the installed-client/profile/version discovery lane.
///
/// The catalog lineage is deliberately caller-owned: physical database paths are acquisition
/// locations and never enter stable source, session, or event identity.
#[derive(Debug, Clone)]
pub(crate) struct LingmaSourceInventoryV0 {
    pub(super) databases: Vec<LingmaDatabaseSourceV0>,
    pub(super) observation: SourceInventoryObservation,
}

impl LingmaSourceInventoryV0 {
    pub(crate) fn new(
        authority_key: TypedKey,
        mut databases: Vec<LingmaDatabaseSourceV0>,
    ) -> LingmaSourceBackedResultV0<Self> {
        if databases.len() > MAX_INVENTORY_DATABASES {
            return Err(LingmaSourceBackedErrorV0::InventoryTooLarge);
        }
        databases.sort_by_key(|database| {
            database
                .source_key()
                .map(|source| source.identity().digest())
                .unwrap_or([0; 32])
        });
        let mut source_keys = Vec::with_capacity(databases.len());
        for database in &databases {
            source_keys.push(database.source_key()?);
        }
        if source_keys
            .windows(2)
            .any(|pair| pair[0].identity() == pair[1].identity())
        {
            return Err(LingmaSourceBackedErrorV0::DuplicateDatabaseLineage);
        }
        let revision = inventory_revision(&source_keys);
        let observation = SourceInventoryObservation::new(
            CaptureProvider::Lingma.as_str(),
            INVENTORY_AUTHORITY_NAMESPACE,
            authority_key.clone(),
            INVENTORY_REVISION_KIND,
            revision.to_vec(),
        )?;
        Ok(Self {
            databases,
            observation,
        })
    }

    #[cfg(test)]
    pub(super) fn source_keys(&self) -> LingmaSourceBackedResultV0<Vec<SourceKey>> {
        self.databases
            .iter()
            .map(LingmaDatabaseSourceV0::source_key)
            .collect()
    }

    pub(crate) fn observation(&self) -> &SourceInventoryObservation {
        &self.observation
    }

    pub(crate) fn databases(&self) -> &[LingmaDatabaseSourceV0] {
        &self.databases
    }

    #[cfg(test)]
    pub(super) fn exact_inventory_eq(&self, other: &Self) -> LingmaSourceBackedResultV0<bool> {
        if self.observation.authority_key() != other.observation.authority_key()
            || self.databases.len() != other.databases.len()
        {
            return Ok(false);
        }
        for (left, right) in self.databases.iter().zip(&other.databases) {
            let left_key = left.source_key()?;
            let right_key = right.source_key()?;
            if !left_key.exact_descriptor_eq(&right_key) || left.path != right.path {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn inventory_revision(sources: &[SourceKey]) -> [u8; 32] {
    let mut revision = Sha256::new();
    revision.update(INVENTORY_REVISION_DOMAIN);
    revision.update((sources.len() as u64).to_be_bytes());
    for source in sources {
        revision.update(source.identity().digest());
        revision.update(source.exact_descriptor_digest());
    }
    revision.finalize().into()
}
