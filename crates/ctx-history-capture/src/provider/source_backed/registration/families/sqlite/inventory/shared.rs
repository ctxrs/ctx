use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use rusqlite::{types::ValueRef, Connection, OptionalExtension};
use sha2::Digest;

use crate::{
    common::io::ProviderSourceRoot,
    provider::source_backed::family::document::{
        document_frontier_fingerprint,
        register_replacement_document_tree_route as register_document_tree_route,
        register_replacement_document_tree_route_with_authority as register_document_tree_route_with_authority,
        ChangedDocumentSink, CompleteDocumentTree, DocumentLeafExecutionPolicy,
        DocumentLeafFingerprint, DocumentSourceTerminal, ObservedDocumentLeaf,
        ReplacementDocumentTree,
    },
    provider_sources::{
        open_root_handle_sqlite_source_physical_revision, open_root_handle_sqlite_source_snapshot,
        retain_sqlite_source_directory_authority, SqlitePhysicalReplayHint,
        SqliteSourceDirectoryAuthority, SqliteSourcePhysicalRevision, SqliteSourceReadSnapshot,
    },
    MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::*;

const SQLITE_LOGICAL_LEAF_DOMAIN: &[u8] = b"ctx.sqlite-inventory-logical-leaf-v1\0";

/// Central admission policy for independently certifiable SQLite inventories.
///
/// These providers discover a bounded set of distinct databases, derive each
/// exact source from catalog evidence, and scan one retained snapshot per
/// leaf. Single-database and compound-database routes retain the serial
/// default.
pub(super) fn sqlite_inventory_leaf_execution_policy(
    provider: CaptureProvider,
) -> DocumentLeafExecutionPolicy {
    match provider {
        CaptureProvider::AstrBot | CaptureProvider::Lingma | CaptureProvider::Crush => {
            DocumentLeafExecutionPolicy::Independent
        }
        _ => DocumentLeafExecutionPolicy::Serial,
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SqliteInventorySnapshotCounters {
    pub(super) immutable_snapshot_opens: u64,
    pub(super) copied_snapshot_opens: u64,
    pub(super) source_bytes_copied: u64,
    pub(super) physical_revision_captures: u64,
    pub(super) physical_replay_hits: u64,
    pub(super) physical_database_bytes_read: u64,
    pub(super) physical_wal_bytes_read: u64,
    pub(super) logical_rows_scanned: u64,
    pub(super) terminal_fences: u64,
    pub(super) terminal_revalidations: u64,
    pub(super) active_snapshots: u64,
    pub(super) max_active_snapshots: u64,
}

pub(super) struct SqliteInventoryCatalog<L> {
    pub(super) authority_fingerprint: [u8; 32],
    pub(super) leaves: Vec<SqliteInventoryCatalogLeaf<L>>,
}

pub(super) struct SqliteInventoryCatalogLeaf<L> {
    pub(super) source: SourceKey,
    pub(super) path: PathBuf,
    pub(super) provider_leaf: L,
}

pub(super) trait SqliteInventoryProvider: Send + Sync + 'static {
    type Leaf: Send + Sync + 'static;

    fn parser_revision(&self) -> &'static str;

    /// Tables whose logical rows determine whether projection can be replayed.
    ///
    /// Missing optional tables are represented explicitly in the fingerprint;
    /// provider validation still owns whether a schema is admissible.
    fn logical_tables(&self) -> &'static [&'static str];

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>>;

    fn scan(
        &self,
        leaf: &Self::Leaf,
        snapshot: SqliteSourceReadSnapshot,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<CertifiedSource>;

    fn hydrate(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure>;

    #[cfg(test)]
    fn after_snapshots_sealed(&self) {}

    #[cfg(test)]
    fn observe_snapshot_counters(&self, _counters: SqliteInventorySnapshotCounters) {}

    #[cfg(test)]
    fn test_leaf_execution_policy(&self) -> Option<DocumentLeafExecutionPolicy> {
        None
    }
}

pub(super) struct SqliteInventoryDocumentAdapter<A> {
    data_root: PathBuf,
    provider: CaptureProvider,
    source_format: &'static str,
    provider_adapter: A,
}

impl<A> SqliteInventoryDocumentAdapter<A>
where
    A: SqliteInventoryProvider,
{
    fn new(
        data_root: &Path,
        provider: CaptureProvider,
        source_format: &'static str,
        provider_adapter: A,
    ) -> Self {
        Self {
            data_root: data_root.to_path_buf(),
            provider,
            source_format,
            provider_adapter,
        }
    }

    pub(super) fn register_replacement_document_tree_route(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
        data_root: &Path,
        provider: CaptureProvider,
        source_format: &'static str,
        provider_adapter: A,
    ) -> SourceBackedCoordinatorResult<()> {
        register_document_tree_route(
            registry,
            source,
            selection,
            Self::new(data_root, provider, source_format, provider_adapter),
        )
    }

    // The arguments are the complete declarative route contract; grouping them
    // would only introduce another provider-registration wrapper type.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn register_replacement_document_tree_route_with_authority(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
        authority: SourceBackedSelectorAuthority,
        data_root: &Path,
        provider: CaptureProvider,
        source_format: &'static str,
        provider_adapter: A,
    ) -> SourceBackedCoordinatorResult<()> {
        register_document_tree_route_with_authority(
            registry,
            source,
            selection,
            authority,
            Self::new(data_root, provider, source_format, provider_adapter),
        )
    }

    fn discover_with_base(
        &self,
        base_sources: &[CertifiedSource],
    ) -> SourceBackedRouteResult<
        CompleteDocumentTree<SqliteInventoryDocumentLeaf<A::Leaf>, SqliteInventoryTreeAuthority>,
    > {
        let catalog = self.provider_adapter.discover()?;
        let mut catalog_leaves = Vec::with_capacity(catalog.leaves.len());
        let mut fingerprints = Vec::with_capacity(catalog.leaves.len());
        let mut observed = Vec::with_capacity(catalog.leaves.len());
        for (index, leaf) in catalog.leaves.into_iter().enumerate() {
            let catalog_leaf = sqlite_catalog_leaf_fingerprint(&leaf.source, &leaf.path);
            let retained = RetainedSqliteInventoryLeaf::retain(&self.data_root, &leaf.path)?;
            let replay = exact_base_certificate(base_sources, &leaf.source, self.parser_revision())
                .zip(SqlitePhysicalReplayHint::load(
                    &self.data_root,
                    &leaf.source,
                ))
                .and_then(|(committed, hint)| {
                    let physical = retained.open_physical_revision().ok()?;
                    if !hint.matches(
                        &leaf.source,
                        self.parser_revision(),
                        physical.revision(),
                        committed,
                    ) {
                        return None;
                    }
                    let fingerprint = document_frontier_fingerprint(hint.certificate())?;
                    physical.mark_replay_hit().ok()?;
                    let physical_revision = *physical.revision();
                    let terminal_revalidate: Box<dyn Fn() -> bool + Send + Sync> =
                        Box::new(move || physical.revalidate().is_ok());
                    Some((
                        fingerprint.as_bytes(),
                        physical_revision,
                        terminal_revalidate,
                    ))
                });
            let (
                logical_fingerprint,
                physical_revision,
                snapshot,
                terminal_revalidate,
                replay_hint_current,
            ) = if let Some((fingerprint, physical_revision, terminal_revalidate)) = replay {
                (
                    fingerprint,
                    physical_revision,
                    None,
                    terminal_revalidate,
                    true,
                )
            } else {
                let snapshot = retained.open()?;
                let physical_revision = *snapshot.evidence().physical_revision();
                let logical_fingerprint = sqlite_logical_leaf_fingerprint(
                    catalog_leaf,
                    self.provider_adapter.logical_tables(),
                    snapshot.connection().map_err(route_error)?,
                    &retained.authority,
                )?;
                let revalidate = snapshot.terminal_revalidator();
                let terminal_revalidate: Box<dyn Fn() -> bool + Send + Sync> =
                    Box::new(move || revalidate().is_ok());
                (
                    logical_fingerprint,
                    physical_revision,
                    Some(snapshot),
                    terminal_revalidate,
                    false,
                )
            };
            catalog_leaves.push(catalog_leaf);
            fingerprints.push(logical_fingerprint);
            observed.push(ObservedDocumentLeaf::new(
                DocumentLeafFingerprint::new(logical_fingerprint),
                SqliteInventoryDocumentLeaf {
                    index,
                    source: leaf.source,
                    path: leaf.path,
                    logical_fingerprint,
                    physical_revision,
                    replay_hint_current,
                    provider_leaf: leaf.provider_leaf,
                    _retained: retained,
                    snapshot: Mutex::new(snapshot),
                    terminal_revalidate,
                },
            ));
        }
        let tree_fingerprint =
            sqlite_inventory_tree_fingerprint(catalog.authority_fingerprint, &fingerprints);
        Ok(CompleteDocumentTree::new(
            tree_fingerprint,
            observed,
            SqliteInventoryTreeAuthority {
                authority_fingerprint: catalog.authority_fingerprint,
                catalog_leaves,
            },
        ))
    }
}

pub(super) struct SqliteInventoryDocumentLeaf<L> {
    index: usize,
    source: SourceKey,
    path: PathBuf,
    logical_fingerprint: [u8; 32],
    physical_revision: [u8; 32],
    replay_hint_current: bool,
    provider_leaf: L,
    _retained: RetainedSqliteInventoryLeaf,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
    terminal_revalidate: Box<dyn Fn() -> bool + Send + Sync>,
}

#[derive(Debug)]
struct RetainedSqliteInventoryLeaf {
    authority: SqliteSourceDirectoryAuthority,
    database_name: OsString,
}

impl RetainedSqliteInventoryLeaf {
    fn retain(data_root: &Path, path: &Path) -> SourceBackedRouteResult<Self> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let database_name = path.file_name().map(OsString::from).ok_or_else(|| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::InvalidSource,
                format!("SQLite inventory source {path:?} has no database leaf"),
            )
        })?;
        let source_root = ProviderSourceRoot::open(parent).map_err(route_error)?;
        let directory = source_root.directory().map_err(route_error)?;
        let authority_handle = directory
            .try_clone_authority_handle()
            .map_err(route_error)?;
        let authority =
            retain_sqlite_source_directory_authority(data_root, &authority_handle, parent)
                .map_err(route_error)?;
        let retained = Self {
            authority,
            database_name,
        };
        directory.revalidate().map_err(route_error)?;
        source_root.revalidate().map_err(route_error)?;
        Ok(retained)
    }

    fn open(&self) -> SourceBackedRouteResult<SqliteSourceReadSnapshot> {
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&self.authority, &self.database_name)
                .map_err(route_error)?;
        let connection = snapshot.connection().map_err(route_error)?;
        let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(route_error)?;
        connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(route_error)?;
        Ok(snapshot)
    }

    fn open_physical_revision(&self) -> SourceBackedRouteResult<SqliteSourcePhysicalRevision> {
        open_root_handle_sqlite_source_physical_revision(&self.authority, &self.database_name)
            .map_err(route_error)
    }
}

