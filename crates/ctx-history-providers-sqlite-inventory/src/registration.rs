use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_capture_runtime::{
    CaptureLifecycleSink, DocumentRecordSpool, ReplacementDocumentTree,
    SourceBackedRecordRejectionDrafts, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResult, SourceBackedRouteSelection, SourceBackedRouteWatchTargets,
    SourceBackedSelectorAuthority,
};
use ctx_history_core::{CaptureProvider, CertifiedSource, SourceAnchorScope, TypedKey};
use ctx_history_source_discovery::{LingmaDiscoveredInventory, LingmaDiscoveryUnavailable};

use sha2::Digest;

use crate::provider::providers::lingma::native_path::{
    reject_duplicate_paths as reject_duplicate_lingma_paths, scan_lingma_snapshot_v0,
    LingmaDatabaseSourceV0, LingmaSourceBackedErrorV0, LingmaSourceBackedResultV0,
    LingmaSourceInventoryV0, LINGMA_SOURCE_BACKED_PARSER_REVISION,
};
use crate::provider::providers::shelley::native_path::source_backed::{
    discover_shelley_source_backed_exact_cwd_scoped, ShelleySourceBackedAdapter,
    SHELLEY_SOURCE_PARSER_REVISION,
};
use crate::{
    provider::providers::astrbot::native_path::source_backed::{
        scan_astrbot_snapshot_v0, AstrBotSourceBackedErrorV0, AstrBotSourceBackedInventoryV0,
        AstrBotSourceBackedResultV0, AstrBotSourceBackedSourceV0,
        PARSER_REVISION as ASTRBOT_SOURCE_BACKED_PARSER_REVISION,
    },
    provider::source_backed::family::document::ChangedDocumentSink,
    provider::source_backed::{
        combine_primary_and_cleanup_route_errors, route_error, sqlite_rejection_draft,
        SourceBackedRecordRejectionClass,
    },
    provider_sources::SqliteSourceReadSnapshot,
};

use super::*;

mod astrbot_released;
mod crush;
mod shared;

pub use astrbot_released::{
    astrbot_released_registration_scoped, AstrBotReleasedInventoryProvider,
};
pub use crush::{crush_registration, crush_registration_scoped};
use shared::{
    sqlite_inventory_authority_fingerprint, SqliteInventoryCatalog, SqliteInventoryCatalogLeaf,
    SqliteInventoryDocumentAdapter, SqliteInventoryProvider,
};

pub type WatchTargets =
    Box<dyn Fn() -> Option<SourceBackedRouteWatchTargets> + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteInventoryCoverage {
    Complete,
    SelectedSubset,
}

/// Complete provider-owned registration contract. Capture consumes this
/// fragment only to bind its concrete lifecycle and install one executable
/// route; all provider selection and watch authority is fixed here.
pub struct SqliteInventoryRegistration<A> {
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
    adapter: A,
    watch_targets: Option<WatchTargets>,
}

impl<A> SqliteInventoryRegistration<A> {
    fn new(
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
        selector_authority: SourceBackedSelectorAuthority,
        adapter: A,
        watch_targets: Option<WatchTargets>,
    ) -> Self {
        Self {
            source,
            selection,
            selector_authority,
            adapter,
            watch_targets,
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        ProviderSource,
        SourceBackedRouteSelection,
        SourceBackedSelectorAuthority,
        A,
        Option<WatchTargets>,
    ) {
        (
            self.source,
            self.selection,
            self.selector_authority,
            self.adapter,
            self.watch_targets,
        )
    }
}

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
pub fn astrbot_registration<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    discovery: DiscoveryContext,
) -> SqliteInventoryRegistration<
    impl ReplacementDocumentTree<
        Lifecycle = L,
        Spool = S,
        RouteControl = crate::ProviderRouteControlExpectation,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    astrbot_registration_scoped(
        source,
        selection,
        data_root,
        discovery,
        SourceAnchorScope::Unqualified,
    )
}

pub fn astrbot_registration_scoped<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    discovery: DiscoveryContext,
    source_scope: SourceAnchorScope,
) -> SqliteInventoryRegistration<
    impl ReplacementDocumentTree<
        Lifecycle = L,
        Spool = S,
        RouteControl = crate::ProviderRouteControlExpectation,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    let watch_primary = source.path.clone();
    let watch_discovery = discovery.clone();
    let adapter = SqliteInventoryDocumentAdapter::new(
        data_root,
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        AstrBotInventoryProvider {
            discovery,
            source_scope,
        },
    );
    SqliteInventoryRegistration::new(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        adapter,
        Some(Box::new(move || {
            let mut targets =
                AstrBotSourceBackedInventoryV0::discover_scoped(&watch_discovery, source_scope)
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
        })),
    )
}

