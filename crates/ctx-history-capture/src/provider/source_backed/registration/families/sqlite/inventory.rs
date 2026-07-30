use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use sha2::Digest;

use crate::{
    common::io::ProviderSourceRoot,
    provider::source_backed::family::document::{
        register_replacement_document_tree_route,
        register_replacement_document_tree_route_with_authority, ChangedDocumentSink,
        CompleteDocumentTree, DocumentLeafFingerprint, DocumentSourceTerminal,
        ObservedDocumentLeaf, ReplacementDocumentTree,
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceDirectoryAuthority, SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::*;

struct SqliteInventoryCatalog<L> {
    authority_fingerprint: [u8; 32],
    leaves: Vec<SqliteInventoryCatalogLeaf<L>>,
}

struct SqliteInventoryCatalogLeaf<L> {
    source: SourceKey,
    path: PathBuf,
    provider_leaf: L,
}

trait SqliteInventoryProvider: Send + Sync + 'static {
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

struct SqliteInventoryDocumentAdapter<A> {
    provider: CaptureProvider,
    source_format: &'static str,
    provider_adapter: A,
}

impl<A> SqliteInventoryDocumentAdapter<A> {
    fn new(provider: CaptureProvider, source_format: &'static str, provider_adapter: A) -> Self {
        Self {
            provider,
            source_format,
            provider_adapter,
        }
    }
}

struct SqliteInventoryDocumentLeaf<L> {
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
struct SqliteInventoryTreeAuthority {
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

fn sqlite_inventory_authority_fingerprint(
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

/// Registers Crush's selector-owned finite project inventory. The coordinator
/// consumes the adapter's existing scan helpers but remains the only owner of
/// `GenerationWriter` and commit.
pub fn register_crush_source_backed_route<I>(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    inventory_source: Arc<I>,
) -> SourceBackedCoordinatorResult<()>
where
    I: CrushProjectInventorySourceV0 + Send + Sync + 'static,
{
    let scan_inventory = Arc::clone(&inventory_source);
    let revalidation_inventory = Arc::clone(&inventory_source);
    let complete_inventory_revalidation = Arc::clone(&inventory_source);
    let hydration_inventory = inventory_source;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let opening = bind_crush_inventory(scan_inventory.observe().map_err(route_error)?)
                .map_err(route_error)?;
            let base_sources = sink
                .writer
                .base_manifest()
                .map(|manifest| {
                    manifest
                        .sources
                        .iter()
                        .cloned()
                        .map(|certificate| {
                            (certificate.observation().source().clone(), certificate)
                        })
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();

            for database in &opening.databases {
                let opened = open_crush_source(database.clone()).map_err(route_error)?;
                let base = base_sources.get(&database.source_key);
                if base.is_some_and(|base| crush_exact_replay_matches(base, &opened)) {
                    if !finish_crush_source(opened).map_err(route_error)? {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Crush source changed while its replay was staged",
                        ));
                    }
                    let base = base.ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            "Crush replay base disappeared",
                        )
                    })?;
                    let writer_base = sink
                        .begin_source_append(database.source_key.clone())
                        .map_err(route_coordinator_error)?;
                    if writer_base != base {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Crush replay base changed inside the shared writer",
                        ));
                    }
                    let frontier = base.frontier().ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::InvalidSource,
                            "Crush replay base has no exact frontier",
                        )
                    })?;
                    sink.certify_source_append(
                        CertifiedSourceAppend::certify(
                            base,
                            base.clone(),
                            frontier.certified_prefix_bytes(),
                            *frontier.certified_prefix_digest(),
                        )
                        .map_err(route_error)?,
                    )
                    .map_err(route_coordinator_error)?;
                } else {
                    sink.begin_source(database.source_key.clone())
                        .map_err(route_coordinator_error)?;
                    let scan = scan_crush_source(&opened, sink.writer).map_err(route_error)?;
                    let closing = closing_crush_observation(&opened).map_err(route_error)?;
                    let opening_observation = opened.observation.clone();
                    if !finish_crush_source(opened).map_err(route_error)? {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Crush source changed while its replacement was staged",
                        ));
                    }
                    let frontier = SourceFrontier::new(
                        CRUSH_FRONTIER_KIND,
                        TypedKey::bytes(opening_observation.revision().to_vec())
                            .map_err(route_error)?,
                        scan.counts.certified_bytes,
                        scan.content_digest,
                    )
                    .map_err(route_error)?;
                    let certificate = CertifiedSource::certify_with_frontier(
                        opening_observation,
                        closing,
                        CRUSH_PARSER_REVISION,
                        scan.content_digest,
                        scan.counts,
                        Some(frontier),
                    )
                    .map_err(route_error)?;
                    sink.certify_source(certificate)
                        .map_err(route_coordinator_error)?;
                }
            }

            let closing_observation = scan_inventory.observe().map_err(route_error)?;
            if !opening
                .matches(closing_observation.clone())
                .map_err(route_error)?
            {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Crush project inventory changed during shared staging",
                ));
            }
            let closing = bind_crush_inventory(closing_observation).map_err(route_error)?;
            let certified_inventory = CertifiedSourceInventory::certify(
                opening.observation.clone(),
                closing.observation,
                CRUSH_DISCOVERY_REVISION,
                opening.source_keys(),
            )
            .map_err(route_error)?;
            sink.certify_complete_inventory(certified_inventory.clone())
                .map_err(route_coordinator_error)?;
            for base in base_sources.values() {
                let base_source = base.observation().source();
                if base_source.provider() == CaptureProvider::Crush.as_str()
                    && base_source.source_format() == "crush_sqlite"
                    && base_source.schema_variant() == CRUSH_SOURCE_SCHEMA_VARIANT
                    && !opening.contains_exact_source(base_source)
                {
                    sink.delete_source(
                        CertifiedSourceDeletion::from_inventory(
                            base_source.clone(),
                            &certified_inventory,
                        )
                        .map_err(route_error)?,
                        certified_inventory.clone(),
                    )
                    .map_err(route_coordinator_error)?;
                }
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Crush, "crush_sqlite"),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                let Ok(observation) = revalidation_inventory.observe() else {
                    return false;
                };
                let Ok(inventory) = bind_crush_inventory(observation) else {
                    return false;
                };
                let Some(database) = inventory.databases.iter().find(|database| {
                    database
                        .source_key
                        .exact_descriptor_eq(expected.observation().source())
                }) else {
                    return false;
                };
                let Ok(opened) = open_crush_source(database.clone()) else {
                    return false;
                };
                let observation_matches = opened.observation == *expected.observation();
                observation_matches && finish_crush_source(opened).unwrap_or(false)
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                let Ok(opening_observation) = revalidation_inventory.observe() else {
                    return false;
                };
                let Ok(opening) = bind_crush_inventory(opening_observation.clone()) else {
                    return false;
                };
                let Ok(closing_observation) = revalidation_inventory.observe() else {
                    return false;
                };
                if !opening
                    .matches(closing_observation.clone())
                    .unwrap_or(false)
                {
                    return false;
                }
                let Ok(closing) = bind_crush_inventory(closing_observation) else {
                    return false;
                };
                let source_keys = opening.source_keys();
                CertifiedSourceInventory::certify(
                    opening.observation,
                    closing.observation,
                    CRUSH_DISCOVERY_REVISION,
                    source_keys,
                )
                .is_ok_and(|inventory| deletion.verifies(&inventory))
            }
        },
        move |request| {
            let hydrated = CrushLocatorResolverV0::discover(hydration_inventory.as_ref())
                .and_then(|resolver| resolver.hydrate(request.locator()))
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            let provider_bytes = hydrated
                .decoded_display_text
                .ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::UnsupportedParserRevision,
                        "Crush record has no exact display text",
                    )
                })?
                .into_bytes();
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes,
            })
        },
    )
    .with_complete_inventory_revalidation(move |expected| {
        let Ok(opening_observation) = complete_inventory_revalidation.observe() else {
            return false;
        };
        let Ok(opening) = bind_crush_inventory(opening_observation.clone()) else {
            return false;
        };
        let Ok(closing_observation) = complete_inventory_revalidation.observe() else {
            return false;
        };
        if !opening
            .matches(closing_observation.clone())
            .unwrap_or(false)
        {
            return false;
        }
        let Ok(closing) = bind_crush_inventory(closing_observation) else {
            return false;
        };
        let source_keys = opening.source_keys();
        CertifiedSourceInventory::certify(
            opening.observation,
            closing.observation,
            CRUSH_DISCOVERY_REVISION,
            source_keys,
        )
        .is_ok_and(|current| current == *expected)
    });
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit,
        driver,
    )?);
    Ok(())
}

