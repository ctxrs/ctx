use std::{marker::PhantomData, ops::Deref, sync::Mutex};

use super::*;
use crate::source_backed::{
    family::document::{
        ChangedDocumentSink, CompleteDocumentTree, DocumentAppendBase, DocumentBaseRoute,
        DocumentRecordSpool, DocumentSourceTerminal, ReplacementDocumentTree,
    },
    route_error as default_route_error, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResult,
};

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static BEFORE_HERMES_SNAPSHOT_SEAL_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static AFTER_HERMES_SNAPSHOT_SEAL_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn set_before_hermes_snapshot_seal_hook(hook: impl FnOnce() + 'static) {
    BEFORE_HERMES_SNAPSHOT_SEAL_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn set_after_hermes_snapshot_seal_hook(hook: impl FnOnce() + 'static) {
    AFTER_HERMES_SNAPSHOT_SEAL_HOOK.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(any(test, feature = "test-support"))]
fn run_before_hermes_snapshot_seal_hook() {
    BEFORE_HERMES_SNAPSHOT_SEAL_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(any(test, feature = "test-support"))]
fn run_after_hermes_snapshot_seal_hook() {
    AFTER_HERMES_SNAPSHOT_SEAL_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static DOCUMENT_BASE_ROUTE_SOURCE_VISITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn reset_document_base_route_source_visits() {
    DOCUMENT_BASE_ROUTE_SOURCE_VISITS.with(|visits| visits.set(0));
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn document_base_route_source_visits() -> u64 {
    DOCUMENT_BASE_ROUTE_SOURCE_VISITS.with(std::cell::Cell::get)
}

pub(crate) struct HermesTreeAuthority {
    opening_evidence: Option<SqliteSourceEvidence>,
    schema: Option<HermesSchema>,
    _schema_evidence: Vec<u8>,
    _sqlite_authority: Option<SqliteSourceDirectoryAuthority>,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
    message_spool: Option<Mutex<HermesExactMessageSpool>>,
    publication_receipt: HermesRefreshReceipt,
    terminal_revalidate:
        Option<Box<dyn Fn() -> Result<(), SqliteSourceAccessError> + Send + Sync + 'static>>,
    physical_replay: Option<HermesPhysicalReplay>,
    deferred_incremental: bool,
}

struct HermesPhysicalReplay {
    fence: SqliteSourceReplayFence,
}

struct HermesCompleteReconciliationContext<'a, L: CaptureLifecycleSink> {
    base_sources: &'a [CertifiedSource],
    route_control: Option<&'a [u8]>,
    demand: SourceBackedReconciliationDemand,
    report_progress:
        &'a mut dyn FnMut(SourceBackedCurrentSourceProgress) -> SourceBackedRouteResult<()>,
    marker: PhantomData<fn() -> L>,
}

impl<L: CaptureLifecycleSink> HermesReconciliationContext<L>
    for HermesCompleteReconciliationContext<'_, L>
{
    fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand {
        self.demand
    }

    fn route_control(&self) -> Option<&[u8]> {
        self.route_control
    }

    fn exact_base_source(&self, source: &SourceKey) -> Option<DocumentAppendBase<L>> {
        self.base_sources
            .iter()
            .find(|base| base.observation().source().exact_descriptor_eq(source))
            .cloned()
            .map(|certificate| DocumentAppendBase::Certificate(Box::new(certificate)))
    }

    fn report_progress(
        &mut self,
        progress: SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()> {
        (self.report_progress)(progress)
    }
}

impl<L: CaptureLifecycleSink> HermesReconciliationContext<L> for DocumentBaseRoute<'_, '_, L> {
    fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand {
        DocumentBaseRoute::reconciliation_demand(self)
    }

    fn route_control(&self) -> Option<&[u8]> {
        DocumentBaseRoute::route_control(self)
    }

    fn exact_base_source(&self, source: &SourceKey) -> Option<DocumentAppendBase<L>> {
        #[cfg(any(test, feature = "test-support"))]
        DOCUMENT_BASE_ROUTE_SOURCE_VISITS.with(|visits| {
            visits.set(visits.get().saturating_add(1));
        });
        DocumentBaseRoute::exact_source(self, source)
    }

    fn report_progress(
        &mut self,
        progress: SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()> {
        DocumentBaseRoute::report_progress(self, progress)
    }
}

pub(crate) struct HermesDocumentAdapter<L, S> {
    candidate: HermesSourceCandidate,
    marker: PhantomData<fn() -> (L, S)>,
}

impl<L, S> HermesDocumentAdapter<L, S> {
    pub(crate) fn new(candidate: HermesSourceCandidate) -> Self {
        Self {
            candidate,
            marker: PhantomData,
        }
    }
}

impl<L, S> Deref for HermesDocumentAdapter<L, S> {
    type Target = HermesSourceCandidate;

    fn deref(&self) -> &Self::Target {
        &self.candidate
    }
}

impl<L, S> ReplacementDocumentTree for HermesDocumentAdapter<L, S>
where
    L: CaptureLifecycleSink + 'static,
    L::PinnedAppendBase: Clone + Send + Sync + 'static,
    S: DocumentRecordSpool,
{
    type Lifecycle = L;
    type Spool = S;
    type RouteControl = crate::ProviderRouteControlExpectation;
    type Leaf = HermesSessionLeaf<L>;
    type TreeAuthority = HermesTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        HERMES_SOURCE_PARSER_REVISION
    }

    fn route_control_expectation(&self) -> Option<Self::RouteControl> {
        Some(crate::ProviderRouteControlExpectation::new(
            HERMES_ROUTE_CONTROL_KIND,
            self.source.exact_descriptor_digest(),
            hermes_route_control_exact_due_for_profile,
            Some(hermes_route_control_database_identity),
        ))
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        if source.provider() != CaptureProvider::Hermes.as_str() {
            return false;
        }
        if source.source_format() != HERMES_SQLITE_SOURCE_FORMAT
            || source.provider_identity_version() != 1
        {
            return false;
        }
        hermes_provider_session_id(&self.source, source).is_some()
    }

    fn durable_replay_source(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<Option<SourceKey>> {
        Ok(Some(leaf.source.clone()))
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        self.discover_complete_with_progress(&[], &mut |_| Ok(()))
    }

    fn discover_complete_with_progress(
        &self,
        base_sources: &[CertifiedSource],
        report_progress: &mut dyn FnMut(
            SourceBackedCurrentSourceProgress,
        ) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        self.discover_complete_with_reconciliation(
            base_sources,
            None,
            SourceBackedReconciliationDemand::Exhaustive,
            report_progress,
        )
    }

    fn discover_complete_with_reconciliation(
        &self,
        base_sources: &[CertifiedSource],
        base_route_control: Option<&[u8]>,
        reconciliation_demand: SourceBackedReconciliationDemand,
        report_progress: &mut dyn FnMut(
            SourceBackedCurrentSourceProgress,
        ) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let mut context = HermesCompleteReconciliationContext {
            base_sources,
            route_control: base_route_control,
            demand: reconciliation_demand,
            report_progress,
            marker: PhantomData,
        };
        discover_hermes_tree(self, &mut context)
    }

    fn uses_lazy_base_route(&self, demand: SourceBackedReconciliationDemand) -> bool {
        demand == SourceBackedReconciliationDemand::Incremental
    }

    fn discover_complete_with_lazy_base(
        &self,
        base_route: &mut DocumentBaseRoute<'_, '_, L>,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        discover_hermes_tree(self, base_route)
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let expected = hermes_session_source_key(&self.source, &leaf.provider_session_id)
            .map_err(hermes_route_error)?;
        if !expected.exact_descriptor_eq(&leaf.source) {
            return Err(hermes_changed(
                "Hermes logical session source changed after inventory observation",
            ));
        }
        let snapshot = take_snapshot(&authority.snapshot)?;
        let scan = (|| {
            sink.begin_source(leaf.source.clone())?;
            let mut sink_error = None;
            let mut project = |output| match output {
                HermesSnapshotProjectionOutput::Page(page) => {
                    if let Err(error) = sink.report_completed_bytes(page.completed_bytes) {
                        let detail = error.to_string();
                        sink_error = Some(error);
                        return Err(HermesSourceBackedError::Capture(
                            CaptureError::InvalidPayload(detail),
                        ));
                    }
                    for record in page.records {
                        if let HermesSourceBackedRecord::Event(document) = record {
                            if let Err(error) = sink.emit_core_record(document) {
                                let detail = error.to_string();
                                sink_error = Some(error);
                                return Err(HermesSourceBackedError::Capture(
                                    CaptureError::InvalidPayload(detail),
                                ));
                            }
                        }
                    }
                    Ok(())
                }
                HermesSnapshotProjectionOutput::Progress(progress) => sink
                    .report_current_source_progress(progress)
                    .map_err(|error| {
                        sink_error = Some(error.clone());
                        HermesSourceBackedError::Route(error)
                    }),
            };
            let scan = if let Some(incremental) = leaf.incremental.as_ref() {
                project_hermes_incremental_leaf_with_progress(self, leaf, incremental, &mut project)
            } else {
                let mut message_spool = authority
                    .message_spool
                    .as_ref()
                    .ok_or_else(|| hermes_internal("Hermes exact message spool is unavailable"))?
                    .lock()
                    .map_err(|_| hermes_internal("Hermes exact message spool lock is poisoned"))?;
                project_hermes_session_snapshot_with_progress(
                    self,
                    leaf,
                    authority
                        .schema
                        .as_ref()
                        .ok_or_else(|| hermes_internal("Hermes snapshot schema is unavailable"))?,
                    snapshot.connection().map_err(hermes_sqlite_route_error)?,
                    &mut message_spool,
                    &mut project,
                )
            };
            if let Some(error) = sink_error {
                return Err(error);
            }
            let scan = scan.map_err(hermes_route_error)?;
            let counts = scan.certificate.counts();
            if scan.decoded_rows != counts.complete_records
                || scan.peak_buffered_records > 64
                || (counts.complete_records == 0) != (scan.emitted_pages == 0)
                || scan.native_candidate_query_batches == 0
                || scan.native_hydration_query_batches > scan.native_candidate_query_batches
                || scan.max_native_rows_per_set > 64
            {
                return Err(hermes_internal(
                    "Hermes scan violated its one-pass bounded-page receipt",
                ));
            }
            if authority.opening_evidence.as_ref() != Some(snapshot.evidence()) {
                return Err(hermes_changed(
                    "Hermes source changed between physical discovery and logical scan",
                ));
            }
            snapshot.revalidate().map_err(hermes_sqlite_route_error)?;
            Ok(scan)
        })();
        let scan = match scan {
            Ok(scan) => scan,
            Err(error) => return Err(abort_hermes_route_snapshot(snapshot, error)),
        };
        if let Err(failure) = restore_snapshot(&authority.snapshot, snapshot) {
            let (error, snapshot) = *failure;
            return Err(abort_hermes_route_snapshot(snapshot, error));
        }
        Ok(document_terminal(scan.certificate))
    }

    fn append_base(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> Option<DocumentAppendBase<L>> {
        leaf.incremental
            .as_ref()
            .and_then(|incremental| incremental.base.clone())
    }

    fn publication_control(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<Option<Vec<u8>>> {
        serde_json::to_vec(&tree.authority.publication_receipt)
            .map(Some)
            .map_err(HermesSourceBackedError::from)
            .map_err(hermes_route_error)
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        if let Some(replay) = &tree.authority.physical_replay {
            let snapshot_present = tree
                .authority
                .snapshot
                .lock()
                .map_err(|_| hermes_internal("Hermes snapshot lock was poisoned"))?
                .is_some();
            if !tree.leaves.is_empty() || snapshot_present {
                return Err(hermes_internal(
                    "exact Hermes replay retained logical snapshot work",
                ));
            }
            #[cfg(any(test, feature = "test-support"))]
            run_before_hermes_snapshot_seal_hook();
            #[cfg(any(test, feature = "test-support"))]
            run_after_hermes_snapshot_seal_hook();
            replay
                .fence
                .revalidate()
                .map_err(hermes_sqlite_route_error)?;
            return Ok(tree.tree_fingerprint);
        }
        if tree.authority.deferred_incremental {
            let snapshot_present = tree
                .authority
                .snapshot
                .lock()
                .map_err(|_| hermes_internal("Hermes snapshot lock was poisoned"))?
                .is_some();
            if !tree.leaves.is_empty() || snapshot_present {
                return Err(hermes_internal(
                    "deferred Hermes incremental route retained snapshot work",
                ));
            }
            return Ok(tree.tree_fingerprint);
        }
        let snapshot = take_snapshot(&tree.authority.snapshot)?;
        #[cfg(any(test, feature = "test-support"))]
        run_before_hermes_snapshot_seal_hook();
        let evidence = route_hermes_terminal_revalidation(snapshot.finish())?;
        #[cfg(any(test, feature = "test-support"))]
        run_after_hermes_snapshot_seal_hook();
        if tree.authority.opening_evidence.as_ref() != Some(&evidence) {
            return Err(hermes_changed(format!(
                "{}: physical source changed before commit",
                HermesSourceBackedError::SourceChanged
            )));
        }
        let terminal_revalidate = tree
            .authority
            .terminal_revalidate
            .as_ref()
            .ok_or_else(|| hermes_internal("Hermes terminal revalidator is unavailable"))?;
        route_hermes_terminal_revalidation(terminal_revalidate())?;
        Ok(tree.tree_fingerprint)
    }
}

fn discover_hermes_tree<L: CaptureLifecycleSink>(
    candidate: &HermesSourceCandidate,
    context: &mut dyn HermesReconciliationContext<L>,
) -> SourceBackedRouteResult<CompleteDocumentTree<HermesSessionLeaf<L>, HermesTreeAuthority>>
where
    L::PinnedAppendBase: Clone,
{
    if std::fs::symlink_metadata(candidate.path()).is_err() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "selected Hermes database is unavailable",
        ));
    }
    let reconciliation_demand = context.reconciliation_demand();
    let base_route_control = context.route_control().map(<[u8]>::to_vec);
    if reconciliation_demand == SourceBackedReconciliationDemand::Exhaustive {
        if let Some(tree) = discover_hermes_physical_replay(
            candidate,
            base_route_control.as_deref(),
            hermes_now_ms(),
        )? {
            return Ok(tree);
        }
    }
    let may_increment = reconciliation_demand == SourceBackedReconciliationDemand::Incremental
        && hermes_refresh_receipt(base_route_control.as_deref()).is_some();
    let (mut sqlite_authority, mut snapshot, mut admitted_demand) = if may_increment {
        match open_root_authorized_snapshot_with_progress(
            &candidate.data_root,
            candidate.path(),
            true,
            &mut |progress| context.report_progress(progress),
        ) {
            Ok((authority, snapshot)) => (
                authority,
                snapshot,
                SourceBackedReconciliationDemand::Incremental,
            ),
            Err(error) if hermes_incremental_snapshot_unavailable(&error) => {
                let publication_receipt = hermes_refresh_receipt(base_route_control.as_deref())
                    .ok_or_else(|| hermes_changed("Hermes route control disappeared"))?;
                let tree_fingerprint = hermes_deferred_tree_fingerprint(
                    &candidate.source,
                    base_route_control
                        .as_deref()
                        .expect("validated Hermes route control"),
                );
                return Ok(CompleteDocumentTree::new_partial(
                    tree_fingerprint,
                    Vec::new(),
                    HermesTreeAuthority {
                        opening_evidence: None,
                        schema: None,
                        _schema_evidence: Vec::new(),
                        _sqlite_authority: None,
                        snapshot: Mutex::new(None),
                        message_spool: None,
                        publication_receipt,
                        terminal_revalidate: None,
                        physical_replay: None,
                        deferred_incremental: true,
                    },
                ));
            }
            Err(error) => return Err(hermes_route_error(error)),
        }
    } else {
        let (authority, snapshot) = open_root_authorized_snapshot_with_progress(
            &candidate.data_root,
            candidate.path(),
            false,
            &mut |progress| context.report_progress(progress),
        )
        .map_err(hermes_route_error)?;
        (
            authority,
            snapshot,
            SourceBackedReconciliationDemand::Exhaustive,
        )
    };
    if let Err(error) = snapshot.revalidate().map_err(hermes_sqlite_route_error) {
        return Err(abort_hermes_route_snapshot(snapshot, error));
    }
    if admitted_demand == SourceBackedReconciliationDemand::Incremental {
        let prior = hermes_refresh_receipt(base_route_control.as_deref())
            .ok_or_else(|| hermes_changed("Hermes route control disappeared"))?;
        let requires_exhaustive = hermes_incremental_requires_exhaustive(
            snapshot.connection().map_err(hermes_sqlite_route_error)?,
            &prior,
            candidate.source.exact_descriptor_digest(),
            *snapshot.evidence().identity(),
        )
        .map_err(hermes_route_error)?;
        if requires_exhaustive {
            snapshot.abort().map_err(hermes_sqlite_route_error)?;
            let reopened = open_root_authorized_snapshot_with_progress(
                &candidate.data_root,
                candidate.path(),
                false,
                &mut |progress| context.report_progress(progress),
            )
            .map_err(hermes_route_error)?;
            sqlite_authority = reopened.0;
            snapshot = reopened.1;
            admitted_demand = SourceBackedReconciliationDemand::Exhaustive;
        }
    }
    let opening_evidence = snapshot.evidence().clone();
    let inventory = match observe_hermes_reconciliation_inventory(
        candidate,
        snapshot.connection().map_err(hermes_sqlite_route_error)?,
        base_route_control.as_deref(),
        admitted_demand,
        HermesPhysicalSourceRevision {
            database_identity: *opening_evidence.identity(),
            physical_revision: *opening_evidence.physical_revision(),
        },
        hermes_now_ms(),
        context,
    ) {
        Ok(inventory) => inventory,
        Err(error) => {
            return Err(abort_hermes_route_snapshot(
                snapshot,
                hermes_route_error(error),
            ))
        }
    };
    let publication_receipt = inventory.publication_receipt.clone().ok_or_else(|| {
        hermes_internal("Hermes reconciliation produced no route control receipt")
    })?;
    let authority = HermesTreeAuthority {
        opening_evidence: Some(opening_evidence),
        schema: Some(inventory.schema),
        _schema_evidence: inventory.schema_evidence,
        _sqlite_authority: Some(sqlite_authority),
        terminal_revalidate: Some(snapshot.terminal_revalidator()),
        physical_replay: None,
        snapshot: Mutex::new(Some(snapshot)),
        message_spool: inventory.message_spool.map(Mutex::new),
        publication_receipt,
        deferred_incremental: false,
    };
    if inventory.reconciliation_demand == SourceBackedReconciliationDemand::Incremental {
        Ok(CompleteDocumentTree::new_partial(
            inventory.tree_fingerprint,
            inventory.leaves,
            authority,
        ))
    } else {
        Ok(CompleteDocumentTree::new(
            inventory.tree_fingerprint,
            inventory.leaves,
            authority,
        ))
    }
}

fn discover_hermes_physical_replay<L: CaptureLifecycleSink>(
    candidate: &HermesSourceCandidate,
    base_route_control: Option<&[u8]>,
    now_ms: i64,
) -> SourceBackedRouteResult<Option<CompleteDocumentTree<HermesSessionLeaf<L>, HermesTreeAuthority>>>
where
    L::PinnedAppendBase: Clone,
{
    let Some(prior) = hermes_refresh_receipt(base_route_control) else {
        return Ok(None);
    };
    if prior.kind != HERMES_ROUTE_CONTROL_KIND
        || prior.version != HERMES_ROUTE_CONTROL_VERSION
        || prior.parser_revision != HERMES_SOURCE_PARSER_REVISION
        || prior.outcome != "successful"
        || prior.profile_source_descriptor != candidate.source.exact_descriptor_digest()
    {
        return Ok(None);
    }
    let retained = retain_root_authorized_source(&candidate.data_root, candidate.path())
        .map_err(hermes_route_error)?;
    let physical_fence = match retained
        .sqlite_authority
        .observe_replay_fence(&retained.database_leaf)
    {
        Ok(fence) => fence,
        Err(error) if error.is_source_changed() => return Ok(None),
        Err(error) => return Err(hermes_sqlite_route_error(error)),
    };
    let physical_revision = *physical_fence.revision();
    if physical_revision != prior.physical_revision {
        return Ok(None);
    }
    let publication_receipt = if prior.exact_due_at_ms <= now_ms {
        hermes_advanced_exact_receipt(prior, now_ms)
    } else {
        prior
    };
    let tree_fingerprint = hermes_physical_replay_tree_fingerprint(
        &candidate.source,
        physical_revision,
        &publication_receipt,
    )?;
    Ok(Some(CompleteDocumentTree::new_partial(
        tree_fingerprint,
        Vec::new(),
        HermesTreeAuthority {
            opening_evidence: None,
            schema: None,
            _schema_evidence: Vec::new(),
            _sqlite_authority: None,
            snapshot: Mutex::new(None),
            message_spool: None,
            publication_receipt,
            terminal_revalidate: None,
            physical_replay: Some(HermesPhysicalReplay {
                fence: physical_fence,
            }),
            deferred_incremental: false,
        },
    )))
}

fn hermes_advanced_exact_receipt(
    mut prior: HermesRefreshReceipt,
    now_ms: i64,
) -> HermesRefreshReceipt {
    if prior.last_successful_exhaustive_at_ms != now_ms {
        prior.exhaustive_sequence = prior.exhaustive_sequence.saturating_add(1);
    }
    prior.last_successful_exhaustive_at_ms = now_ms;
    prior.exact_due_at_ms = now_ms.saturating_add(HERMES_EXACT_INTERVAL_MS);
    prior.mode = "exhaustive".to_owned();
    prior.outcome = "successful".to_owned();
    prior
}

fn hermes_physical_replay_tree_fingerprint(
    profile_source: &SourceKey,
    physical_revision: [u8; 32],
    receipt: &HermesRefreshReceipt,
) -> SourceBackedRouteResult<[u8; 32]> {
    let receipt = serde_json::to_vec(receipt)
        .map_err(HermesSourceBackedError::from)
        .map_err(hermes_route_error)?;
    let mut digest = Sha256::new();
    digest.update(b"ctx-hermes-exact-physical-replay-v1\0");
    digest.update(profile_source.exact_descriptor_digest());
    digest.update(physical_revision);
    digest.update((receipt.len() as u64).to_be_bytes());
    digest.update(receipt);
    Ok(digest.finalize().into())
}

fn hermes_incremental_snapshot_unavailable(error: &HermesSourceBackedError) -> bool {
    match error {
        HermesSourceBackedError::SqliteSource(error) => error.is_snapshot_unavailable(),
        HermesSourceBackedError::SqliteFinalization {
            primary,
            finalization,
        } => {
            hermes_incremental_snapshot_unavailable(primary)
                || finalization.is_snapshot_unavailable()
        }
        _ => false,
    }
}

fn hermes_deferred_tree_fingerprint(profile_source: &SourceKey, route_control: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-hermes-deferred-incremental-v1\0");
    digest.update(profile_source.exact_descriptor_digest());
    digest.update((route_control.len() as u64).to_be_bytes());
    digest.update(route_control);
    digest.finalize().into()
}

pub(super) fn hermes_route_error(error: HermesSourceBackedError) -> SourceBackedRouteError {
    let error = match error {
        HermesSourceBackedError::Route(error) => return error,
        error => error,
    };
    let kind = match &error {
        HermesSourceBackedError::SqliteSource(error) if error.is_source_changed() => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        HermesSourceBackedError::SqliteSource(error) if error.is_snapshot_capacity_failure() => {
            SourceBackedRouteErrorKind::Unavailable
        }
        HermesSourceBackedError::SqliteSource(error) if error.is_systemic_resource_failure() => {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        HermesSourceBackedError::SqliteSource(error) if error.is_ctx_owned_corruption() => {
            SourceBackedRouteErrorKind::Internal
        }
        HermesSourceBackedError::Capture(CaptureError::Io(error))
            if crate::provider_sources::resource_exhaustion_io_error(error) =>
        {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        HermesSourceBackedError::Capture(CaptureError::SystemIo { source, .. })
            if crate::provider_sources::resource_exhaustion_io_error(source) =>
        {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        HermesSourceBackedError::Capture(CaptureError::Sqlite(error))
            if crate::provider_sources::rusqlite_resource_failure(error) =>
        {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        _ => return default_route_error(error),
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

pub(super) fn hermes_sqlite_route_error(error: SqliteSourceAccessError) -> SourceBackedRouteError {
    hermes_route_error(error.into())
}

pub(super) fn route_hermes_terminal_revalidation<T>(
    result: Result<T, SqliteSourceAccessError>,
) -> SourceBackedRouteResult<T> {
    result.map_err(hermes_sqlite_route_error)
}

fn abort_hermes_route_snapshot(
    snapshot: SqliteSourceReadSnapshot,
    primary: SourceBackedRouteError,
) -> SourceBackedRouteError {
    match snapshot.abort() {
        Ok(()) => primary,
        Err(cleanup) => crate::source_backed::combine_primary_and_cleanup_route_errors(
            primary,
            hermes_sqlite_route_error(cleanup),
        ),
    }
}

fn take_snapshot(
    slot: &Mutex<Option<SqliteSourceReadSnapshot>>,
) -> SourceBackedRouteResult<SqliteSourceReadSnapshot> {
    slot.lock()
        .map_err(|_| hermes_internal("Hermes SQLite snapshot lock was poisoned"))?
        .take()
        .ok_or_else(|| hermes_internal("Hermes SQLite snapshot was already consumed"))
}

fn restore_snapshot(
    slot: &Mutex<Option<SqliteSourceReadSnapshot>>,
    snapshot: SqliteSourceReadSnapshot,
) -> Result<(), Box<(SourceBackedRouteError, SqliteSourceReadSnapshot)>> {
    let mut slot = match slot.lock() {
        Ok(slot) => slot,
        Err(_) => {
            return Err(Box::new((
                hermes_internal("Hermes SQLite snapshot lock was poisoned"),
                snapshot,
            )));
        }
    };
    if slot.is_some() {
        return Err(Box::new((
            hermes_internal("Hermes SQLite snapshot slot was already occupied"),
            snapshot,
        )));
    }
    *slot = Some(snapshot);
    Ok(())
}

fn document_terminal(certificate: CertifiedSource) -> DocumentSourceTerminal {
    DocumentSourceTerminal {
        source: certificate.observation().source().clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: HERMES_SOURCE_PARSER_REVISION,
        content_digest: *certificate.content_digest(),
        counts: certificate.counts(),
    }
}

fn hermes_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn hermes_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use crate::provider_sources::SqliteRetryDecision;
    use rusqlite::ffi;

    #[test]
    fn successful_overdue_exact_check_advances_the_next_deadline() {
        let prior = HermesRefreshReceipt {
            kind: HERMES_ROUTE_CONTROL_KIND.to_owned(),
            version: HERMES_ROUTE_CONTROL_VERSION,
            parser_revision: HERMES_SOURCE_PARSER_REVISION.to_owned(),
            profile_source_descriptor: [1; 32],
            database_identity: [2; 32],
            physical_revision: [3; 32],
            schema_evidence: [4; 32],
            session_rowid: 5,
            message_rowid: 6,
            last_successful_exhaustive_at_ms: 100,
            exact_due_at_ms: 200,
            exhaustive_sequence: 1,
            mode: "incremental".to_owned(),
            outcome: "successful".to_owned(),
        };
        let now_ms = 300;

        let advanced = hermes_advanced_exact_receipt(prior, now_ms);
        let control = serde_json::to_vec(&advanced).unwrap();

        assert_eq!(advanced.last_successful_exhaustive_at_ms, now_ms);
        assert_eq!(advanced.exhaustive_sequence, 2);
        assert_eq!(
            hermes_route_control_exact_due(&control, now_ms),
            Some(false)
        );
    }

    #[test]
    fn real_hermes_projection_full_failure_is_systemic() {
        let directory = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(directory.path().join("full.sqlite")).unwrap();
        connection
            .execute_batch(
                "PRAGMA page_size=512;
                 PRAGMA max_page_count=2;
                 CREATE TABLE payload(value BLOB)",
            )
            .unwrap();
        let sqlite = (0..128)
            .find_map(|_| {
                connection
                    .execute("INSERT INTO payload VALUES (zeroblob(4096))", [])
                    .err()
            })
            .unwrap();
        let diagnosed = diagnose_hermes_query_error(
            HermesSourceBackedError::Capture(CaptureError::Sqlite(sqlite)),
            SqliteFailurePhase::Projection,
        );
        let HermesSourceBackedError::SqliteSource(source) = &diagnosed else {
            panic!("unexpected Hermes error: {diagnosed:?}");
        };
        let diagnostic = source.diagnostic().unwrap();
        assert_eq!(diagnostic.phase, SqliteFailurePhase::Projection);
        assert_eq!(diagnostic.artifact, SqliteArtifactKind::PrivateSourceCopy);
        assert_eq!(diagnostic.sqlite_primary_code, Some(ffi::SQLITE_FULL));
        assert_eq!(
            crate::provider_sources::sqlite_retry_decision(source),
            SqliteRetryDecision::RouteFatalResource
        );
        assert_eq!(
            hermes_route_error(diagnosed).kind,
            SourceBackedRouteErrorKind::ResourceUnavailable
        );
    }
}
