//! Provider-neutral replacement lifecycle for bounded whole-document source trees.
//!
//! Providers retain discovery, parsing, projection, source observations, and
//! exact locator semantics. This family owns only cheap physical observation,
//! exact replay, replacement staging, complete-inventory deletion evidence,
//! and commit-time tree revalidation.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use super::*;
use crate::CaptureLifecycleSink;
use ctx_history_core::{
    CertifiedSource, CertifiedSourceAppend, CertifiedSourceDeletion, ScannedSourceCounts,
    SourceFrontier, SourceKey, SourceObservation, TypedKey,
};
const DOCUMENT_FRONTIER_KIND: &str = "ctx-document-full-snapshot-v1";
const MAX_PARALLEL_DOCUMENT_LEAF_WORKERS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentFullSnapshotCheckpoint {
    physical_fingerprint: DocumentLeafFingerprint,
    logical_fingerprint: [u8; 32],
}

impl DocumentFullSnapshotCheckpoint {
    pub fn physical_fingerprint(self) -> [u8; 32] {
        self.physical_fingerprint.as_bytes()
    }

    pub fn logical_fingerprint(self) -> [u8; 32] {
        self.logical_fingerprint
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
pub enum DocumentFullSnapshotCheckpointError {
    #[error("current certificate has no document frontier")]
    MissingFrontier,
    #[error("current certificate has an unexpected document frontier kind")]
    UnexpectedFrontierKind,
    #[error("document frontier checkpoint is not bytes")]
    NonByteCheckpoint,
    #[error("document frontier checkpoint is not a SHA-256 digest")]
    InvalidFingerprint,
}

/// Decodes the persisted full-document snapshot frontier owned by this runtime.
///
/// Provider adapters may combine the returned logical fingerprint with their
/// own provider-specific authority, but must not reinterpret the checkpoint
/// kind, typed-key representation, or physical digest width themselves.
pub fn decode_document_full_snapshot_checkpoint(
    certificate: &CertifiedSource,
) -> Result<DocumentFullSnapshotCheckpoint, DocumentFullSnapshotCheckpointError> {
    let frontier = certificate
        .frontier()
        .ok_or(DocumentFullSnapshotCheckpointError::MissingFrontier)?;
    if frontier.checkpoint_kind() != DOCUMENT_FRONTIER_KIND {
        return Err(DocumentFullSnapshotCheckpointError::UnexpectedFrontierKind);
    }
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return Err(DocumentFullSnapshotCheckpointError::NonByteCheckpoint);
    };
    let physical_fingerprint = <[u8; 32]>::try_from(bytes.as_slice())
        .map(DocumentLeafFingerprint::new)
        .map_err(|_| DocumentFullSnapshotCheckpointError::InvalidFingerprint)?;
    Ok(DocumentFullSnapshotCheckpoint {
        physical_fingerprint,
        logical_fingerprint: *certificate.content_digest(),
    })
}

pub fn document_full_snapshot_frontier(
    physical_fingerprint: DocumentLeafFingerprint,
    certified_bytes: u64,
    logical_fingerprint: [u8; 32],
) -> Result<SourceFrontier, ctx_history_core::ProjectionContractError> {
    SourceFrontier::new(
        DOCUMENT_FRONTIER_KIND,
        TypedKey::bytes(physical_fingerprint.as_bytes().to_vec())?,
        certified_bytes,
        logical_fingerprint,
    )
}

pub struct DocumentBaseRoute<'scan, 'writer, L: CaptureLifecycleSink> {
    sink: &'scan mut SourceBackedGenerationSink<'writer, L>,
    owns_source: &'scan dyn Fn(&SourceKey) -> bool,
}

impl<'scan, 'writer, L: CaptureLifecycleSink> DocumentBaseRoute<'scan, 'writer, L> {
    fn new(
        sink: &'scan mut SourceBackedGenerationSink<'writer, L>,
        owns_source: &'scan dyn Fn(&SourceKey) -> bool,
    ) -> Self {
        Self { sink, owns_source }
    }

