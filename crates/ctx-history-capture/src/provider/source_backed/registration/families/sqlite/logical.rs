use super::*;
use crate::provider::{
    providers::trae::nativepath::TraeReplacementTree,
    source_backed::family::document::register_replacement_document_tree_route_with_authority,
};

pub(super) fn register_forgecode_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    make_selection: impl Fn(std::path::PathBuf, &Path) -> ForgeCodeSourceSelectionV0
        + Send
        + Sync
        + Clone
        + 'static,
) -> SourceBackedCoordinatorResult<()> {
    let authority = if selection == SourceBackedRouteSelection::Automatic {
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit
    } else {
        SourceBackedSelectorAuthority::ExplicitPath
    };
    let adapter = make_selection(source.path.clone(), data_root);
    register_replacement_document_tree_route_with_authority(
        registry, source, selection, authority, adapter,
    )
}
pub(super) fn register_deepagents_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    let adapter = DeepAgentsDatabaseSelectionV0::explicit(data_root, source.path.clone());
    register_replacement_document_tree_route_with_authority(
        registry,
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        adapter,
    )
}
pub(super) fn register_opencode_family_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    crate::provider::providers::opencode::native_path::source_backed::register_source_backed_route(
        registry, source, selection, data_root,
    )
}

pub(super) fn register_hermes_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    if selection != SourceBackedRouteSelection::Automatic {
        return Err(invalid_route(
            source.provider,
            "manual Hermes registration requires a persistent explicit SourceAnchor",
        ));
    }
    let candidate = HermesSourceCandidate::automatic(data_root, source.clone())
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    register_hermes_candidate(
        registry,
        source,
        selection,
        candidate,
        SourceBackedSelectorAuthority::DiscoveredWinner,
    )
}

pub(super) fn register_hermes_candidate(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    candidate: HermesSourceCandidate,
    authority: SourceBackedSelectorAuthority,
) -> SourceBackedCoordinatorResult<()> {
    register_replacement_document_tree_route_with_authority(
        registry, source, selection, authority, candidate,
    )
}
pub(super) fn register_trae_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    let adapter = TraeReplacementTree::new(data_root, source.path.clone());
    register_replacement_document_tree_route_with_authority(
        registry,
        source,
        selection,
        SourceBackedSelectorAuthority::ExplicitPath,
        adapter,
    )
}

pub(super) fn register_zed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let revalidation_path = path.clone();
    let hydration_path = path;
    let capture_data_root = data_root.to_path_buf();
    let revalidation_data_root = data_root.to_path_buf();
    let hydration_data_root = data_root.to_path_buf();
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let source_key = zed_source_key().map_err(route_error)?;
            let mut snapshot =
                acquire_zed_snapshot(&capture_data_root, &capture_path).map_err(route_error)?;
            let snapshot_revision = snapshot.snapshot_revision.clone();
            let physical_locator = snapshot.physical_locator.clone();
            let revision_digest = zed_snapshot_revision_digest(&snapshot_revision);
            sink.begin_source(source_key.clone())
                .map_err(route_coordinator_error)?;
            let connection = snapshot.connection().map_err(route_error)?;
            let mut zed_sink = ZedSourceBackedSinkV0::new(
                sink.writer,
                connection,
                source_key.clone(),
                revision_digest,
                capture_path.to_string_lossy().into_owned(),
            )
            .map_err(route_error)?;
            let scan = scan_zed_native_snapshot(
                connection,
                &physical_locator,
                &snapshot_revision,
                &mut zed_sink,
            )
            .map_err(route_error)?;
            if let Some(error) = zed_sink.take_failure() {
                return Err(route_error(error));
            }
            let staged_documents = zed_sink.staged_documents();
            drop(zed_sink);
            snapshot.finish().map_err(route_error)?;
            if staged_documents != scan.counters.retained_events {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "Zed source-backed counts do not reconcile",
                ));
            }
            let complete_records = scan
                .counters
                .retained_events
                .checked_add(scan.counters.rejected_threads)
                .ok_or_else(|| {
                    SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "Zed source-backed counts overflowed",
                    )
                })?;
            let counts = ScannedSourceCounts {
                complete_records,
                retained_records: scan.counters.retained_events,
                rejected_records: scan.counters.rejected_threads,
                ignored_records: 0,
                indexed_documents: staged_documents,
                certified_bytes: scan.counters.certified_logical_bytes,
            };
            let observation =
                zed_source_observation(&source_key, &snapshot_revision).map_err(route_error)?;
            let certificate = CertifiedSource::certify(
                observation.clone(),
                observation,
                "zed-nativepath-source-backed-v0",
                decode_zed_digest(&scan.source_integrity_digest).map_err(route_error)?,
                counts,
            )
            .map_err(route_error)?;
            sink.certify_source(certificate)
                .map_err(route_coordinator_error)
        },
        provider_format_scope(CaptureProvider::Zed, "zed_threads_sqlite"),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                let Ok(source_key) = zed_source_key() else {
                    return false;
                };
                let Ok(mut snapshot) =
                    acquire_zed_snapshot(&revalidation_data_root, &revalidation_path)
                else {
                    return false;
                };
                let observation = zed_source_observation(&source_key, &snapshot.snapshot_revision);
                observation.is_ok_and(|observation| observation == *expected.observation())
                    && snapshot.finish().is_ok()
            }
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
        move |request| {
            ZedLocatorResolverV0::new(&hydration_data_root, &hydration_path)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
pub(super) fn register_forgecode_selected_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    if selection != SourceBackedRouteSelection::Automatic {
        return Err(invalid_route(
            source.provider,
            "manual ForgeCode registration requires explicit catalog lineage",
        ));
    }
    register_forgecode_route(registry, source, selection, data_root, |path, data_root| {
        ForgeCodeSourceSelectionV0::selected(data_root, path)
    })
}

pub fn register_forgecode_explicit_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    data_root: &Path,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    register_forgecode_route(
        registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        data_root,
        move |path, data_root| {
            ForgeCodeSourceSelectionV0::explicit(data_root, path, catalog_lineage)
        },
    )
}
