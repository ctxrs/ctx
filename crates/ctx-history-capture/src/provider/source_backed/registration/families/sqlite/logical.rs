use std::sync::Mutex;

use super::*;
use crate::provider::{
    providers::trae::nativepath::TraeReplacementTree,
    source_backed::family::document::{
        register_replacement_document_tree_route_with_authority, ChangedDocumentSink,
        CompleteDocumentTree, DocumentLeafFingerprint, DocumentSourceTerminal,
        ObservedDocumentLeaf, ReplacementDocumentTree,
    },
};
use crate::ZED_THREADS_SQLITE_SOURCE_FORMAT;

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
    let adapter = ZedReplacementTree::new(data_root, source.path.clone());
    register_replacement_document_tree_route_with_authority(
        registry,
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        adapter,
    )
}

const ZED_REPLACEMENT_PARSER_REVISION: &str = "zed-nativepath-source-backed-v0";

#[derive(Debug, Clone)]
struct ZedReplacementTree {
    data_root: PathBuf,
    path: PathBuf,
}

impl ZedReplacementTree {
    fn new(data_root: &Path, path: PathBuf) -> Self {
        Self {
            data_root: data_root.to_path_buf(),
            path,
        }
    }
}

struct ZedTreeAuthority {
    snapshot:
        Mutex<Option<crate::provider::providers::zed::native_path::ZedImmutableSqliteSnapshot>>,
    terminal_revalidate: Box<
        dyn Fn() -> std::result::Result<
                (),
                crate::provider::providers::zed::native_path::ZedNativePathError,
            > + Send
            + Sync
            + 'static,
    >,
}

impl ReplacementDocumentTree for ZedReplacementTree {
    type Leaf = SourceKey;
    type TreeAuthority = ZedTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        ZED_REPLACEMENT_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == CaptureProvider::Zed.as_str()
            && source.source_format() == ZED_THREADS_SQLITE_SOURCE_FORMAT
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let snapshot = acquire_zed_snapshot(&self.data_root, &self.path).map_err(route_error)?;
        let fingerprint =
            DocumentLeafFingerprint::new(zed_snapshot_revision_digest(&snapshot.snapshot_revision));
        let terminal_revalidate = snapshot.terminal_revalidator().map_err(route_error)?;
        let source = zed_source_key().map_err(route_error)?;
        Ok(CompleteDocumentTree::new(
            fingerprint.as_bytes(),
            vec![ObservedDocumentLeaf::new(fingerprint, source)],
            ZedTreeAuthority {
                snapshot: Mutex::new(Some(snapshot)),
                terminal_revalidate,
            },
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        source: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let mut snapshot = authority
            .snapshot
            .lock()
            .map_err(|_| zed_internal("Zed snapshot lock was poisoned"))?
            .take()
            .ok_or_else(|| zed_internal("Zed snapshot was consumed twice"))?;
        let snapshot_revision = snapshot.snapshot_revision.clone();
        let physical_locator = snapshot.physical_locator.clone();
        sink.begin_source(source.clone())?;
        let connection = snapshot.connection().map_err(route_error)?;
        let mut projection = ZedSourceBackedSinkV0::with_emitter(
            connection,
            source.clone(),
            zed_snapshot_revision_digest(&snapshot_revision),
            self.path.to_string_lossy().into_owned(),
            |document| {
                sink.emit_core_record(document)
                    .map_err(|error| CaptureError::InvalidPayload(error.to_string()).into())
            },
        )
        .map_err(route_error)?;
        let scan = scan_zed_native_snapshot(
            connection,
            &physical_locator,
            &snapshot_revision,
            &mut projection,
        )
        .map_err(route_error)?;
        if let Some(error) = projection.take_failure() {
            return Err(route_error(error));
        }
        let staged_documents = projection.staged_documents();
        drop(projection);
        snapshot.finish().map_err(route_error)?;
        if staged_documents != scan.counters.retained_events {
            return Err(zed_internal("Zed source-backed counts do not reconcile"));
        }
        let complete_records = scan
            .counters
            .retained_events
            .checked_add(scan.counters.rejected_threads)
            .ok_or_else(|| zed_internal("Zed source-backed counts overflowed"))?;
        let counts = ScannedSourceCounts {
            complete_records,
            retained_records: scan.counters.retained_events,
            rejected_records: scan.counters.rejected_threads,
            ignored_records: 0,
            indexed_documents: staged_documents,
            certified_bytes: scan.counters.certified_logical_bytes,
        };
        let observation =
            zed_source_observation(source, &snapshot_revision).map_err(route_error)?;
        let certificate = CertifiedSource::certify(
            observation.clone(),
            observation,
            ZED_REPLACEMENT_PARSER_REVISION,
            decode_zed_digest(&scan.source_integrity_digest).map_err(route_error)?,
            counts,
        )
        .map_err(route_error)?;
        Ok(zed_document_terminal(certificate))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        if let Some(mut snapshot) = tree
            .authority
            .snapshot
            .lock()
            .map_err(|_| zed_internal("Zed snapshot lock was poisoned"))?
            .take()
        {
            snapshot.finish().map_err(route_error)?;
        }
        (tree.authority.terminal_revalidate)().map_err(route_error)?;
        Ok(tree.tree_fingerprint)
    }
}

fn zed_document_terminal(certificate: CertifiedSource) -> DocumentSourceTerminal {
    DocumentSourceTerminal {
        source: certificate.observation().source().clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: ZED_REPLACEMENT_PARSER_REVISION,
        content_digest: *certificate.content_digest(),
        counts: certificate.counts(),
    }
}

fn zed_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
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