#[derive(Debug)]
pub(super) struct SqliteInventoryTreeAuthority {
    authority_fingerprint: [u8; 32],
    catalog_leaves: Vec<[u8; 32]>,
}

impl<A> ReplacementDocumentTree for SqliteInventoryDocumentAdapter<A>
where
    A: SqliteInventoryProvider,
{
    type Leaf = SqliteInventoryDocumentLeaf<A::Leaf>;
    type TreeAuthority = SqliteInventoryTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        self.provider_adapter.parser_revision()
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == self.provider.as_str() && source.source_format() == self.source_format
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        #[cfg(test)]
        if let Some(policy) = self.provider_adapter.test_leaf_execution_policy() {
            return policy;
        }
        sqlite_inventory_leaf_execution_policy(self.provider)
    }

    fn independent_leaf_source(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<SourceKey> {
        validate_catalog_slot(authority, leaf)?;
        Ok(leaf.source.clone())
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        self.discover_with_base(&[])
    }

    fn discover_complete_with_base(
        &self,
        base_sources: &[CertifiedSource],
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        self.discover_with_base(base_sources)
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        validate_catalog_slot(authority, leaf)?;
        let snapshot = leaf
            .snapshot
            .lock()
            .map_err(|_| sqlite_inventory_internal("SQLite snapshot lock was poisoned"))?
            .take()
            .ok_or_else(|| sqlite_inventory_internal("SQLite snapshot was already consumed"))?;
        sink.begin_source(leaf.source.clone())?;
        let certificate = self
            .provider_adapter
            .scan(&leaf.provider_leaf, snapshot, sink)?;
        if !certificate
            .observation()
            .source()
            .exact_descriptor_eq(&leaf.source)
            || certificate.parser_revision() != self.parser_revision()
            || certificate.frontier().is_some()
        {
            return Err(sqlite_inventory_changed(
                "logical scan returned an unexpected source certificate",
            ));
        }
        let observation = certificate.observation().clone();
        Ok(DocumentSourceTerminal {
            source: observation.source().clone(),
            opening: observation.clone(),
            closing: observation,
            parser_revision: self.parser_revision(),
            content_digest: *certificate.content_digest(),
            counts: certificate.counts(),
        })
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let current = self.provider_adapter.discover()?;
        let current_catalog = current
            .leaves
            .iter()
            .map(|leaf| sqlite_catalog_leaf_fingerprint(&leaf.source, &leaf.path))
            .collect::<Vec<_>>();
        if current.authority_fingerprint != tree.authority.authority_fingerprint
            || current_catalog != tree.authority.catalog_leaves
        {
            return Err(sqlite_inventory_changed(
                "complete SQLite inventory changed during staging",
            ));
        }
        let mut fingerprints = Vec::with_capacity(tree.leaves.len());
        for observed in &tree.leaves {
            let leaf = &observed.provider_leaf;
            if let Some(snapshot) = leaf
                .snapshot
                .lock()
                .map_err(|_| sqlite_inventory_internal("SQLite snapshot lock was poisoned"))?
                .take()
            {
                snapshot.finish().map_err(route_error)?;
            }
            fingerprints.push(leaf.logical_fingerprint);
        }
        #[cfg(test)]
        self.provider_adapter.after_snapshots_sealed();
        for observed in &tree.leaves {
            if !(observed.provider_leaf.terminal_revalidate)() {
                return Err(sqlite_inventory_changed(
                    "retained SQLite terminal fence no longer matches its source family",
                ));
            }
            #[cfg(test)]
            {
                let counters = observed
                    .provider_leaf
                    ._retained
                    .authority
                    .snapshot_counters();
                self.provider_adapter
                    .observe_snapshot_counters(SqliteInventorySnapshotCounters {
                        immutable_snapshot_opens: counters.immutable_snapshot_opens(),
                        copied_snapshot_opens: counters.copied_snapshot_opens(),
                        source_bytes_copied: counters.source_bytes_copied(),
                        physical_revision_captures: counters.physical_revision_captures(),
                        physical_replay_hits: counters.physical_replay_hits(),
                        physical_database_bytes_read: counters.physical_database_bytes_read(),
                        physical_wal_bytes_read: counters.physical_wal_bytes_read(),
                        logical_rows_scanned: counters.logical_rows_scanned(),
                        terminal_fences: counters.terminal_fences(),
                        terminal_revalidations: counters.terminal_revalidations(),
                        active_snapshots: counters.active_snapshots(),
                        max_active_snapshots: counters.max_active_snapshots(),
                    });
            }
        }
        Ok(sqlite_inventory_tree_fingerprint(
            current.authority_fingerprint,
            &fingerprints,
        ))
    }

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        self.provider_adapter.hydrate(request)
    }

    fn after_successful_publication(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
        certificates: &std::collections::HashMap<[u8; 32], CertifiedSource>,
    ) {
        for observed in &tree.leaves {
            let leaf = &observed.provider_leaf;
            if leaf.replay_hint_current {
                continue;
            }
            let Some(certificate) = certificates.get(&leaf.source.identity().digest()) else {
                continue;
            };
            if !certificate
                .observation()
                .source()
                .exact_descriptor_eq(&leaf.source)
            {
                continue;
            }
            SqlitePhysicalReplayHint::publish_best_effort(
                &self.data_root,
                &leaf.source,
                self.parser_revision(),
                certificate,
                leaf.physical_revision,
            );
        }
    }

    fn has_successful_publication_work(&self) -> bool {
        true
    }
}