/// Registers AstrBot's complete selected/launcher inventory from the same
/// bounded discovery context used by provider selection.
pub fn register_astrbot_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    discovery: DiscoveryContext,
) -> SourceBackedCoordinatorResult<()> {
    register_replacement_document_tree_route(
        registry,
        source,
        selection,
        SqliteInventoryDocumentAdapter::new(
            CaptureProvider::AstrBot,
            "astrbot_data_v4_sqlite",
            AstrBotInventoryProvider { discovery },
        ),
    )
}

struct AstrBotInventoryProvider {
    discovery: DiscoveryContext,
}

impl SqliteInventoryProvider for AstrBotInventoryProvider {
    type Leaf = AstrBotSourceBackedSourceV0;

    fn parser_revision(&self) -> &'static str {
        ASTRBOT_SOURCE_BACKED_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let inventory = AstrBotSourceBackedInventoryV0::discover(&self.discovery)
            .map_err(astrbot_inventory_route_error)?;
        let authority_fingerprint =
            sqlite_inventory_authority_fingerprint(inventory.observation())?;
        let leaves = inventory
            .sources()
            .iter()
            .cloned()
            .map(|leaf| SqliteInventoryCatalogLeaf {
                source: leaf.source_key().clone(),
                path: leaf.path().to_path_buf(),
                provider_leaf: leaf,
            })
            .collect();
        Ok(SqliteInventoryCatalog {
            authority_fingerprint,
            leaves,
        })
    }

    fn scan(
        &self,
        leaf: &Self::Leaf,
        snapshot: SqliteSourceReadSnapshot,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let mut sink_failure = None;
        let certificate = scan_astrbot_snapshot_v0(leaf, snapshot, &mut |document| {
            if let Err(error) = sink.emit_document(document) {
                let detail = error.to_string();
                sink_failure = Some(error);
                return Err(
                    crate::provider::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0::Capture(
                        CaptureError::InvalidPayload(detail),
                    ),
                );
            }
            Ok(())
        });
        if let Some(error) = sink_failure {
            return Err(error);
        }
        certificate.map_err(route_error)
    }

    fn hydrate(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let inventory =
            AstrBotSourceBackedInventoryV0::discover(&self.discovery).map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
        AstrBotSourceBackedResolverV0::from_inventory(&inventory)
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?
            .hydrate_batch_request(request)
    }
}