pub struct AstrBotInventoryProvider {
    discovery: DiscoveryContext,
    source_scope: SourceAnchorScope,
}

impl<L, S> SqliteInventoryProvider<L, S> for AstrBotInventoryProvider
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    type Leaf = AstrBotSourceBackedSourceV0;

    fn parser_revision(&self) -> &'static str {
        ASTRBOT_SOURCE_BACKED_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let inventory =
            AstrBotSourceBackedInventoryV0::discover_scoped(&self.discovery, self.source_scope)
                .map_err(astrbot_inventory_route_error)?;
        let authority_fingerprint =
            sqlite_inventory_authority_fingerprint(inventory.observation())?;
        let leaves = inventory
            .sources()
            .iter()
            .cloned()
            .map(|leaf| SqliteInventoryCatalogLeaf {
                source: leaf.source_key().clone(),
                physical_locator: leaf.path().to_path_buf(),
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
        sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        scan_astrbot_inventory_leaf(leaf, snapshot, sink)
    }
}

fn scan_astrbot_inventory_leaf<L, S>(
    leaf: &AstrBotSourceBackedSourceV0,
    snapshot: SqliteSourceReadSnapshot,
    sink: &mut ChangedDocumentSink<'_, '_, L, S>,
) -> SourceBackedRouteResult<CertifiedSource>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    let mut sink_failure = None;
    let mut rejections = SourceBackedRecordRejectionDrafts::default();
    let certificate = scan_astrbot_snapshot_v0(
        leaf,
        snapshot,
        &mut |record| {
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
        },
        &mut rejections,
    );
    let certificate = astrbot_scan_route_result(sink_failure, certificate)?;
    sink.record_rejections(rejections);
    Ok(certificate)
}

fn astrbot_scan_route_result(
    sink_failure: Option<SourceBackedRouteError>,
    certificate: AstrBotSourceBackedResultV0<CertifiedSource>,
) -> SourceBackedRouteResult<CertifiedSource> {
    if let Some(primary) = sink_failure {
        if let Err(AstrBotSourceBackedErrorV0::SnapshotCleanup { cleanup, .. }) = certificate {
            return Err(combine_primary_and_cleanup_route_errors(
                primary,
                sqlite_source_route_error(cleanup),
            ));
        }
        return Err(primary);
    }
    certificate.map_err(astrbot_inventory_route_error)
}

fn astrbot_inventory_route_error(
    error: crate::provider::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0,
) -> SourceBackedRouteError {
    use crate::provider::providers::astrbot::native_path::source_backed::AstrBotSourceBackedErrorV0;
    if let AstrBotSourceBackedErrorV0::SnapshotCleanup { primary, cleanup } = error {
        return combine_primary_and_cleanup_route_errors(
            astrbot_inventory_route_error(*primary),
            sqlite_source_route_error(cleanup),
        );
    }
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

/// Registers Shelley only from the exact CWD which owns the selected
/// `shelley.db`. Automatic callers supply the discovery CWD; explicit callers
/// derive it only from their approved exact database path. No branch or
/// fallback CWD is inferred.
pub fn shelley_registration<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    exact_cwd: impl Into<PathBuf>,
) -> SourceBackedRouteResult<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    shelley_registration_scoped(
        source,
        selection,
        data_root,
        exact_cwd,
        SourceAnchorScope::Unqualified,
    )
}

pub fn shelley_registration_scoped<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    exact_cwd: impl Into<PathBuf>,
    source_scope: SourceAnchorScope,
) -> SourceBackedRouteResult<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    let exact_cwd = exact_cwd.into();
    let adapter =
        discover_shelley_source_backed_exact_cwd_scoped(data_root, &exact_cwd, source_scope)
            .map_err(shelley_inventory_route_error)?
            .ok_or_else(|| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "the exact Shelley CWD no longer contains an admitted database".to_owned(),
                )
            })?;
    if adapter.database_path() != source.path {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            "the Shelley source path does not belong to the supplied exact CWD".to_owned(),
        ));
    }
    let adapter = SqliteInventoryDocumentAdapter::new(
        data_root,
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        ShelleyInventoryProvider { exact_cwd, adapter },
    );
    Ok(SqliteInventoryRegistration::new(
        source,
        selection,
        match selection {
            SourceBackedRouteSelection::Automatic => SourceBackedSelectorAuthority::ExactCwd,
            // An exact --path is a path-bound escape hatch. It must not gain
            // automatic exact-CWD authority merely because the SQLite adapter
            // needs the database's parent for safe leaf validation.
            SourceBackedRouteSelection::ExplicitManual => {
                SourceBackedSelectorAuthority::ExplicitPath
            }
        },
        adapter,
        None,
    ))
}

