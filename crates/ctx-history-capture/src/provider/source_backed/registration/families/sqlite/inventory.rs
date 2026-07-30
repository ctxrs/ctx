use std::path::PathBuf;

use sha2::Digest;

use crate::{
    provider::source_backed::family::document::ChangedDocumentSink,
    provider_sources::SqliteSourceReadSnapshot,
};

use super::*;

mod crush;
mod hermes;
mod shared;

pub use crush::register_crush_source_backed_route;
pub use hermes::register_hermes_explicit_source_backed_route;
use shared::{
    sqlite_inventory_authority_fingerprint, SqliteInventoryCatalog, SqliteInventoryCatalogLeaf,
    SqliteInventoryDocumentAdapter, SqliteInventoryProvider,
};

/// Registers AstrBot's complete selected/launcher inventory from the same
/// bounded discovery context used by provider selection.
pub fn register_astrbot_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    discovery: DiscoveryContext,
) -> SourceBackedCoordinatorResult<()> {
    SqliteInventoryDocumentAdapter::register_replacement_document_tree_route(
        registry,
        source,
        selection,
        data_root,
        CaptureProvider::AstrBot,
        "astrbot_data_v4_sqlite",
        AstrBotInventoryProvider {
            data_root: data_root.to_path_buf(),
            discovery,
        },
    )
}

struct AstrBotInventoryProvider {
    data_root: PathBuf,
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
        AstrBotSourceBackedResolverV0::from_inventory(&self.data_root, &inventory)
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
    data_root: &Path,
    exact_cwd: impl Into<PathBuf>,
) -> SourceBackedCoordinatorResult<()> {
    let exact_cwd = exact_cwd.into();
    let adapter = discover_shelley_source_backed_exact_cwd(data_root, &exact_cwd)
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
    SqliteInventoryDocumentAdapter::register_replacement_document_tree_route_with_authority(
        registry,
        source,
        SourceBackedRouteSelection::Automatic,
        SourceBackedSelectorAuthority::ExactCwd,
        data_root,
        CaptureProvider::Shelley,
        "shelley_sqlite",
        ShelleyInventoryProvider {
            data_root: data_root.to_path_buf(),
            exact_cwd,
        },
    )
}

struct ShelleyInventoryProvider {
    data_root: PathBuf,
    exact_cwd: PathBuf,
}

impl SqliteInventoryProvider for ShelleyInventoryProvider {
    type Leaf = ShelleySourceBackedAdapter;

    fn parser_revision(&self) -> &'static str {
        SHELLEY_SOURCE_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let adapter = discover_shelley_source_backed_exact_cwd(&self.data_root, &self.exact_cwd)
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
        let adapter = discover_shelley_source_backed_exact_cwd(&self.data_root, &self.exact_cwd)
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

pub fn register_lingma_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    authority_key: TypedKey,
    databases: Vec<(PathBuf, TypedKey)>,
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
        data_root,
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
    data_root: &Path,
    inventory_source: Arc<dyn LingmaInventorySource>,
) -> SourceBackedCoordinatorResult<()> {
    SqliteInventoryDocumentAdapter::register_replacement_document_tree_route(
        registry,
        source,
        selection,
        data_root,
        CaptureProvider::Lingma,
        "lingma_sqlite",
        LingmaInventoryProvider {
            data_root: data_root.to_path_buf(),
            inventory_source,
        },
    )
}

struct LingmaInventoryProvider {
    data_root: PathBuf,
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
        LingmaSourceBackedResolverV0::new(&self.data_root, &inventory)
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
