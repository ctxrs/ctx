use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use sha2::Digest;

#[cfg(test)]
use crate::provider_sources::SqliteSourceSnapshotCounters;
use crate::{
    common::io::ProviderSourceRoot,
    provider::source_backed::family::document::{
        register_replacement_document_tree_route as register_document_tree_route,
        register_replacement_document_tree_route_with_authority as register_document_tree_route_with_authority,
        ChangedDocumentSink, CompleteDocumentTree, DocumentLeafExecutionPolicy,
        DocumentLeafFingerprint, DocumentSourceTerminal, ObservedDocumentLeaf,
        ReplacementDocumentTree,
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot,
    },
    MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::*;

/// SQLite leaves can be individually large and already stream through one
/// bounded logical snapshot. Keep them serial so a changed database writes
/// directly to Tantivy instead of duplicating its complete Core projection in
/// per-leaf scratch. Provider routes themselves remain independently
/// schedulable by the refresh coordinator.
pub(super) fn sqlite_inventory_leaf_execution_policy(
    _provider: CaptureProvider,
) -> DocumentLeafExecutionPolicy {
    DocumentLeafExecutionPolicy::Serial
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SqliteInventorySnapshotCounters {
    pub(super) immutable_snapshot_opens: u64,
    pub(super) copied_snapshot_opens: u64,
    pub(super) source_bytes_copied: u64,
    pub(super) logical_projection_passes: u64,
    pub(super) logical_rows_projected: u64,
    pub(super) documents_staged: u64,
    pub(super) logical_noops: u64,
    pub(super) logical_replacements: u64,
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

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>>;

    fn scan(
        &self,
        leaf: &Self::Leaf,
        snapshot: SqliteSourceReadSnapshot,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<CertifiedSource>;

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
        _base_sources: &[CertifiedSource],
    ) -> SourceBackedRouteResult<
        CompleteDocumentTree<SqliteInventoryDocumentLeaf<A::Leaf>, SqliteInventoryTreeAuthority>,
    > {
        let catalog = self.provider_adapter.discover()?;
        let mut catalog_leaves = Vec::with_capacity(catalog.leaves.len());
        let mut fingerprints = Vec::with_capacity(catalog.leaves.len());
        let mut observed = Vec::with_capacity(catalog.leaves.len());
        for (index, leaf) in catalog.leaves.into_iter().enumerate() {
            let catalog_leaf = sqlite_catalog_leaf_fingerprint(&leaf.source, &leaf.path);
            // Discovery validates one path at a time but retains no provider
            // descriptors. Active scan workers reacquire the same no-follow
            // authority, bounding descriptors by worker count.
            let retained = RetainedSqliteInventoryLeaf::retain(&self.data_root, &leaf.path)?;
            let replay_fingerprint = retained.replay_fingerprint(catalog_leaf)?;
            #[cfg(test)]
            let counter_authority = retained.authority.clone();
            drop(retained);
            #[cfg(test)]
            let base_certificate =
                exact_base_certificate(_base_sources, &leaf.source, self.parser_revision())
                    .cloned();
            catalog_leaves.push(catalog_leaf);
            fingerprints.push(catalog_leaf);
            observed.push(ObservedDocumentLeaf::with_durable_replay(
                DocumentLeafFingerprint::new(replay_fingerprint),
                SqliteInventoryDocumentLeaf {
                    index,
                    source: leaf.source,
                    path: leaf.path,
                    catalog_fingerprint: catalog_leaf,
                    replay_fingerprint,
                    #[cfg(test)]
                    base_certificate,
                    provider_leaf: leaf.provider_leaf,
                    terminal_revalidate: Mutex::new(None),
                    #[cfg(test)]
                    snapshot_counters: Mutex::new(Some(Box::new(move || {
                        counter_authority.snapshot_counters()
                    }))),
                },
                true,
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

type TerminalRevalidator = Box<dyn Fn() -> SourceBackedRouteResult<()> + Send + Sync>;

pub(super) struct SqliteInventoryDocumentLeaf<L> {
    index: usize,
    source: SourceKey,
    path: PathBuf,
    catalog_fingerprint: [u8; 32],
    replay_fingerprint: [u8; 32],
    #[cfg(test)]
    base_certificate: Option<CertifiedSource>,
    provider_leaf: L,
    terminal_revalidate: Mutex<Option<TerminalRevalidator>>,
    #[cfg(test)]
    snapshot_counters: Mutex<Option<Box<dyn Fn() -> SqliteSourceSnapshotCounters + Send + Sync>>>,
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
        let source_root =
            ProviderSourceRoot::open(parent).map_err(sqlite_inventory_capture_error)?;
        let directory = source_root
            .directory()
            .map_err(sqlite_inventory_capture_error)?;
        let authority_handle = directory
            .try_clone_authority_handle()
            .map_err(|error| sqlite_inventory_capture_error(error.into()))?;
        let authority =
            retain_sqlite_source_directory_authority(data_root, &authority_handle, parent)
                .map_err(sqlite_source_route_error)?;
        // The SQLite authority certifies the parent object's identity and opens
        // only the named DB family. Directory metadata also reflects unrelated
        // sibling churn and is not part of this leaf's source authority.
        Ok(Self {
            authority,
            database_name,
        })
    }

    fn open(&self) -> SourceBackedRouteResult<SqliteSourceReadSnapshot> {
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&self.authority, &self.database_name)
                .map_err(sqlite_source_route_error)?;
        let configure = (|| {
            let connection = snapshot.connection().map_err(sqlite_source_route_error)?;
            let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
                sqlite_inventory_internal("SQLite provider value limit does not fit i32")
            })?;
            connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(|source| {
                    sqlite_source_route_error(snapshot.diagnose_provider_query_error(
                        "setting the private SQLite provider busy timeout",
                        source,
                        crate::provider_sources::SqliteFailurePhase::SourceValidation,
                    ))
                })?;
            Ok(())
        })();
        match configure {
            Ok(()) => Ok(snapshot),
            Err(primary) => Err(abort_sqlite_inventory_snapshot(snapshot, primary)),
        }
    }

    fn replay_fingerprint(
        &self,
        catalog_fingerprint: [u8; 32],
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let revision = self
            .authority
            .observe_physical_revision(&self.database_name)
            .map_err(sqlite_source_route_error)?;
        Ok(sqlite_inventory_replay_fingerprint(
            catalog_fingerprint,
            revision,
        ))
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
        let retained = RetainedSqliteInventoryLeaf::retain(&self.data_root, &leaf.path)?;
        let snapshot = retained.open()?;
        let revalidate = snapshot.terminal_revalidator();
        #[cfg(test)]
        let counter_authority = retained.authority.clone();
        if let Err(error) = sink.begin_source(leaf.source.clone()) {
            return Err(abort_sqlite_inventory_snapshot(snapshot, error));
        }
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
        #[cfg(test)]
        retained
            .authority
            .record_logical_projection(
                certificate.counts().complete_records,
                certificate.counts().indexed_documents,
                leaf.base_certificate
                    .as_ref()
                    .is_some_and(|base| sqlite_logical_certificate_eq(base, &certificate)),
            )
            .map_err(route_error)?;
        {
            let mut terminal = leaf.terminal_revalidate.lock().map_err(|_| {
                sqlite_inventory_internal("SQLite terminal witness lock was poisoned")
            })?;
            if terminal.is_some() {
                return Err(sqlite_inventory_internal(
                    "SQLite inventory leaf was scanned more than once",
                ));
            }
            *terminal = Some(Box::new(move || {
                revalidate().map_err(sqlite_source_route_error)
            }));
        }
        #[cfg(test)]
        {
            let mut counters = leaf.snapshot_counters.lock().map_err(|_| {
                sqlite_inventory_internal("SQLite snapshot counter lock was poisoned")
            })?;
            *counters = Some(Box::new(move || counter_authority.snapshot_counters()));
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
            fingerprints.push(observed.provider_leaf.catalog_fingerprint);
        }
        #[cfg(test)]
        self.provider_adapter.after_snapshots_sealed();
        for observed in &tree.leaves {
            let leaf = &observed.provider_leaf;
            let terminal = leaf.terminal_revalidate.lock().map_err(|_| {
                sqlite_inventory_internal("SQLite terminal witness lock was poisoned")
            })?;
            if let Some(revalidate) = terminal.as_ref() {
                revalidate()?;
            } else {
                let retained = RetainedSqliteInventoryLeaf::retain(&self.data_root, &leaf.path)?;
                if retained.replay_fingerprint(leaf.catalog_fingerprint)? != leaf.replay_fingerprint
                {
                    return Err(sqlite_inventory_changed(
                        "SQLite replay revision changed during staging",
                    ));
                }
            }
            drop(terminal);
            #[cfg(test)]
            {
                let counters = leaf
                    .snapshot_counters
                    .lock()
                    .map_err(|_| {
                        sqlite_inventory_internal("SQLite snapshot counter lock was poisoned")
                    })?
                    .as_ref()
                    .ok_or_else(|| {
                        sqlite_inventory_internal("SQLite snapshot counters were not installed")
                    })?();
                self.provider_adapter
                    .observe_snapshot_counters(SqliteInventorySnapshotCounters {
                        immutable_snapshot_opens: counters.immutable_snapshot_opens(),
                        copied_snapshot_opens: counters.copied_snapshot_opens(),
                        source_bytes_copied: counters.source_bytes_copied(),
                        logical_projection_passes: counters.logical_projection_passes(),
                        logical_rows_projected: counters.logical_rows_projected(),
                        documents_staged: counters.documents_staged(),
                        logical_noops: counters.logical_noops(),
                        logical_replacements: counters.logical_replacements(),
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
}

fn sqlite_inventory_capture_error(error: CaptureError) -> SourceBackedRouteError {
    if let Some(kind) = sqlite_capture_route_error(&error) {
        SourceBackedRouteError::new(kind, error.to_string())
    } else {
        route_error(error)
    }
}

fn abort_sqlite_inventory_snapshot(
    snapshot: SqliteSourceReadSnapshot,
    primary: SourceBackedRouteError,
) -> SourceBackedRouteError {
    match snapshot.abort() {
        Ok(()) => primary,
        Err(cleanup) => {
            combine_primary_and_cleanup_route_errors(primary, sqlite_source_route_error(cleanup))
        }
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

fn sqlite_inventory_replay_fingerprint(
    catalog_fingerprint: [u8; 32],
    physical_revision: [u8; 32],
) -> [u8; 32] {
    let mut digest = sha2::Sha256::new();
    digest.update(b"ctx.sqlite-inventory-durable-replay-v1\0");
    digest.update(catalog_fingerprint);
    digest.update(physical_revision);
    digest.finalize().into()
}

#[cfg(test)]
fn sqlite_logical_certificate_eq(left: &CertifiedSource, right: &CertifiedSource) -> bool {
    left.observation() == right.observation()
        && left.parser_revision() == right.parser_revision()
        && left.content_digest() == right.content_digest()
        && left.counts() == right.counts()
}

#[cfg(test)]
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