    pub fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand {
        self.sink.reconciliation_demand()
    }

    pub fn route_control(&self) -> Option<&[u8]> {
        self.sink.base_route_control()
    }

    pub fn report_progress(
        &mut self,
        progress: SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()> {
        self.sink.report_current_source_progress(progress)
    }

    pub fn exact_source(&self, source: &SourceKey) -> Option<DocumentAppendBase<L>> {
        if !(self.owns_source)(source) {
            return None;
        }
        self.sink
            .pinned_append_base(source)
            .map(DocumentAppendBase::Generation)
    }
}

pub enum DocumentAppendBase<L: CaptureLifecycleSink> {
    Generation(L::PinnedAppendBase),
    Certificate(Box<CertifiedSource>),
}

impl<L> std::fmt::Debug for DocumentAppendBase<L>
where
    L: CaptureLifecycleSink,
    L::PinnedAppendBase: std::fmt::Debug,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Generation(base) => formatter.debug_tuple("Generation").field(base).finish(),
            Self::Certificate(base) => formatter.debug_tuple("Certificate").field(base).finish(),
        }
    }
}

impl<L> Clone for DocumentAppendBase<L>
where
    L: CaptureLifecycleSink,
    L::PinnedAppendBase: Clone,
{
    fn clone(&self) -> Self {
        match self {
            Self::Generation(base) => Self::Generation(base.clone()),
            Self::Certificate(base) => Self::Certificate(Box::new((**base).clone())),
        }
    }
}

impl<L: CaptureLifecycleSink> DocumentAppendBase<L> {
    pub fn certificate(&self) -> &CertifiedSource {
        match self {
            Self::Generation(base) => L::pinned_append_base_source(base),
            Self::Certificate(base) => base,
        }
    }
}

#[derive(Debug, Clone)]
struct DocumentLeafCompletion {
    certificate: CertifiedSource,
    record_rejections: SourceBackedRecordRejectionDrafts,
}

impl DocumentLeafCompletion {
    fn replay(certificate: CertifiedSource) -> Self {
        Self {
            certificate,
            record_rejections: Default::default(),
        }
    }
}

mod inventory;
pub use inventory::DocumentInventoryAuthority;
mod independent;
use independent::scan_document_leaves_independently;
mod finite_inventory;
pub use finite_inventory::{
    FiniteInventoryCatalog, FiniteInventoryCatalogLeaf, FiniteInventoryTreeAuthority,
};
mod revalidation;
use revalidation::{
    revalidate_document_inventory, revalidate_document_target, revalidate_durable_replay_sources,
    CurrentDocumentSources, DocumentCommitState, ExpectedDocumentRoute,
};
mod sink;
pub use sink::{ChangedDocumentSink, DocumentRecordSpool};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentLeafFingerprint([u8; 32]);
impl DocumentLeafFingerprint {
    pub fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug)]
pub struct ObservedDocumentLeaf<L> {
    pub fingerprint: DocumentLeafFingerprint,
    replay_from_frontier: bool,
    bound_replay_source: Option<SourceKey>,
    pub provider_leaf: L,
}

impl<L> ObservedDocumentLeaf<L> {
    pub fn new(fingerprint: DocumentLeafFingerprint, provider_leaf: L) -> Self {
        Self::with_durable_replay(fingerprint, provider_leaf, true)
    }