fn astrbot_inventory_route_error(
    error: crate::provider::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0,
) -> SourceBackedRouteError {
    let kind = if matches!(
        &error,
        crate::provider::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0::IncompleteInventory { .. }
    ) {
        SourceBackedRouteErrorKind::Unavailable
    } else {
        SourceBackedRouteErrorKind::InvalidSource
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

/// Registers Shelley only when the caller retains the exact CWD that selected
/// `shelley.db`. No branch or fallback CWD is inferred.
pub fn register_shelley_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    exact_cwd: impl Into<std::path::PathBuf>,
) -> SourceBackedCoordinatorResult<()> {
    let exact_cwd = exact_cwd.into();
    let adapter = discover_shelley_source_backed_exact_cwd(&exact_cwd)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?
        .ok_or_else(|| {
            invalid_route(
                source.provider,
                "the exact Shelley CWD no longer contains an admitted database",
            )
        })?;
    if adapter.database_path() != source.path {
        return Err(invalid_route(
            source.provider,
            "the Shelley source path does not belong to the supplied exact CWD",
        ));
    }
    register_replacement_document_tree_route_with_authority(
        registry,
        source,
        SourceBackedRouteSelection::Automatic,
        SourceBackedSelectorAuthority::ExactCwd,
        SqliteInventoryDocumentAdapter::new(
            CaptureProvider::Shelley,
            "shelley_sqlite",
            ShelleyInventoryProvider { exact_cwd },
        ),
    )
}

struct ShelleyInventoryProvider {
    exact_cwd: PathBuf,
}

impl SqliteInventoryProvider for ShelleyInventoryProvider {
    type Leaf = ShelleySourceBackedAdapter;

    fn parser_revision(&self) -> &'static str {
        SHELLEY_SOURCE_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let adapter = discover_shelley_source_backed_exact_cwd(&self.exact_cwd)
            .map_err(shelley_inventory_route_error)?;
        let mut authority = sha2::Sha256::new();
        authority.update(b"ctx.shelley-exact-cwd-inventory-v1\0");
        authority.update(self.exact_cwd.as_os_str().as_encoded_bytes());
        let leaves = adapter
            .into_iter()
            .map(|leaf| SqliteInventoryCatalogLeaf {
                source: leaf.source().clone(),
                path: leaf.database_path().to_path_buf(),
                provider_leaf: leaf,
            })
            .collect();
        Ok(SqliteInventoryCatalog {
            authority_fingerprint: authority.finalize().into(),
            leaves,
        })
    }

    fn scan(
        &self,
        leaf: &Self::Leaf,
        snapshot: SqliteSourceReadSnapshot,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let mut scan = leaf.start_snapshot_scan(snapshot).map_err(route_error)?;
        while let Some(page) = scan.next_page().map_err(route_error)? {
            for document in page.documents {
                sink.emit_document(document)?;
            }
        }
        Ok(scan.finish().map_err(route_error)?.certificate)
    }

    fn hydrate(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let adapter = discover_shelley_source_backed_exact_cwd(&self.exact_cwd)
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "Shelley database is absent from the exact CWD",
                )
            })?;
        let records = request
            .events()
            .iter()
            .map(|event| {
                adapter
                    .hydrate(event.locator())
                    .map(|hydrated| HydratedProviderRecord {
                        event_id: event.event_id(),
                        provider_bytes: hydrated.text.into_bytes(),
                    })
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        BatchHydrationResult::new(records).map_err(|error| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                format!("invalid Shelley batch hydration result: {error}"),
            )
        })
    }
}

