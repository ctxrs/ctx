use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use sha2::Digest;

use crate::{
    common::io::ProviderSourceRoot,
    provider::source_backed::family::document::{
        register_replacement_document_tree_route as register_document_tree_route,
        register_replacement_document_tree_route_with_authority as register_document_tree_route_with_authority,
        ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint, DocumentSourceTerminal,
        ObservedDocumentLeaf, ReplacementDocumentTree,
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceDirectoryAuthority, SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::*;

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

    fn hydrate(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure>;
}

pub(super) struct SqliteInventoryDocumentAdapter<A> {
    provider: CaptureProvider,
    source_format: &'static str,
    provider_adapter: A,
}

impl<A> SqliteInventoryDocumentAdapter<A>
where
    A: SqliteInventoryProvider,
{
    fn new(provider: CaptureProvider, source_format: &'static str, provider_adapter: A) -> Self {
        Self {
            provider,
            source_format,
            provider_adapter,
        }
    }

    pub(super) fn register_replacement_document_tree_route(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
        provider: CaptureProvider,
        source_format: &'static str,
        provider_adapter: A,
    ) -> SourceBackedCoordinatorResult<()> {
        register_document_tree_route(
            registry,
            source,
            selection,
            Self::new(provider, source_format, provider_adapter),
        )
    }

    pub(super) fn register_replacement_document_tree_route_with_authority(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
        authority: SourceBackedSelectorAuthority,
        provider: CaptureProvider,
        source_format: &'static str,
        provider_adapter: A,
    ) -> SourceBackedCoordinatorResult<()> {
        register_document_tree_route_with_authority(
            registry,
            source,
            selection,
            authority,
            Self::new(provider, source_format, provider_adapter),
        )
    }
}

pub(super) struct SqliteInventoryDocumentLeaf<L> {
    index: usize,
    source: SourceKey,
    path: PathBuf,
    evidence: SqliteSourceEvidence,
    provider_leaf: L,
}

#[derive(Debug)]
struct RetainedSqliteInventoryLeaf {
    authority: SqliteSourceDirectoryAuthority,
    database_name: OsString,
}

impl RetainedSqliteInventoryLeaf {
    fn observe(path: &Path) -> SourceBackedRouteResult<(Self, SqliteSourceEvidence)> {
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
        let authority = retain_sqlite_source_directory_authority(&authority_handle, parent)
            .map_err(route_error)?;
        let retained = Self {
            authority,
            database_name,
        };
        let evidence = retained.open()?.finish().map_err(route_error)?;
        directory.revalidate().map_err(route_error)?;
        source_root.revalidate().map_err(route_error)?;
        Ok((retained, evidence))
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
}

#[derive(Debug)]
pub(super) struct SqliteInventoryTreeAuthority {
    authority_fingerprint: [u8; 32],
    catalog_leaves: Vec<[u8; 32]>,
    leaves: Vec<RetainedSqliteInventoryLeaf>,
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

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let catalog = self.provider_adapter.discover()?;
        let mut retained = Vec::with_capacity(catalog.leaves.len());
        let mut catalog_leaves = Vec::with_capacity(catalog.leaves.len());
        let mut fingerprints = Vec::with_capacity(catalog.leaves.len());
        let mut observed = Vec::with_capacity(catalog.leaves.len());
        for (index, leaf) in catalog.leaves.into_iter().enumerate() {
            let catalog_leaf = sqlite_catalog_leaf_fingerprint(&leaf.source, &leaf.path);
            let (authority, evidence) = RetainedSqliteInventoryLeaf::observe(&leaf.path)?;
            let fingerprint = sqlite_physical_leaf_fingerprint(catalog_leaf, &evidence);
            retained.push(authority);
            catalog_leaves.push(catalog_leaf);
            fingerprints.push(fingerprint);
            observed.push(ObservedDocumentLeaf::with_durable_replay(
                DocumentLeafFingerprint::new(fingerprint),
                SqliteInventoryDocumentLeaf {
                    index,
                    source: leaf.source,
                    path: leaf.path,
                    evidence,
                    provider_leaf: leaf.provider_leaf,
                },
                false,
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
                leaves: retained,
            },
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let retained = authority.leaves.get(leaf.index).ok_or_else(|| {
            sqlite_inventory_changed("observed leaf has no retained directory authority")
        })?;
        if authority.catalog_leaves.get(leaf.index)
            != Some(&sqlite_catalog_leaf_fingerprint(&leaf.source, &leaf.path))
        {
            return Err(sqlite_inventory_changed(
                "observed leaf no longer matches its catalog slot",
            ));
        }
        let snapshot = retained.open()?;
        if snapshot.evidence() != &leaf.evidence {
            return Err(sqlite_inventory_changed(
                "SQLite source changed after complete physical discovery",
            ));
        }
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
            || tree.leaves.len() != tree.authority.leaves.len()
        {
            return Err(sqlite_inventory_changed(
                "complete SQLite inventory changed during staging",
            ));
        }
        let mut fingerprints = Vec::with_capacity(tree.leaves.len());
        for (observed, retained) in tree.leaves.iter().zip(&tree.authority.leaves) {
            let snapshot = retained.open()?;
            let evidence = snapshot.finish().map_err(route_error)?;
            fingerprints.push(sqlite_physical_leaf_fingerprint(
                sqlite_catalog_leaf_fingerprint(
                    &observed.provider_leaf.source,
                    &observed.provider_leaf.path,
                ),
                &evidence,
            ));
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

fn sqlite_physical_leaf_fingerprint(
    catalog_leaf: [u8; 32],
    evidence: &SqliteSourceEvidence,
) -> [u8; 32] {
    let mut digest = sha2::Sha256::new();
    digest.update(b"ctx.sqlite-inventory-physical-leaf-v1\0");
    digest.update(catalog_leaf);
    digest.update(evidence.identity());
    digest.update(evidence.length().to_be_bytes());
    digest.update(evidence.revision());
    digest.finalize().into()
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