    /// Selects whether the physical fingerprint is durable replay identity.
    ///
    /// Ordinary files and sources with a bounded, terminally revalidated
    /// physical revision use `true`. Sources without such an authority use
    /// `false` and must rescan before an identical staging result is discarded.
    pub fn with_durable_replay(
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
pub struct CompleteDocumentTree<L, A> {
    pub tree_fingerprint: [u8; 32],
    pub leaves: Vec<ObservedDocumentLeaf<L>>,
    pub authority: A,
    inventory_scope: DocumentInventoryScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentInventoryScope {
    Complete,
    Partial,
}
impl<L, A> CompleteDocumentTree<L, A> {
    pub fn new(
        tree_fingerprint: [u8; 32],
        leaves: Vec<ObservedDocumentLeaf<L>>,
        authority: A,
    ) -> Self {
        Self {
            tree_fingerprint,
            leaves,
            authority,
            inventory_scope: DocumentInventoryScope::Complete,
        }
    }

    pub fn new_partial(
        tree_fingerprint: [u8; 32],
        leaves: Vec<ObservedDocumentLeaf<L>>,
        authority: A,
    ) -> Self {
        Self {
            tree_fingerprint,
            leaves,
            authority,
            inventory_scope: DocumentInventoryScope::Partial,
        }
    }
}

#[derive(Debug)]
pub struct DocumentSourceTerminal {
    pub source: SourceKey,
    pub opening: SourceObservation,
    pub closing: SourceObservation,
    pub parser_revision: &'static str,
    pub content_digest: [u8; 32],
    pub counts: ScannedSourceCounts,
}

impl DocumentSourceTerminal {
    fn certify(
        &self,
        replay_fingerprint: Option<DocumentLeafFingerprint>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let frontier = replay_fingerprint
            .map(|fingerprint| {
                document_full_snapshot_frontier(
                    fingerprint,
                    self.counts.certified_bytes,
                    self.content_digest,
                )
                .map_err(|error| document_changed(error.to_string()))
            })
            .transpose()?;
        CertifiedSource::certify_with_frontier(
            self.opening.clone(),
            self.closing.clone(),
            self.parser_revision,
            self.content_digest,
            self.counts,
            frontier,
        )
        .map_err(|error| document_changed(error.to_string()))
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
pub enum DocumentLeafExecutionPolicy {
    #[default]
    Serial,
    Independent,
    #[doc(hidden)]
    IndependentCapped(usize),
}
pub trait ReplacementDocumentTree: Send + Sync + 'static {
    type Lifecycle: CaptureLifecycleSink;
    type Spool: DocumentRecordSpool;
    type RouteControl: Send + Sync + 'static;
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
            DocumentLeafExecutionPolicy::Independent => {
                self.independent_leaf_source(authority, leaf).map(Some)
            }
            DocumentLeafExecutionPolicy::IndependentCapped(_) => {
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
    fn discover_complete_with_progress(
        &self,
        base_sources: &[CertifiedSource],
        _report_progress: &mut dyn FnMut(
            SourceBackedCurrentSourceProgress,
        ) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        self.discover_complete_with_base(base_sources)
    }
    fn discover_complete_with_reconciliation(
        &self,
        base_sources: &[CertifiedSource],
        _base_route_control: Option<&[u8]>,
        _reconciliation_demand: SourceBackedReconciliationDemand,
        report_progress: &mut dyn FnMut(
            SourceBackedCurrentSourceProgress,
        ) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        self.discover_complete_with_progress(base_sources, report_progress)
    }
    fn uses_lazy_base_route(&self, _demand: SourceBackedReconciliationDemand) -> bool {
        false
    }
    fn discover_complete_with_lazy_base(
        &self,
        _base_route: &mut DocumentBaseRoute<'_, '_, Self::Lifecycle>,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        Err(document_internal(
            "document adapter selected lazy base-route discovery without implementing it",
        ))
    }
    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_, Self::Lifecycle, Self::Spool>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal>;
    fn append_base(
        &self,
        _authority: &Self::TreeAuthority,
        _leaf: &Self::Leaf,
    ) -> Option<DocumentAppendBase<Self::Lifecycle>> {
        None
    }
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
    fn publication_control(
        &self,
        _tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<Option<Vec<u8>>> {
        Ok(None)
    }
    fn route_control_expectation(&self) -> Option<Self::RouteControl> {
        None
    }
}
pub fn replacement_document_tree_driver<A>(
    inventory_authority: DocumentInventoryAuthority,
    adapter: A,
) -> SourceBackedRouteDriver<A::Lifecycle, A::RouteControl>
where
    A: ReplacementDocumentTree,
{
    let adapter = Arc::new(adapter);
    let uses_parallel_leaf_workers = !matches!(
        adapter.leaf_execution_policy(),
        DocumentLeafExecutionPolicy::Serial
    );
    let state = Arc::new(Mutex::new(
        DocumentCommitState::<A::Leaf, A::TreeAuthority>::default(),
    ));
    let scan_adapter = Arc::clone(&adapter);
    let scan_state = Arc::clone(&state);
    let scan_authority = inventory_authority.clone();
    let owns_adapter = Arc::clone(&adapter);
    let source_state = Arc::clone(&state);
    let inventory_adapter = Arc::clone(&adapter);
    let inventory_state = Arc::clone(&state);
    let publication_adapter = Arc::clone(&adapter);
    let publication_state = Arc::clone(&state);
    let fence_adapter = Arc::clone(&adapter);
    let fence_state = Arc::clone(&state);
    let control_adapter = Arc::clone(&adapter);
    let control_state = Arc::clone(&state);
    let has_successful_publication_work = publication_adapter.has_successful_publication_work();
    let route_control_expectation = adapter.route_control_expectation();

    let mut driver = SourceBackedRouteDriver::new(
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
    .with_fallible_complete_inventory_revalidation(move |inventory| {
        revalidate_document_inventory(inventory_adapter.as_ref(), &inventory_state, inventory)
    })
    .with_publication_revalidation(move || {
        let Ok(state) = fence_state.lock() else {
            return false;
        };
        let Some(expected) = state.expected.as_ref() else {
            return false;
        };
        if expected.tree.inventory_scope == DocumentInventoryScope::Complete {
            return true;
        }
        fence_adapter
            .revalidate_complete(&expected.tree)
            .is_ok_and(|terminal| terminal == expected.tree.tree_fingerprint)
            && revalidate_durable_replay_sources(fence_adapter.as_ref(), &expected.tree)
    })
    .with_publication_control(move || {
        let state = control_state
            .lock()
            .map_err(|_| document_internal("document commit state lock was poisoned"))?;
        let expected = state
            .expected
            .as_ref()
            .ok_or_else(|| document_internal("document route has no expected publication"))?;
        control_adapter.publication_control(&expected.tree)
    });
    if let Some(expectation) = route_control_expectation {
        driver = driver.with_route_control_expectation(expectation);
    }
    if uses_parallel_leaf_workers {
        driver = driver.with_parallel_leaf_workers();
    }
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
    sink: &mut SourceBackedGenerationSink<'_, A::Lifecycle>,
) -> SourceBackedRouteResult<ExpectedDocumentRoute<A::Leaf, A::TreeAuthority>>
where
    A: ReplacementDocumentTree,
{
    let base_route_control = sink.base_route_control().map(<[u8]>::to_vec);
    let reconciliation_demand = sink.reconciliation_demand();
    let lazy_base_route = adapter.uses_lazy_base_route(reconciliation_demand);
    let mut base_sources = if lazy_base_route {
        Vec::new()
    } else {
        document_base_sources(sink, |source| adapter.owns_source(source))?
    };
    let mut tree = if lazy_base_route {
        let owns_source = |source: &SourceKey| adapter.owns_source(source);
        let mut base_route = DocumentBaseRoute::new(sink, &owns_source);
        adapter.discover_complete_with_lazy_base(&mut base_route)?
    } else {
        adapter.discover_complete_with_reconciliation(
            &base_sources,
            base_route_control.as_deref(),
            reconciliation_demand,
            &mut |progress| sink.report_current_source_progress(progress),
        )?
    };
    if lazy_base_route && tree.inventory_scope == DocumentInventoryScope::Complete {
        base_sources = document_base_sources(sink, |source| adapter.owns_source(source))?;
    }
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
            sink.recommended_leaf_workers(tree.leaves.len())
                .min(MAX_PARALLEL_DOCUMENT_LEAF_WORKERS),
            sink,
        )?,
        DocumentLeafExecutionPolicy::IndependentCapped(worker_count) => {
            scan_document_leaves_independently(
                adapter,
                &tree,
                &base_sources,
                replayable,
                adapter.parser_revision(),
                worker_count
                    .min(sink.recommended_leaf_workers(tree.leaves.len()))
                    .min(MAX_PARALLEL_DOCUMENT_LEAF_WORKERS),
                sink,
            )?
        }
    };

    let inventory = inventory_authority.certify(
        tree.tree_fingerprint,
        current_sources.ordered_inventory_sources(),
    )?;
    if tree.inventory_scope == DocumentInventoryScope::Partial {
        sink.retain_unstaged_base_route_sources()
            .map_err(route_coordinator_error)?;
        return Ok(ExpectedDocumentRoute::new(tree, certificates, inventory));
    }
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
    sink: &mut SourceBackedGenerationSink<'_, A::Lifecycle>,
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
            let append_base = adapter.append_base(&tree.authority, &observed.provider_leaf);
            let mut changed = if observed.replay_from_frontier && append_base.is_none() {
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
            let replay_fingerprint = observed
                .replay_from_frontier
                .then_some(observed.fingerprint);
            let terminal = match changed.preflight_terminal(
                terminal,
                replay_fingerprint,
                append_base.as_ref(),
            ) {
                Ok(terminal) => terminal,
                Err(error) => {
                    changed.preserve_record_rejections_on_failure();
                    return Err(error);
                }
            };
            changed.finish(terminal, append_base)?
        };
        let source = certificate.observation().source().clone();
        validate_current_document_source(adapter, &mut current_sources, source)?;
        certificates.push(certificate);
    }
    Ok((current_sources, certificates))
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

fn document_parallel_error<E: std::error::Error + 'static>(
    error: ParallelLeafScanError<SourceBackedRouteError, E>,
) -> SourceBackedRouteError {
    match error {
        ParallelLeafScanError::Worker { source, .. } => source,
        ParallelLeafScanError::Sink { source, .. } => route_coordinator_error(*source),
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
    sink: &mut SourceBackedGenerationSink<'_, impl CaptureLifecycleSink>,
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

pub fn document_frontier_fingerprint(
    certificate: &CertifiedSource,
) -> Option<DocumentLeafFingerprint> {
    decode_document_full_snapshot_checkpoint(certificate)
        .ok()
        .map(|checkpoint| checkpoint.physical_fingerprint)
}

fn document_base_sources<L: CaptureLifecycleSink>(
    sink: &SourceBackedGenerationSink<'_, L>,
    owns: impl Fn(&SourceKey) -> bool,
) -> SourceBackedRouteResult<Vec<CertifiedSource>> {
    let mut sources = sink
        .base_route_sources()
        .map_err(route_coordinator_error)?
        .into_values()
        .filter(|source| owns(source.observation().source()))
        .collect::<Vec<_>>();
    sources.sort_by_key(|source| source.observation().source().identity().digest());
    Ok(sources)
}

fn route_coordinator_error<E: std::error::Error + 'static>(
    error: SourceBackedCoordinatorError<E>,
) -> SourceBackedRouteError {
    match error {
        SourceBackedCoordinatorError::CoreEmission(source) => source,
        error => {
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
        }
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

fn reject_all_rejected_document_source(
    terminal: &DocumentSourceTerminal,
) -> SourceBackedRouteResult<()> {
    let counts = terminal.counts;
    if counts.complete_records > 0 && counts.retained_records == 0 && counts.rejected_records > 0 {
        Err(document_contract_error("document source is unreadable"))
    } else {
        Ok(())
    }
}
