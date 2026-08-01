//! Replacement-only lifecycle for bounded whole-document source trees.
//!
//! Providers retain discovery, parsing, projection, source observations, and
//! exact locator semantics. This family owns only cheap physical observation,
//! exact replay, replacement staging, complete-inventory deletion evidence,
//! and commit-time tree revalidation.

use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::{Arc, Mutex},
};

use crate::provider::source_backed::{
    executable_route, route_coordinator_error, source_backed_base_sources, ParallelLeafScanBegin,
    ParallelLeafScanCancelled, ParallelLeafScanComplete, ParallelLeafScanEmitter,
    ParallelLeafScanError, ParallelLeafScanJob, ParallelLeafScanWorkerError,
    SourceBackedCoordinatorResult, SourceBackedGenerationSink, SourceBackedProviderRegistry,
    SourceBackedRevalidationTarget, SourceBackedRouteDriver, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResult, SourceBackedRouteSelection,
    SourceBackedSelectorAuthority,
};
use crate::ProviderSource;
use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion, CertifiedSourceInventory,
    CoreRecord, ScannedSourceCounts, SourceFrontier, SourceKey, SourceObservation, TypedKey,
};
const DOCUMENT_FRONTIER_KIND: &str = "ctx-document-full-snapshot-v1";

mod inventory;
use inventory::DocumentInventoryAuthority;
mod sink;
pub(crate) use sink::ChangedDocumentSink;

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
    bound_replay_source: Option<SourceKey>,
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
            bound_replay_source: None,
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

struct CurrentDocumentSources {
    ordered: Vec<SourceKey>,
    canonical: HashMap<[u8; 32], usize>,
    exact_descriptors: HashMap<[u8; 32], ExactDescriptorEntry>,
    #[cfg(test)]
    operations: DocumentMembershipOperations,
}

