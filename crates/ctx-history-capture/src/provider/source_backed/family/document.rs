//! Replacement-only lifecycle for bounded whole-document source trees.
//!
//! Providers retain discovery, parsing, projection, source observations, and
//! exact locator semantics. This family owns only cheap physical observation,
//! exact replay, replacement staging, complete-inventory deletion evidence,
//! and commit-time tree revalidation.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, CertifiedSource, CertifiedSourceAppend,
    CertifiedSourceDeletion, CertifiedSourceInventory, EventHydrationRequest,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, ScannedSourceCounts,
    SourceFrontier, SourceInventoryObservation, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};

use crate::provider::source_backed::{
    executable_route, hydration_failure, route_coordinator_error, source_backed_base_sources,
    SourceBackedCoordinatorResult, SourceBackedGenerationSink, SourceBackedProviderRegistry,
    SourceBackedRevalidationTarget, SourceBackedRouteDriver, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResult, SourceBackedRouteSelection,
    SourceBackedSelectorAuthority,
};
use crate::ProviderSource;

const DOCUMENT_FRONTIER_KIND: &str = "ctx-document-full-snapshot-v1";
const DOCUMENT_INVENTORY_AUTHORITY_NAMESPACE: &str = "ctx.document-tree";
const DOCUMENT_INVENTORY_REVISION_KIND: &str = "ctx-document-tree-fingerprint-v1";
const DOCUMENT_INVENTORY_DISCOVERY_REVISION: &str = "ctx-document-tree-discovery-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct DocumentLeafFingerprint([u8; 32]);

impl DocumentLeafFingerprint {
    pub(crate) fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub(crate) fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug)]
pub(crate) struct ObservedDocumentLeaf<L> {
    pub(crate) fingerprint: DocumentLeafFingerprint,
    replay_from_frontier: bool,
    pub(crate) provider_leaf: L,
}

impl<L> ObservedDocumentLeaf<L> {
    pub(crate) fn new(fingerprint: DocumentLeafFingerprint, provider_leaf: L) -> Self {
        Self::with_durable_replay(fingerprint, provider_leaf, true)
    }