pub struct ShelleyInventoryProvider {
    exact_cwd: PathBuf,
    adapter: ShelleySourceBackedAdapter,
}

impl<L, S> SqliteInventoryProvider<L, S> for ShelleyInventoryProvider
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
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
                physical_locator: leaf.database_path().to_path_buf(),
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
        sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let mut scan = leaf
            .start_snapshot_scan(snapshot)
            .map_err(shelley_inventory_route_error)?;
        loop {
            let page = match scan.next_page() {
                Ok(Some(page)) => page,
                Ok(None) => break,
                Err(primary) => {
                    let primary = shelley_inventory_route_error(primary);
                    return Err(abort_shelley_inventory_scan(scan, primary));
                }
            };
            for rejection in page.rejections {
                sink.record_rejection(sqlite_rejection_draft(
                    leaf.source(),
                    CaptureProvider::Shelley,
                    leaf.database_path(),
                    u64::try_from(rejection.rowid).unwrap_or_default(),
                    SourceBackedRecordRejectionClass::UnsupportedRecord,
                    rejection.reason,
                ));
            }
            for document in page.documents {
                if let Err(primary) = sink.emit_core_record(document) {
                    return Err(abort_shelley_inventory_scan(scan, primary));
                }
            }
        }
        Ok(scan
            .finish()
            .map_err(shelley_inventory_route_error)?
            .certificate)
    }
}

pub fn lingma_registration<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    authority_key: TypedKey,
    databases: Vec<(PathBuf, TypedKey)>,
) -> Result<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    lingma_registration_scoped(
        source,
        selection,
        data_root,
        authority_key,
        databases,
        SourceAnchorScope::Unqualified,
        SqliteInventoryCoverage::Complete,
    )
}

pub fn lingma_registration_scoped<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    authority_key: TypedKey,
    databases: Vec<(PathBuf, TypedKey)>,
    source_scope: SourceAnchorScope,
    coverage: SqliteInventoryCoverage,
) -> Result<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    let databases = databases
        .into_iter()
        .map(|(path, lineage)| LingmaDatabaseSourceV0::new_scoped(path, lineage, source_scope))
        .collect::<LingmaSourceBackedResultV0<Vec<_>>>()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let inventory = LingmaSourceInventoryV0::new(authority_key, databases)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(lingma_inventory_registration(
        source,
        selection,
        data_root,
        Arc::new(FixedLingmaInventorySource { inventory }),
        coverage,
    ))
}

trait LingmaInventorySource: Send + Sync {
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

struct DiscoveredLingmaInventorySource<F> {
    observe: F,
    source_scope: SourceAnchorScope,
}

impl<F> LingmaInventorySource for DiscoveredLingmaInventorySource<F>
where
    F: Fn() -> std::result::Result<LingmaDiscoveredInventory, LingmaDiscoveryUnavailable>
        + Send
        + Sync,
{
    fn observe(&self) -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0> {
        (self.observe)()
            .map_err(lingma_discovery_adapter_error)
            .and_then(|inventory| lingma_adapter_inventory(inventory, self.source_scope))
    }
}

fn lingma_inventory_registration<L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    inventory_source: Arc<dyn LingmaInventorySource>,
    coverage: SqliteInventoryCoverage,
) -> SqliteInventoryRegistration<
    impl ReplacementDocumentTree<
        Lifecycle = L,
        Spool = S,
        RouteControl = crate::ProviderRouteControlExpectation,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    let watch_inventory = Arc::clone(&inventory_source);
    let adapter = SqliteInventoryDocumentAdapter::new(
        data_root,
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        LingmaInventoryProvider { inventory_source },
    )
    .with_coverage(coverage);
    SqliteInventoryRegistration::new(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        adapter,
        Some(Box::new(move || {
            let inventory = watch_inventory.observe().ok()?;
            Some(sqlite_inventory_watch_targets(
                inventory
                    .databases()
                    .iter()
                    .map(LingmaDatabaseSourceV0::path),
            ))
        })),
    )
}

pub struct LingmaInventoryProvider {
    inventory_source: Arc<dyn LingmaInventorySource>,
}