fn shelley_inventory_route_error(
    error: crate::provider::providers::shelley::native_path::source_backed::ShelleySourceBackedError,
) -> SourceBackedRouteError {
    let kind = match &error {
        crate::provider::providers::shelley::native_path::source_backed::ShelleySourceBackedError::Capture(
            CaptureError::Io(source),
        ) if source.kind() == std::io::ErrorKind::NotFound => {
            SourceBackedRouteErrorKind::Unavailable
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

/// Registers an inactive Hermes database only with a caller-owned persistent
/// anchor. Automatic profile routes continue to use provider-native profile
/// identity.
pub fn register_hermes_explicit_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    anchor: SourceAnchor,
) -> SourceBackedCoordinatorResult<()> {
    let candidate = hermes_source_backed_explicit(source.path.clone(), anchor)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    register_hermes_candidate(
        registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        candidate,
        SourceBackedSelectorAuthority::ExplicitPath,
    )
}

pub fn register_lingma_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    authority_key: TypedKey,
    databases: Vec<(std::path::PathBuf, TypedKey)>,
) -> SourceBackedCoordinatorResult<()> {
    let databases = databases
        .into_iter()
        .map(|(path, lineage)| LingmaDatabaseSourceV0::new(path, lineage))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let inventory = LingmaSourceInventoryV0::new(authority_key, databases)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    register_lingma_inventory_source(
        registry,
        source,
        selection,
        Arc::new(FixedLingmaInventorySource { inventory }),
    )
}

pub(in crate::provider::source_backed) trait LingmaInventorySource:
    Send + Sync
{
    fn observe(&self) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0>;
}

#[derive(Debug, Clone)]
struct FixedLingmaInventorySource {
    inventory: LingmaSourceInventoryV0,
}

impl LingmaInventorySource for FixedLingmaInventorySource {
    fn observe(&self) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0> {
        Ok(self.inventory.clone())
    }
}

#[derive(Debug, Clone)]
struct DiscoveredLingmaInventorySource {
    selector: LingmaInventorySelector,
}

impl LingmaInventorySource for DiscoveredLingmaInventorySource {
    fn observe(&self) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0> {
        self.selector
            .observe()
            .map_err(lingma_discovery_adapter_error)
            .and_then(lingma_adapter_inventory)
    }
}

pub(in crate::provider::source_backed) fn register_lingma_inventory_source(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    inventory_source: Arc<dyn LingmaInventorySource>,
) -> SourceBackedCoordinatorResult<()> {
    register_replacement_document_tree_route(
        registry,
        source,
        selection,
        SqliteInventoryDocumentAdapter::new(
            CaptureProvider::Lingma,
            "lingma_sqlite",
            LingmaInventoryProvider { inventory_source },
        ),
    )
}

struct LingmaInventoryProvider {
    inventory_source: Arc<dyn LingmaInventorySource>,
}

impl SqliteInventoryProvider for LingmaInventoryProvider {
    type Leaf = LingmaDatabaseSourceV0;

    fn parser_revision(&self) -> &'static str {
        LINGMA_SOURCE_BACKED_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let inventory = self.inventory_source.observe().map_err(route_error)?;
        reject_duplicate_lingma_paths(&inventory).map_err(route_error)?;
        let authority_fingerprint =
            sqlite_inventory_authority_fingerprint(inventory.observation())?;
        let leaves = inventory
            .databases()
            .iter()
            .cloned()
            .map(|leaf| {
                Ok(SqliteInventoryCatalogLeaf {
                    source: leaf.source_key()?,
                    path: leaf.path().to_path_buf(),
                    provider_leaf: leaf,
                })
            })
            .collect::<LingmaSourceBackedResultV0<Vec<_>>>()
            .map_err(route_error)?;
        Ok(SqliteInventoryCatalog {
            authority_fingerprint,
            leaves,
        })
    }

    fn scan(
        &self,
        leaf: &Self::Leaf,
        snapshot: SqliteSourceReadSnapshot,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let mut sink_failure = None;
        let certificate = scan_lingma_snapshot_v0(leaf, snapshot, &mut |document| {
            if let Err(error) = sink.emit_document(document) {
                let detail = error.to_string();
                sink_failure = Some(error);
                return Err(LingmaSourceBackedErrorV0::Capture(
                    CaptureError::InvalidPayload(detail),
                ));
            }
            Ok(())
        });
        if let Some(error) = sink_failure {
            return Err(error);
        }
        certificate.map_err(route_error)
    }

    fn hydrate(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let inventory = self.inventory_source.observe().map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?;
        LingmaSourceBackedResolverV0::new(&inventory)
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?
            .hydrate_batch_request(request)
    }
}