fn validate_catalog_slot<L>(
    authority: &SqliteInventoryTreeAuthority,
    leaf: &SqliteInventoryDocumentLeaf<L>,
) -> SourceBackedRouteResult<()> {
    if authority.catalog_leaves.get(leaf.index)
        != Some(&sqlite_catalog_leaf_fingerprint(&leaf.source, &leaf.path))
    {
        return Err(sqlite_inventory_changed(
            "observed leaf no longer matches its catalog slot",
        ));
    }
    Ok(())
}

fn sqlite_catalog_leaf_fingerprint(source: &SourceKey, path: &Path) -> [u8; 32] {
    let path = path.as_os_str().as_encoded_bytes();
    let mut digest = sha2::Sha256::new();
    digest.update(b"ctx.sqlite-inventory-catalog-leaf-v1\0");
    digest.update(source.exact_descriptor_digest());
    digest.update((path.len() as u64).to_be_bytes());
    digest.update(path);
    digest.finalize().into()
}

fn exact_base_certificate<'a>(
    base_sources: &'a [CertifiedSource],
    source: &SourceKey,
    parser_revision: &str,
) -> Option<&'a CertifiedSource> {
    let mut matching = base_sources.iter().filter(|candidate| {
        candidate.observation().source().exact_descriptor_eq(source)
            && candidate.parser_revision() == parser_revision
    });
    let certificate = matching.next()?;
    matching.next().is_none().then_some(certificate)
}