impl<L, S> SqliteInventoryProvider<L, S> for LingmaInventoryProvider
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
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
                    physical_locator: leaf.path().to_path_buf(),
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
        sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let mut sink_failure = None;
        let mut rejections = SourceBackedRecordRejectionDrafts::default();
        let certificate = scan_lingma_snapshot_v0(
            leaf,
            snapshot,
            &mut |record| {
                if let Err(error) = sink.emit_core_record(record) {
                    let detail = error.to_string();
                    sink_failure = Some(error);
                    return Err(LingmaSourceBackedErrorV0::Capture(
                        CaptureError::InvalidPayload(detail),
                    ));
                }
                Ok(())
            },
            &mut rejections,
        );
        let certificate = lingma_scan_route_result(sink_failure, certificate)?;
        sink.record_rejections(rejections);
        Ok(certificate)
    }
}

fn lingma_scan_route_result(
    sink_failure: Option<SourceBackedRouteError>,
    certificate: LingmaSourceBackedResultV0<CertifiedSource>,
) -> SourceBackedRouteResult<CertifiedSource> {
    if let Some(primary) = sink_failure {
        if let Err(LingmaSourceBackedErrorV0::SnapshotCleanup { cleanup, .. }) = certificate {
            return Err(combine_primary_and_cleanup_route_errors(
                primary,
                sqlite_source_route_error(cleanup),
            ));
        }
        return Err(primary);
    }
    certificate.map_err(lingma_inventory_route_error)
}