pub(in crate::provider::source_backed) fn discovered_lingma_inventory_source(
    discovery: &DiscoveryContext,
    selected_source: &ProviderSource,
) -> Result<Arc<dyn LingmaInventorySource>, SourceBackedAutomaticUnavailableReason> {
    let source = DiscoveredLingmaInventorySource {
        selector: LingmaInventorySelector::new(discovery.clone()),
    };
    let opening = source.selector.observe().map_err(|error| {
        SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
            detail: error.detail(),
        }
    })?;
    if !opening
        .databases()
        .iter()
        .any(|database| database.source() == selected_source)
    {
        return Err(
            SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                detail: "Lingma selected database is absent from its installed-client inventory",
            },
        );
    }
    lingma_adapter_inventory(opening).map_err(|error| {
        SourceBackedAutomaticUnavailableReason::RegistrationRejected {
            detail: error.to_string(),
        }
    })?;
    Ok(Arc::new(source))
}

fn lingma_adapter_inventory(
    inventory: crate::provider_sources::LingmaDiscoveredInventory,
) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0> {
    let authority_key = inventory
        .authority_key()
        .map_err(lingma_discovery_adapter_error)?;
    let databases = inventory
        .databases()
        .iter()
        .map(|database| {
            let lineage = database
                .catalog_lineage()
                .typed_key()
                .map_err(lingma_discovery_adapter_error)?;
            LingmaDatabaseSourceV0::new(database.path(), lineage)
        })
        .collect::<LingmaSourceBackedResultV0<Vec<_>>>()?;
    LingmaSourceInventoryV0::new(authority_key, databases)
}

fn lingma_discovery_adapter_error(error: LingmaDiscoveryUnavailable) -> LingmaSourceBackedErrorV0 {
    CaptureError::InvalidPayload(error.to_string()).into()
}