struct ExactDescriptorEntry {
    first: usize,
    collisions: Vec<usize>,
}
impl CurrentDocumentSources {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            ordered: Vec::with_capacity(capacity),
            canonical: HashMap::with_capacity(capacity),
            exact_descriptors: HashMap::with_capacity(capacity),
            #[cfg(test)]
            operations: DocumentMembershipOperations::default(),
        }
    }
    fn contains_canonical(&mut self, source: &SourceKey) -> bool {
        self.canonical_source(source).is_some()
    }
    fn canonical_source(&mut self, source: &SourceKey) -> Option<&SourceKey> {
        #[cfg(test)]
        {
            self.operations.canonical_lookups += 1;
        }
        self.canonical
            .get(&source.identity().digest())
            .and_then(|index| self.ordered.get(*index))
    }
    fn insert(&mut self, source: SourceKey) -> bool {
        #[cfg(test)]
        {
            self.operations.source_insertions += 1;
        }
        let canonical_digest = source.identity().digest();
        let Entry::Vacant(canonical) = self.canonical.entry(canonical_digest) else {
            return false;
        };
        let descriptor_digest = source.exact_descriptor_digest();
        let index = self.ordered.len();
        canonical.insert(index);
        self.ordered.push(source);
        self.exact_descriptors
            .entry(descriptor_digest)
            .and_modify(|entry| entry.collisions.push(index))
            .or_insert_with(|| ExactDescriptorEntry {
                first: index,
                collisions: Vec::new(),
            });
        true
    }
    fn contains_exact(&mut self, source: &SourceKey) -> bool {
        #[cfg(test)]
        {
            self.operations.exact_lookups += 1;
        }
        let Some(entry) = self
            .exact_descriptors
            .get(&source.exact_descriptor_digest())
        else {
            return false;
        };
        let mut comparisons = 0;
        let matched = std::iter::once(&entry.first)
            .chain(&entry.collisions)
            .any(|index| {
                comparisons += 1;
                self.ordered
                    .get(*index)
                    .is_some_and(|candidate| candidate.exact_descriptor_eq(source))
            });
        #[cfg(test)]
        {
            self.operations.exact_comparisons += comparisons;
        }
        #[cfg(not(test))]
        let _ = comparisons;
        matched
    }
    fn ordered_inventory_sources(&self) -> Vec<SourceKey> {
        self.ordered.clone()
    }
    #[cfg(test)]
    fn reset_operations(&mut self) {
        self.operations = DocumentMembershipOperations::default();
    }
    #[cfg(test)]
    fn operations(&self) -> DocumentMembershipOperations {
        self.operations
    }
}
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DocumentMembershipOperations {
    source_insertions: usize,
    canonical_lookups: usize,
    exact_lookups: usize,
    exact_comparisons: usize,
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

/// Declares whether changed leaves may be scanned independently.
///
/// `Independent` is a strong adapter promise: exact source identity must be
/// derivable without reading content, and each `scan_changed` call must read
/// and certify only its supplied leaf without depending on scan order or
/// mutable state shared with another leaf. The family deliberately cannot
/// infer that promise from `Send + Sync`, so existing adapters remain serial.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum DocumentLeafExecutionPolicy {
    #[default]
    Serial,
    Independent,
    IndependentCapped(usize),
}
pub(crate) trait ReplacementDocumentTree: Send + Sync + 'static {
    type Leaf: Send + Sync + 'static;
    type TreeAuthority: Send + Sync + 'static;

    fn parser_revision(&self) -> &'static str;
    fn owns_source(&self, source: &SourceKey) -> bool;
    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        DocumentLeafExecutionPolicy::Serial
    }
    fn independent_leaf_source(
        &self,
        _authority: &Self::TreeAuthority,
        _leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<SourceKey> {
        Err(document_internal(
            "document adapter opted into independent leaves without deriving an exact source",
        ))
    }
    /// Derives the current exact descriptor before durable replay admission.
    ///
    /// `None` means the descriptor is not independently derivable: replay
    /// retains the existing parser-revision plus physical-fingerprint
    /// contract. `Some` additionally binds replay to the exact current source
    /// descriptor and forces a scan when that descriptor changed. The
    /// independent policy already promises cheap exact-source derivation, so
    /// it adds that binding without an additional adapter method.
    fn durable_replay_source(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<Option<SourceKey>> {
        match self.leaf_execution_policy() {
            DocumentLeafExecutionPolicy::Serial => Ok(None),
            DocumentLeafExecutionPolicy::Independent
            | DocumentLeafExecutionPolicy::IndependentCapped(_) => {
                self.independent_leaf_source(authority, leaf).map(Some)
            }
        }
    }
    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>>;
    fn discover_complete_with_base(
        &self,
        _base_sources: &[CertifiedSource],
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        self.discover_complete()
    }
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
    fn after_successful_publication(
        &self,
        _tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
        _certificates: &HashMap<[u8; 32], CertifiedSource>,
    ) {
    }
    fn has_successful_publication_work(&self) -> bool {
        false
    }
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
    let publication_adapter = Arc::clone(&adapter);
    let publication_state = Arc::clone(&state);
    let has_successful_publication_work = publication_adapter.has_successful_publication_work();

    let driver = SourceBackedRouteDriver::new(
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
    )
    .with_complete_inventory_revalidation(move |inventory| {
        revalidate_document_inventory(inventory_adapter.as_ref(), &inventory_state, inventory)
    });
    if !has_successful_publication_work {
        return driver;
    }
    driver.with_successful_publication(move || {
        let Ok(state) = publication_state.lock() else {
            return;
        };
        let Some(expected) = state.expected.as_ref() else {
            return;
        };
        publication_adapter.after_successful_publication(&expected.tree, &expected.certificates);
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
    let base_sources = source_backed_base_sources(sink, |source| adapter.owns_source(source));
    let mut tree = adapter.discover_complete_with_base(&base_sources)?;
    validate_unique_leaf_fingerprints(&tree.leaves)?;
    bind_durable_replay_sources(adapter, &mut tree)?;
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

    let (mut current_sources, certificates) = match adapter.leaf_execution_policy() {
        DocumentLeafExecutionPolicy::Serial => {
            scan_document_leaves_serial(adapter, &tree, replayable, sink)?
        }
        DocumentLeafExecutionPolicy::Independent => scan_document_leaves_independently(
            adapter,
            &tree,
            &base_sources,
            replayable,
            adapter.parser_revision(),
            sink.recommended_leaf_workers(tree.leaves.len()),
            sink,
        )?,
        DocumentLeafExecutionPolicy::IndependentCapped(worker_count) => {
            scan_document_leaves_independently(
                adapter,
                &tree,
                &base_sources,
                replayable,
                adapter.parser_revision(),
                worker_count.min(sink.recommended_leaf_workers(tree.leaves.len())),
                sink,
            )?
        }
    };

    let inventory = inventory_authority.certify(
        tree.tree_fingerprint,
        current_sources.ordered_inventory_sources(),
    )?;
    sink.certify_complete_inventory(inventory.clone())
        .map_err(route_coordinator_error)?;
    for base in &base_sources {
        if current_sources.contains_exact(base.observation().source()) {
            continue;
        }
        if let Some(replacement) = current_sources.canonical_source(base.observation().source()) {
            if base
                .observation()
                .source()
                .is_same_lineage_descriptor_replacement(replacement)
                && inventory.contains(replacement)
            {
                // `begin_source` has already staged the replacement under the
                // canonical source token. The writer atomically removes A's
                // documents and publishes B after exact-source and complete-
                // inventory terminal revalidation. This is not a deletion:
                // the authoritative inventory still contains the lineage.
                continue;
            }
            return Err(document_changed(
                "complete document tree produced an ambiguous source descriptor transition",
            ));
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

fn bind_durable_replay_sources<A>(
    adapter: &A,
    tree: &mut CompleteDocumentTree<A::Leaf, A::TreeAuthority>,
) -> SourceBackedRouteResult<()>
where
    A: ReplacementDocumentTree,
{
    for observed in &mut tree.leaves {
        if !observed.replay_from_frontier {
            continue;
        }
        let source = adapter.durable_replay_source(&tree.authority, &observed.provider_leaf)?;
        if source
            .as_ref()
            .is_some_and(|source| !adapter.owns_source(source))
        {
            return Err(document_changed(
                "document adapter derived a replay source outside its route ownership",
            ));
        }
        observed.bound_replay_source = source;
    }
    Ok(())
}

fn exact_replay_for_observed(
    observed: &ObservedDocumentLeaf<impl Sized>,
    replayable: &mut HashMap<DocumentLeafFingerprint, CertifiedSource>,
) -> Option<CertifiedSource> {
    let base = replayable.remove(&observed.fingerprint)?;
    match observed.bound_replay_source.as_ref() {
        Some(current) => base.observation().source().exact_descriptor_eq(current),
        None => true,
    }
    .then_some(base)
}

fn scan_document_leaves_serial<A>(
    adapter: &A,
    tree: &CompleteDocumentTree<A::Leaf, A::TreeAuthority>,
    mut replayable: HashMap<DocumentLeafFingerprint, CertifiedSource>,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<(CurrentDocumentSources, Vec<CertifiedSource>)>
where
    A: ReplacementDocumentTree,
{
    let mut current_sources = CurrentDocumentSources::with_capacity(tree.leaves.len());
    let mut certificates = Vec::with_capacity(tree.leaves.len());
    for observed in &tree.leaves {
        let replay = exact_replay_for_observed(observed, &mut replayable);
        let certificate = if let Some(base) = replay {
            stage_exact_document_replay(sink, &base)?;
            base
        } else {
            let mut changed = if observed.replay_from_frontier {
                ChangedDocumentSink::new(sink)
            } else {
                ChangedDocumentSink::logical(sink)?
            };
            let terminal =
                adapter.scan_changed(&tree.authority, &observed.provider_leaf, &mut changed)?;
            if terminal.parser_revision != adapter.parser_revision() {
                return Err(document_changed(
                    "document adapter terminal used an unexpected parser revision",
                ));
            }
            let source = changed.source()?.clone();
            if observed
                .bound_replay_source
                .as_ref()
                .is_some_and(|expected| !expected.exact_descriptor_eq(&source))
            {
                return Err(document_changed(
                    "document leaf scan derived a different exact replay source",
                ));
            }
            if current_sources.contains_canonical(&source) {
                return Err(document_changed(
                    "complete document tree produced a duplicate logical source",
                ));
            }
            changed.finish(
                terminal,
                observed
                    .replay_from_frontier
                    .then_some(observed.fingerprint),
            )?
        };
        let source = certificate.observation().source().clone();
        validate_current_document_source(adapter, &mut current_sources, source)?;
        certificates.push(certificate);
    }
    Ok((current_sources, certificates))
}

enum IndependentDocumentLeaf<'leaf, L> {
    Replay {
        base: Box<CertifiedSource>,
    },
    Changed {
        observed: &'leaf ObservedDocumentLeaf<L>,
        logical_base: Option<Box<CertifiedSource>>,
    },
}

fn scan_document_leaves_independently<A>(
    adapter: &A,
    tree: &CompleteDocumentTree<A::Leaf, A::TreeAuthority>,
    base_sources: &[CertifiedSource],
    mut replayable: HashMap<DocumentLeafFingerprint, CertifiedSource>,
    parser_revision: &'static str,
    worker_count: usize,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<(CurrentDocumentSources, Vec<CertifiedSource>)>
where
    A: ReplacementDocumentTree,
{
    let mut current_sources = CurrentDocumentSources::with_capacity(tree.leaves.len());
    let base_by_source = base_sources
        .iter()
        .filter(|source| adapter.owns_source(source.observation().source()))
        .map(|source| {
            (
                source.observation().source().identity().digest(),
                source.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut jobs = Vec::with_capacity(tree.leaves.len());
    for observed in &tree.leaves {
        let replay = exact_replay_for_observed(observed, &mut replayable);
        let (source, leaf) = if let Some(base) = replay {
            (
                base.observation().source().clone(),
                IndependentDocumentLeaf::Replay {
                    base: Box::new(base),
                },
            )
        } else {
            let source = match observed.bound_replay_source.as_ref() {
                Some(source) => source.clone(),
                None => {
                    adapter.independent_leaf_source(&tree.authority, &observed.provider_leaf)?
                }
            };
            let logical_base = (!observed.replay_from_frontier)
                .then(|| base_by_source.get(&source.identity().digest()).cloned())
                .flatten()
                .filter(|base| base.observation().source().exact_descriptor_eq(&source))
                .map(Box::new);
            (
                source,
                IndependentDocumentLeaf::Changed {
                    observed,
                    logical_base,
                },
            )
        };
        validate_current_document_source(adapter, &mut current_sources, source.clone())?;
        jobs.push(ParallelLeafScanJob::new(source, leaf));
    }

    // Jobs and returned certificates retain discovery order. The runner may
    // interleave bounded staging messages, but the writer canonicalizes source
    // publication and the family certifies one ordered complete inventory.
    let certificates = sink
        .run_parallel_leaf_scans(jobs, worker_count, |job, emitter| match job.leaf() {
            IndependentDocumentLeaf::Replay { base } => {
                let append = exact_document_replay_append(base)
                    .map_err(ParallelLeafScanWorkerError::provider)?;
                emitter.begin(ParallelLeafScanBegin::append(
                    job.source().clone(),
                    base.as_ref().clone(),
                ))?;
                emitter.complete(ParallelLeafScanComplete::append(
                    append,
                    base.as_ref().clone(),
                ))?;
                Ok(())
            }
            IndependentDocumentLeaf::Changed {
                observed,
                logical_base,
            } => scan_independent_document_leaf(
                adapter,
                &tree.authority,
                observed,
                parser_revision,
                job.source(),
                logical_base.as_deref(),
                emitter,
            ),
        })
        .map_err(document_parallel_error)?;
    Ok((current_sources, certificates))
}

fn scan_independent_document_leaf<A>(
    adapter: &A,
    authority: &A::TreeAuthority,
    observed: &ObservedDocumentLeaf<A::Leaf>,
    parser_revision: &'static str,
    expected_source: &SourceKey,
    logical_base: Option<&CertifiedSource>,
    emitter: &mut ParallelLeafScanEmitter<'_, CertifiedSource, SourceBackedRouteError>,
) -> Result<(), ParallelLeafScanWorkerError<SourceBackedRouteError>>
where
    A: ReplacementDocumentTree,
{
    let scan_result = {
        let mut changed = if observed.replay_from_frontier {
            ChangedDocumentSink::parallel(emitter)
        } else {
            ChangedDocumentSink::parallel_logical(emitter, logical_base.cloned())
                .map_err(ParallelLeafScanWorkerError::provider)?
        };
        (|| {
            let terminal =
                adapter.scan_changed(authority, &observed.provider_leaf, &mut changed)?;
            if terminal.parser_revision != parser_revision {
                return Err(document_changed(
                    "document adapter terminal used an unexpected parser revision",
                ));
            }
            if !changed.source()?.exact_descriptor_eq(expected_source) {
                return Err(document_changed(
                    "independent document leaf derived a different exact source",
                ));
            }
            changed.finish(
                terminal,
                observed
                    .replay_from_frontier
                    .then_some(observed.fingerprint),
            )
        })()
    };
    let certificate = match scan_result {
        Ok(certificate) => certificate,
        Err(_) if emitter.is_cancelled() => {
            return Err(ParallelLeafScanCancelled.into());
        }
        Err(error) => return Err(ParallelLeafScanWorkerError::provider(error)),
    };
    let _ = certificate;
    Ok(())
}

fn validate_current_document_source<A>(
    adapter: &A,
    current_sources: &mut CurrentDocumentSources,
    source: SourceKey,
) -> SourceBackedRouteResult<()>
where
    A: ReplacementDocumentTree,
{
    if !adapter.owns_source(&source) {
        return Err(document_changed(
            "document adapter emitted a source outside its route ownership",
        ));
    }
    if !current_sources.insert(source) {
        return Err(document_changed(
            "complete document tree produced a duplicate logical source",
        ));
    }
    Ok(())
}

fn document_parallel_error(
    error: ParallelLeafScanError<SourceBackedRouteError>,
) -> SourceBackedRouteError {
    match error {
        ParallelLeafScanError::Worker { source, .. } => source,
        ParallelLeafScanError::Sink { source, .. } => route_coordinator_error(source),
        error => document_internal(format!("independent document leaf runner failed: {error}")),
    }
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
    sink.begin_source_append(base.observation().source().clone())
        .map_err(route_coordinator_error)?;
    let append = exact_document_replay_append(base)?;
    sink.certify_source_append(append)
        .map_err(route_coordinator_error)
}

fn exact_document_replay_append(
    base: &CertifiedSource,
) -> SourceBackedRouteResult<CertifiedSourceAppend> {
    let frontier = base
        .frontier()
        .ok_or_else(|| document_internal("replayable document source has no frontier"))?;
    let append = CertifiedSourceAppend::certify(
        base,
        base.clone(),
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .map_err(document_contract_error)?;
    Ok(append)
}

pub(crate) fn document_frontier_fingerprint(
    certificate: &CertifiedSource,
) -> Option<DocumentLeafFingerprint> {
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
        && revalidate_durable_replay_sources(adapter, &expected.tree)
}

fn revalidate_durable_replay_sources<A>(
    adapter: &A,
    tree: &CompleteDocumentTree<A::Leaf, A::TreeAuthority>,
) -> bool
where
    A: ReplacementDocumentTree,
{
    tree.leaves.iter().all(|observed| {
        if !observed.replay_from_frontier {
            return true;
        }
        adapter
            .durable_replay_source(&tree.authority, &observed.provider_leaf)
            .is_ok_and(|current| match (&observed.bound_replay_source, current) {
                (Some(expected), Some(current)) => expected.exact_descriptor_eq(&current),
                (None, None) => true,
                _ => false,
            })
    })
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