fn shelley_inventory_route_error(
    error: crate::provider::providers::shelley::native_path::source_backed::ShelleySourceBackedError,
) -> SourceBackedRouteError {
    use crate::provider::providers::shelley::native_path::source_backed::ShelleySourceBackedError;
    if let ShelleySourceBackedError::SnapshotCleanup { primary, cleanup } = error {
        return combine_primary_and_cleanup_route_errors(
            shelley_inventory_route_error(*primary),
            sqlite_source_route_error(cleanup),
        );
    }
    let kind = match &error {
        ShelleySourceBackedError::SqliteSource(error) => sqlite_source_route_error_kind(error),
        ShelleySourceBackedError::Capture(error) => {
            sqlite_capture_route_error(error).unwrap_or(SourceBackedRouteErrorKind::InvalidSource)
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

fn abort_shelley_inventory_scan(
    scan: crate::provider::providers::shelley::native_path::source_backed::ShelleySourceBackedScan,
    primary: SourceBackedRouteError,
) -> SourceBackedRouteError {
    match scan.abort() {
        Ok(()) => primary,
        Err(cleanup) => {
            combine_primary_and_cleanup_route_errors(primary, sqlite_source_route_error(cleanup))
        }
    }
}

fn lingma_inventory_route_error(error: LingmaSourceBackedErrorV0) -> SourceBackedRouteError {
    if let LingmaSourceBackedErrorV0::SnapshotCleanup { primary, cleanup } = error {
        return combine_primary_and_cleanup_route_errors(
            lingma_inventory_route_error(*primary),
            sqlite_source_route_error(cleanup),
        );
    }
    let kind = match &error {
        LingmaSourceBackedErrorV0::SqliteSource(error) => sqlite_source_route_error_kind(error),
        LingmaSourceBackedErrorV0::Capture(error) => {
            sqlite_capture_route_error(error).unwrap_or(SourceBackedRouteErrorKind::InvalidSource)
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LingmaRegistrationError {
    SelectorAuthorityUnavailable(&'static str),
    RegistrationRejected(String),
}

pub fn discovered_lingma_registration<L, S, F>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    observe: F,
) -> std::result::Result<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
    LingmaRegistrationError,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
    F: Fn() -> std::result::Result<LingmaDiscoveredInventory, LingmaDiscoveryUnavailable>
        + Send
        + Sync
        + 'static,
{
    discovered_lingma_registration_scoped(
        source,
        selection,
        data_root,
        observe,
        SourceAnchorScope::Unqualified,
        SqliteInventoryCoverage::Complete,
    )
}

pub fn discovered_lingma_registration_scoped<L, S, F>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    observe: F,
    source_scope: SourceAnchorScope,
    coverage: SqliteInventoryCoverage,
) -> std::result::Result<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
    LingmaRegistrationError,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
    F: Fn() -> std::result::Result<LingmaDiscoveredInventory, LingmaDiscoveryUnavailable>
        + Send
        + Sync
        + 'static,
{
    let inventory = discovered_lingma_inventory_source(&source, observe, source_scope)?;
    Ok(lingma_inventory_registration(
        source, selection, data_root, inventory, coverage,
    ))
}

fn discovered_lingma_inventory_source<F>(
    selected_source: &ProviderSource,
    observe: F,
    source_scope: SourceAnchorScope,
) -> std::result::Result<Arc<dyn LingmaInventorySource>, LingmaRegistrationError>
where
    F: Fn() -> std::result::Result<LingmaDiscoveredInventory, LingmaDiscoveryUnavailable>
        + Send
        + Sync
        + 'static,
{
    let source = DiscoveredLingmaInventorySource {
        observe,
        source_scope,
    };
    let opening = (source.observe)()
        .map_err(|error| LingmaRegistrationError::SelectorAuthorityUnavailable(error.detail()))?;
    if !opening
        .databases()
        .iter()
        .any(|database| database.source() == selected_source)
    {
        return Err(LingmaRegistrationError::SelectorAuthorityUnavailable(
            "Lingma selected database is absent from its installed-client inventory",
        ));
    }
    lingma_adapter_inventory(opening, source_scope)
        .map_err(|error| LingmaRegistrationError::RegistrationRejected(error.to_string()))?;
    Ok(Arc::new(source))
}

pub(crate) fn sqlite_source_route_error(
    error: crate::provider_sources::SqliteSourceAccessError,
) -> SourceBackedRouteError {
    SourceBackedRouteError::new(sqlite_source_route_error_kind(&error), error.to_string())
}

pub(crate) fn sqlite_source_route_error_kind(
    error: &crate::provider_sources::SqliteSourceAccessError,
) -> SourceBackedRouteErrorKind {
    if error.is_source_changed() {
        SourceBackedRouteErrorKind::SourceChanged
    } else if error.is_snapshot_capacity_failure() {
        SourceBackedRouteErrorKind::Unavailable
    } else if error.is_systemic_resource_failure() {
        SourceBackedRouteErrorKind::ResourceUnavailable
    } else if sqlite_provider_artifact_is_busy_or_locked(error) {
        SourceBackedRouteErrorKind::Unavailable
    } else if error.is_ctx_owned_corruption() {
        SourceBackedRouteErrorKind::Internal
    } else if error.is_provider_corruption() || error.is_provider_path_unavailable() {
        SourceBackedRouteErrorKind::InvalidSource
    } else if error.is_operational_failure() {
        SourceBackedRouteErrorKind::Internal
    } else {
        SourceBackedRouteErrorKind::InvalidSource
    }
}

fn sqlite_provider_artifact_is_busy_or_locked(
    error: &crate::provider_sources::SqliteSourceAccessError,
) -> bool {
    error.is_busy_or_locked()
        && error.diagnostic().is_some_and(|diagnostic| {
            matches!(
                diagnostic.artifact,
                crate::provider_sources::SqliteArtifactKind::ProviderDatabase
                    | crate::provider_sources::SqliteArtifactKind::ProviderWal
                    | crate::provider_sources::SqliteArtifactKind::ProviderSharedMemory
            )
        })
}

pub(crate) fn sqlite_capture_route_error(
    error: &CaptureError,
) -> Option<SourceBackedRouteErrorKind> {
    match error {
        CaptureError::SourceChangedDuringCapture => Some(SourceBackedRouteErrorKind::SourceChanged),
        CaptureError::Io(error) | CaptureError::SystemIo { source: error, .. }
            if crate::provider_sources::resource_exhaustion_io_error(error) =>
        {
            Some(SourceBackedRouteErrorKind::ResourceUnavailable)
        }
        CaptureError::Sqlite(error)
            if crate::provider_sources::rusqlite_resource_failure(error) =>
        {
            Some(SourceBackedRouteErrorKind::ResourceUnavailable)
        }
        CaptureError::Io(_) | CaptureError::SystemIo { .. } | CaptureError::Sqlite(_) => {
            Some(SourceBackedRouteErrorKind::Internal)
        }
        _ => None,
    }
}

fn lingma_adapter_inventory(
    inventory: LingmaDiscoveredInventory,
    source_scope: SourceAnchorScope,
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
            LingmaDatabaseSourceV0::new_scoped(database.path(), lineage, source_scope)
        })
        .collect::<LingmaSourceBackedResultV0<Vec<_>>>()?;
    LingmaSourceInventoryV0::new(authority_key, databases)
}

fn lingma_discovery_adapter_error(error: LingmaDiscoveryUnavailable) -> LingmaSourceBackedErrorV0 {
    CaptureError::InvalidPayload(error.to_string()).into()
}

#[cfg(test)]
pub(crate) mod tests;
