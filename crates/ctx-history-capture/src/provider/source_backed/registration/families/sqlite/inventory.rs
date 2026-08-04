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

fn sqlite_inventory_watch_targets<'a>(
    databases: impl IntoIterator<Item = &'a Path>,
) -> SourceBackedRouteWatchTargets {
    let mut targets = SourceBackedRouteWatchTargets::default();
    for database in databases {
        targets.sqlite_databases.insert(database.to_path_buf());
        if let Some(parent) = database.parent() {
            targets.authority_paths.insert(parent.to_path_buf());
        }
    }
    targets
}

/// Registers AstrBot's complete selected/launcher inventory from the same
/// bounded discovery context used by provider selection.
pub fn register_astrbot_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    discovery: DiscoveryContext,
) -> SourceBackedCoordinatorResult<()> {
    let watch_source = source.clone();
    let watch_primary = source.path.clone();
    let watch_discovery = discovery.clone();
    SqliteInventoryDocumentAdapter::register_replacement_document_tree_route(
        registry,
        source,
        selection,
        data_root,
        CaptureProvider::AstrBot,
        "astrbot_data_v4_sqlite",
        AstrBotInventoryProvider { discovery },
    )?;
    registry.attach_route_watch_targets(&watch_source, move || {
        let mut targets = AstrBotSourceBackedInventoryV0::discover(&watch_discovery)
            .ok()
            .map(|inventory| {
                sqlite_inventory_watch_targets(
                    inventory
                        .sources()
                        .iter()
                        .map(AstrBotSourceBackedSourceV0::path),
                )
            })
            .unwrap_or_default();
        // Retain exact provider authority roots even when an inventory probe
        // fails. That keeps warm observation indeterminate while ensuring a
        // healthy watcher still dirties the route for selected-root changes,
        // launcher-instance changes, and newly created finite leaves.
        if let Some(parent) = watch_primary.parent() {
            targets.authority_paths.insert(parent.to_path_buf());
        }
        targets.authority_paths.insert(
            watch_discovery
                .home()
                .join(".astrbot_launcher")
                .join("instances"),
        );
        Some(targets)
    })
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
        let certificate = scan_astrbot_snapshot_v0(leaf, snapshot, &mut |record| {
            if let Err(error) = sink.emit_core_record(record) {
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
        certificate.map_err(astrbot_inventory_route_error)
    }
}

fn astrbot_inventory_route_error(
    error: crate::provider::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0,
) -> SourceBackedRouteError {
    use crate::provider::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0;
    let kind = match &error {
        AstrBotSourceBackedErrorV0::IncompleteInventory { .. } => {
            SourceBackedRouteErrorKind::Unavailable
        }
        AstrBotSourceBackedErrorV0::SqliteSource(error) => sqlite_source_route_error_kind(error),
        AstrBotSourceBackedErrorV0::Capture(error) => {
            sqlite_capture_route_error(error).unwrap_or(SourceBackedRouteErrorKind::InvalidSource)
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
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
        ShelleyInventoryProvider { exact_cwd, adapter },
    )
}

struct ShelleyInventoryProvider {
    exact_cwd: PathBuf,
    adapter: ShelleySourceBackedAdapter,
}

impl SqliteInventoryProvider for ShelleyInventoryProvider {
    type Leaf = ShelleySourceBackedAdapter;

    fn parser_revision(&self) -> &'static str {
        SHELLEY_SOURCE_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let mut authority = sha2::Sha256::new();
        authority.update(b"ctx.shelley-exact-cwd-inventory-v1\0");
        authority.update(self.exact_cwd.as_os_str().as_encoded_bytes());
        let leaf = self.adapter.clone();
        let leaves = match std::fs::symlink_metadata(leaf.database_path()) {
            Ok(_) => vec![SqliteInventoryCatalogLeaf {
                source: leaf.source().clone(),
                path: leaf.database_path().to_path_buf(),
                provider_leaf: leaf,
            }],
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    format!("Shelley exact-CWD inventory is unavailable: {error}"),
                ));
            }
        };
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
        let mut scan = leaf
            .start_snapshot_scan(snapshot)
            .map_err(shelley_inventory_route_error)?;
        while let Some(page) = scan.next_page().map_err(shelley_inventory_route_error)? {
            for document in page.documents {
                sink.emit_core_record(document)?;
            }
        }
        Ok(scan
            .finish()
            .map_err(shelley_inventory_route_error)?
            .certificate)
    }
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
    let watch_source = source.clone();
    let watch_inventory = Arc::clone(&inventory_source);
    SqliteInventoryDocumentAdapter::register_replacement_document_tree_route(
        registry,
        source,
        selection,
        data_root,
        CaptureProvider::Lingma,
        "lingma_sqlite",
        LingmaInventoryProvider { inventory_source },
    )?;
    registry.attach_route_watch_targets(&watch_source, move || {
        let inventory = watch_inventory.observe().ok()?;
        Some(sqlite_inventory_watch_targets(
            inventory
                .databases()
                .iter()
                .map(LingmaDatabaseSourceV0::path),
        ))
    })
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
        let certificate = scan_lingma_snapshot_v0(leaf, snapshot, &mut |record| {
            if let Err(error) = sink.emit_core_record(record) {
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
        certificate.map_err(lingma_inventory_route_error)
    }
}

fn shelley_inventory_route_error(
    error: crate::provider::providers::shelley::native_path::source_backed::ShelleySourceBackedError,
) -> SourceBackedRouteError {
    use crate::provider::providers::shelley::native_path::source_backed::ShelleySourceBackedError;
    let kind = match &error {
        ShelleySourceBackedError::SqliteSource(error) => sqlite_source_route_error_kind(error),
        ShelleySourceBackedError::Capture(error) => {
            sqlite_capture_route_error(error).unwrap_or(SourceBackedRouteErrorKind::InvalidSource)
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

fn lingma_inventory_route_error(error: LingmaSourceBackedErrorV0) -> SourceBackedRouteError {
    let kind = match &error {
        LingmaSourceBackedErrorV0::SqliteSource(error) => sqlite_source_route_error_kind(error),
        LingmaSourceBackedErrorV0::Capture(error) => {
            sqlite_capture_route_error(error).unwrap_or(SourceBackedRouteErrorKind::InvalidSource)
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
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

#[cfg(test)]
#[path = "inventory/tests.rs"]
mod tests;