    /// Selects whether the physical fingerprint is durable replay identity.
    ///
    /// Ordinary files use `true`. Logical snapshots such as SQLite use
    /// `false`: they scan once, then identical logical staging is discarded
    /// without publishing a new generation.
    pub(crate) fn with_durable_replay(
        physical_fingerprint: DocumentLeafFingerprint,
        provider_leaf: L,
        replay_from_frontier: bool,
    ) -> Self {
        Self {
            fingerprint: physical_fingerprint,
            replay_from_frontier,
            provider_leaf,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CompleteDocumentTree<L, A> {
    pub(crate) tree_fingerprint: [u8; 32],
    pub(crate) leaves: Vec<ObservedDocumentLeaf<L>>,
    pub(crate) authority: A,
}

impl<L, A> CompleteDocumentTree<L, A> {
    pub(crate) fn new(
        tree_fingerprint: [u8; 32],
        leaves: Vec<ObservedDocumentLeaf<L>>,
        authority: A,
    ) -> Self {
        Self {
            tree_fingerprint,
            leaves,
            authority,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DocumentSourceTerminal {
    pub(crate) source: SourceKey,
    pub(crate) opening: SourceObservation,
    pub(crate) closing: SourceObservation,
    pub(crate) parser_revision: &'static str,
    pub(crate) content_digest: [u8; 32],
    pub(crate) counts: ScannedSourceCounts,
}

impl DocumentSourceTerminal {
    fn certify(
        self,
        replay_fingerprint: Option<DocumentLeafFingerprint>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let frontier = replay_fingerprint
            .map(|fingerprint| {
                SourceFrontier::new(
                    DOCUMENT_FRONTIER_KIND,
                    TypedKey::bytes(fingerprint.as_bytes().to_vec())
                        .map_err(document_contract_error)?,
                    self.counts.certified_bytes,
                    self.content_digest,
                )
                .map_err(document_contract_error)
            })
            .transpose()?;
        CertifiedSource::certify_with_frontier(
            self.opening,
            self.closing,
            self.parser_revision,
            self.content_digest,
            self.counts,
            frontier,
        )
        .map_err(document_contract_error)
    }
}

/// The only write surface available while one changed document is projected.
///
/// There is intentionally no append entry point and no retained page/document
/// collection. Every emitted document is forwarded immediately to the active
/// generation replacement.
pub(crate) struct ChangedDocumentSink<'sink, 'writer> {
    sink: &'sink mut SourceBackedGenerationSink<'writer>,
    source: Option<SourceKey>,
    emitted_documents: u64,
}

impl<'sink, 'writer> ChangedDocumentSink<'sink, 'writer> {
    fn new(sink: &'sink mut SourceBackedGenerationSink<'writer>) -> Self {
        Self {
            sink,
            source: None,
            emitted_documents: 0,
        }
    }

    pub(crate) fn begin_source(&mut self, source: SourceKey) -> SourceBackedRouteResult<()> {
        if self.source.is_some() {
            return Err(document_internal(
                "document adapter began more than one source for one observed leaf",
            ));
        }
        self.sink
            .begin_source(source.clone())
            .map_err(route_coordinator_error)?;
        self.source = Some(source);
        Ok(())
    }

    pub(crate) fn emit_document(
        &mut self,
        document: LexicalDocument,
    ) -> SourceBackedRouteResult<()> {
        let source = self.source.as_ref().ok_or_else(|| {
            document_internal("document adapter emitted before beginning its source")
        })?;
        if !document.source.exact_descriptor_eq(source)
            || !document.locator.source().exact_descriptor_eq(source)
        {
            return Err(document_changed(
                "document adapter emitted a row outside its active exact source",
            ));
        }
        self.sink
            .add_document(document)
            .map_err(route_coordinator_error)?;
        self.emitted_documents = self
            .emitted_documents
            .checked_add(1)
            .ok_or_else(|| document_internal("document emission count overflowed"))?;
        Ok(())
    }

    fn source(&self) -> SourceBackedRouteResult<&SourceKey> {
        self.source
            .as_ref()
            .ok_or_else(|| document_internal("document adapter did not begin its source"))
    }

    fn certify(
        self,
        terminal: DocumentSourceTerminal,
        replay_fingerprint: Option<DocumentLeafFingerprint>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let source = self.source()?;
        if !terminal.source.exact_descriptor_eq(source)
            || !terminal.opening.source().exact_descriptor_eq(source)
            || !terminal.closing.source().exact_descriptor_eq(source)
        {
            return Err(document_changed(
                "document terminal changed its active exact source descriptor",
            ));
        }
        if terminal.counts.indexed_documents != self.emitted_documents {
            return Err(document_changed(
                "document terminal indexed count did not match forwarded documents",
            ));
        }
        let certificate = terminal.certify(replay_fingerprint)?;
        self.sink
            .certify_source(certificate.clone())
            .map_err(route_coordinator_error)?;
        Ok(certificate)
    }
}

pub(crate) trait ReplacementDocumentTree: Send + Sync + 'static {
    type Leaf: Send + Sync + 'static;
    type TreeAuthority: Send + Sync + 'static;

    fn parser_revision(&self) -> &'static str;

    fn owns_source(&self, source: &SourceKey) -> bool;

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>>;

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal>;

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]>;

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure>;
}

pub(crate) fn register_replacement_document_tree_route<A>(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    adapter: A,
) -> SourceBackedCoordinatorResult<()>
where
    A: ReplacementDocumentTree,
{
    register_replacement_document_tree_route_with_authority(
        registry,
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        adapter,
    )
}

pub(crate) fn register_replacement_document_tree_route_with_authority<A>(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
    adapter: A,
) -> SourceBackedCoordinatorResult<()>
where
    A: ReplacementDocumentTree,
{
    let driver = replacement_document_tree_driver(&source, adapter);
    registry.register(executable_route(
        source,
        selection,
        selector_authority,
        driver,
    )?);
    Ok(())
}

fn replacement_document_tree_driver<A>(
    route: &ProviderSource,
    adapter: A,
) -> SourceBackedRouteDriver
where
    A: ReplacementDocumentTree,
{
    let adapter = Arc::new(adapter);
    let state = Arc::new(Mutex::new(
        DocumentCommitState::<A::Leaf, A::TreeAuthority>::default(),
    ));
    let inventory_authority = DocumentInventoryAuthority::new(route);

    let scan_adapter = Arc::clone(&adapter);
    let scan_state = Arc::clone(&state);
    let scan_authority = inventory_authority.clone();
    let owns_adapter = Arc::clone(&adapter);
    let source_state = Arc::clone(&state);
    let inventory_adapter = Arc::clone(&adapter);
    let inventory_state = Arc::clone(&state);
    let single_adapter = Arc::clone(&adapter);
    let batch_adapter = adapter;

    SourceBackedRouteDriver::new(
        move |sink| {
            {
                let mut state = scan_state
                    .lock()
                    .map_err(|_| document_internal("document commit state lock was poisoned"))?;
                *state = DocumentCommitState::default();
            }
            let expected = scan_document_tree(scan_adapter.as_ref(), &scan_authority, sink)?;
            let mut state = scan_state
                .lock()
                .map_err(|_| document_internal("document commit state lock was poisoned"))?;
            state.expected = Some(expected);
            Ok(())
        },
        move |source| owns_adapter.owns_source(source),
        move |target| revalidate_document_target(&source_state, target),
        move |request| hydrate_one_document_group(single_adapter.as_ref(), request),
    )
    .with_complete_inventory_revalidation(move |inventory| {
        revalidate_document_inventory(inventory_adapter.as_ref(), &inventory_state, inventory)
    })
    .with_batch_hydration(move |request| {
        let result = batch_adapter.hydrate_group(request)?;
        result.validate_for_request(request)?;
        Ok(result)
    })
}

fn scan_document_tree<A>(
    adapter: &A,
    inventory_authority: &DocumentInventoryAuthority,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<ExpectedDocumentRoute<A::Leaf, A::TreeAuthority>>
where
    A: ReplacementDocumentTree,
{
    let tree = adapter.discover_complete()?;
    validate_unique_leaf_fingerprints(&tree.leaves)?;
    let base_sources = source_backed_base_sources(sink, |source| adapter.owns_source(source));
    let mut replayable = HashMap::new();
    for base in &base_sources {
        if base.parser_revision() != adapter.parser_revision() {
            continue;
        }
        let Some(fingerprint) = document_frontier_fingerprint(base) else {
            continue;
        };
        if replayable.insert(fingerprint, base.clone()).is_some() {
            return Err(document_internal(
                "base generation contains a duplicate document leaf fingerprint",
            ));
        }
    }

    let mut current_sources = HashMap::<[u8; 32], SourceKey>::new();
    let mut certificates = Vec::with_capacity(tree.leaves.len());
    for observed in &tree.leaves {
        let replay = observed
            .replay_from_frontier
            .then(|| replayable.remove(&observed.fingerprint))
            .flatten();
        let certificate = if let Some(base) = replay {
            stage_exact_document_replay(sink, &base)?;
            base
        } else {
            let mut changed = ChangedDocumentSink::new(sink);
            let terminal =
                adapter.scan_changed(&tree.authority, &observed.provider_leaf, &mut changed)?;
            if terminal.parser_revision != adapter.parser_revision() {
                return Err(document_changed(
                    "document adapter terminal used an unexpected parser revision",
                ));
            }
            let source = changed.source()?.clone();
            if current_sources.contains_key(&source.identity().digest()) {
                return Err(document_changed(
                    "complete document tree produced a duplicate logical source",
                ));
            }
            changed.certify(
                terminal,
                observed
                    .replay_from_frontier
                    .then_some(observed.fingerprint),
            )?
        };
        let source = certificate.observation().source().clone();
        if !adapter.owns_source(&source) {
            return Err(document_changed(
                "document adapter emitted a source outside its route ownership",
            ));
        }
        if current_sources
            .insert(source.identity().digest(), source)
            .is_some()
        {
            return Err(document_changed(
                "complete document tree produced a duplicate logical source",
            ));
        }
        certificates.push(certificate);
    }

    let inventory = inventory_authority.certify(
        tree.tree_fingerprint,
        current_sources.values().cloned().collect(),
    )?;
    sink.certify_complete_inventory(inventory.clone())
        .map_err(route_coordinator_error)?;
    for base in &base_sources {
        if current_sources
            .values()
            .any(|source| source.exact_descriptor_eq(base.observation().source()))
        {
            continue;
        }
        let deletion = CertifiedSourceDeletion::from_inventory(
            base.observation().source().clone(),
            &inventory,
        )
        .map_err(document_contract_error)?;
        sink.delete_source(deletion, inventory.clone())
            .map_err(route_coordinator_error)?;
    }

    Ok(ExpectedDocumentRoute::new(tree, certificates, inventory))
}

fn validate_unique_leaf_fingerprints<L>(
    leaves: &[ObservedDocumentLeaf<L>],
) -> SourceBackedRouteResult<()> {
    let mut fingerprints = HashSet::with_capacity(leaves.len());
    if leaves
        .iter()
        .all(|leaf| fingerprints.insert(leaf.fingerprint))
    {
        Ok(())
    } else {
        Err(document_changed(
            "complete document tree contains a duplicate physical leaf",
        ))
    }
}

fn stage_exact_document_replay(
    sink: &mut SourceBackedGenerationSink<'_>,
    base: &CertifiedSource,
) -> SourceBackedRouteResult<()> {
    let frontier = base
        .frontier()
        .ok_or_else(|| document_internal("replayable document source has no frontier"))?;
    sink.begin_source_append(base.observation().source().clone())
        .map_err(route_coordinator_error)?;
    let append = CertifiedSourceAppend::certify(
        base,
        base.clone(),
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .map_err(document_contract_error)?;
    sink.certify_source_append(append)
        .map_err(route_coordinator_error)
}

fn document_frontier_fingerprint(certificate: &CertifiedSource) -> Option<DocumentLeafFingerprint> {
    let frontier = certificate.frontier()?;
    if frontier.checkpoint_kind() != DOCUMENT_FRONTIER_KIND {
        return None;
    }
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return None;
    };
    let fingerprint = <[u8; 32]>::try_from(bytes.as_slice()).ok()?;
    Some(DocumentLeafFingerprint::new(fingerprint))
}

fn hydrate_one_document_group<A>(
    adapter: &A,
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure>
where
    A: ReplacementDocumentTree,
{
    let batch = BatchHydrationRequest::new(vec![request.clone()]).map_err(|error| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            format!("invalid one-record document hydration group: {error}"),
        )
    })?;
    let result = adapter.hydrate_group(&batch)?;
    result.validate_for_request(&batch)?;
    result.into_records().into_iter().next().ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "document hydration returned no record",
        )
    })
}

struct DocumentCommitState<L, A> {
    expected: Option<ExpectedDocumentRoute<L, A>>,
}

impl<L, A> Default for DocumentCommitState<L, A> {
    fn default() -> Self {
        Self { expected: None }
    }
}

struct ExpectedDocumentRoute<L, A> {
    tree: CompleteDocumentTree<L, A>,
    certificates: HashMap<[u8; 32], CertifiedSource>,
    inventory: CertifiedSourceInventory,
}

impl<L, A> ExpectedDocumentRoute<L, A> {
    fn new(
        tree: CompleteDocumentTree<L, A>,
        certificates: Vec<CertifiedSource>,
        inventory: CertifiedSourceInventory,
    ) -> Self {
        Self {
            tree,
            certificates: certificates
                .into_iter()
                .map(|certificate| {
                    (
                        certificate.observation().source().identity().digest(),
                        certificate,
                    )
                })
                .collect(),
            inventory,
        }
    }
}

fn revalidate_document_target<L, A>(
    state: &Mutex<DocumentCommitState<L, A>>,
    target: SourceBackedRevalidationTarget<'_>,
) -> bool {
    let Ok(state) = state.lock() else {
        return false;
    };
    state
        .expected
        .as_ref()
        .is_some_and(|expected| match target {
            SourceBackedRevalidationTarget::Source(source) => {
                expected
                    .certificates
                    .get(&source.observation().source().identity().digest())
                    == Some(source)
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                deletion.verifies(&expected.inventory)
                    && !expected
                        .certificates
                        .contains_key(&deletion.source().identity().digest())
            }
        })
}

fn revalidate_document_inventory<A>(
    adapter: &A,
    state: &Mutex<DocumentCommitState<A::Leaf, A::TreeAuthority>>,
    inventory: &CertifiedSourceInventory,
) -> bool
where
    A: ReplacementDocumentTree,
{
    let Ok(state) = state.lock() else {
        return false;
    };
    let Some(expected) = state.expected.as_ref() else {
        return false;
    };
    if expected.inventory != *inventory {
        return false;
    }

    // Source and deletion callbacks only bind writer targets to this expected
    // route. The final inventory callback owns the one live terminal tree
    // observation, so changes between callbacks cannot inherit an earlier
    // successful result.
    adapter
        .revalidate_complete(&expected.tree)
        .is_ok_and(|terminal| terminal == expected.tree.tree_fingerprint)
}

#[derive(Clone)]
struct DocumentInventoryAuthority {
    provider: String,
    route_key: [u8; 32],
}

impl DocumentInventoryAuthority {
    fn new(route: &ProviderSource) -> Self {
        let path = route.path.as_os_str().as_encoded_bytes();
        let mut digest = Sha256::new();
        digest.update(b"ctx.document-tree-route-authority-v1\0");
        digest.update((route.provider.as_str().len() as u64).to_be_bytes());
        digest.update(route.provider.as_str().as_bytes());
        digest.update((route.source_format.len() as u64).to_be_bytes());
        digest.update(route.source_format.as_bytes());
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        Self {
            provider: route.provider.as_str().to_owned(),
            route_key: digest.finalize().into(),
        }
    }

    fn certify(
        &self,
        tree_fingerprint: [u8; 32],
        sources: Vec<SourceKey>,
    ) -> SourceBackedRouteResult<CertifiedSourceInventory> {
        let observation = SourceInventoryObservation::new(
            self.provider.clone(),
            DOCUMENT_INVENTORY_AUTHORITY_NAMESPACE,
            TypedKey::bytes(self.route_key.to_vec()).map_err(document_contract_error)?,
            DOCUMENT_INVENTORY_REVISION_KIND,
            tree_fingerprint.to_vec(),
        )
        .map_err(document_contract_error)?;
        CertifiedSourceInventory::certify(
            observation.clone(),
            observation,
            DOCUMENT_INVENTORY_DISCOVERY_REVISION,
            sources,
        )
        .map_err(document_contract_error)
    }
}

fn document_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn document_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

fn document_contract_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

#[cfg(test)]
#[path = "document/tests.rs"]
mod tests;