fn sqlite_logical_leaf_fingerprint(
    catalog_leaf: [u8; 32],
    tables: &[&str],
    connection: &Connection,
    authority: &SqliteSourceDirectoryAuthority,
) -> SourceBackedRouteResult<[u8; 32]> {
    let mut digest = sha2::Sha256::new();
    digest.update(SQLITE_LOGICAL_LEAF_DOMAIN);
    digest.update(catalog_leaf);
    let user_version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(sqlite_logical_observation_error)?;
    digest.update(user_version.to_be_bytes());
    let schema =
        crate::provider::sqlite::sqlite_schema_fingerprint(connection).map_err(route_error)?;
    hash_logical_bytes(&mut digest, schema.as_bytes());
    digest.update((tables.len() as u64).to_be_bytes());
    let mut logical_rows = 0_u64;
    for table in tables {
        logical_rows = logical_rows
            .checked_add(hash_sqlite_table(connection, &mut digest, table)?)
            .ok_or_else(|| sqlite_inventory_internal("SQLite logical row count overflowed"))?;
    }
    authority
        .record_logical_rows_scanned(logical_rows)
        .map_err(route_error)?;
    Ok(digest.finalize().into())
}

fn hash_sqlite_table(
    connection: &Connection,
    digest: &mut sha2::Sha256,
    table: &str,
) -> SourceBackedRouteResult<u64> {
    hash_logical_bytes(digest, table.as_bytes());
    let schema = connection
        .query_row(
            "select sql from sqlite_schema where type = 'table' and name = ?1",
            [table],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(sqlite_logical_observation_error)?
        .flatten();
    let Some(schema) = schema else {
        digest.update([0]);
        return Ok(0);
    };
    digest.update([1]);
    hash_logical_bytes(digest, schema.as_bytes());

    let quoted = format!("\"{}\"", table.replace('"', "\"\""));
    let mut statement = connection
        .prepare(&format!("select * from {quoted} order by rowid"))
        .map_err(sqlite_logical_observation_error)?;
    let columns = statement.column_count();
    digest.update((columns as u64).to_be_bytes());
    let mut rows = statement
        .query([])
        .map_err(sqlite_logical_observation_error)?;
    let mut row_count = 0_u64;
    while let Some(row) = rows.next().map_err(sqlite_logical_observation_error)? {
        row_count = row_count
            .checked_add(1)
            .ok_or_else(|| sqlite_inventory_internal("SQLite logical row count overflowed"))?;
        for column in 0..columns {
            hash_sqlite_value(
                digest,
                row.get_ref(column)
                    .map_err(sqlite_logical_observation_error)?,
            );
        }
    }
    digest.update(row_count.to_be_bytes());
    Ok(row_count)
}

fn hash_sqlite_value(digest: &mut sha2::Sha256, value: ValueRef<'_>) {
    match value {
        ValueRef::Null => digest.update([0]),
        ValueRef::Integer(value) => {
            digest.update([1]);
            digest.update(value.to_be_bytes());
        }
        ValueRef::Real(value) => {
            digest.update([2]);
            digest.update(value.to_bits().to_be_bytes());
        }
        ValueRef::Text(value) => {
            digest.update([3]);
            hash_logical_bytes(digest, value);
        }
        ValueRef::Blob(value) => {
            digest.update([4]);
            hash_logical_bytes(digest, value);
        }
    }
}

fn hash_logical_bytes(digest: &mut sha2::Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn sqlite_logical_observation_error(error: rusqlite::Error) -> SourceBackedRouteError {
    SourceBackedRouteError::new(
        SourceBackedRouteErrorKind::InvalidSource,
        format!("SQLite logical observation failed: {error}"),
    )
}

fn sqlite_inventory_tree_fingerprint(
    authority_fingerprint: [u8; 32],
    leaves: &[[u8; 32]],
) -> [u8; 32] {
    let mut digest = sha2::Sha256::new();
    digest.update(b"ctx.sqlite-inventory-document-tree-v1\0");
    digest.update(authority_fingerprint);
    digest.update((leaves.len() as u64).to_be_bytes());
    for leaf in leaves {
        digest.update(leaf);
    }
    digest.finalize().into()
}

pub(super) fn sqlite_inventory_authority_fingerprint(
    observation: &ctx_history_core::SourceInventoryObservation,
) -> SourceBackedRouteResult<[u8; 32]> {
    let authority_key = serde_json::to_vec(observation.authority_key()).map_err(route_error)?;
    let mut digest = sha2::Sha256::new();
    digest.update(b"ctx.sqlite-inventory-provider-authority-v1\0");
    digest.update(observation.provider().as_bytes());
    digest.update(observation.authority_namespace().as_bytes());
    digest.update(authority_key);
    digest.update(observation.revision_kind().as_bytes());
    digest.update(observation.revision());
    Ok(digest.finalize().into())
}

fn sqlite_inventory_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn sqlite_inventory_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}
