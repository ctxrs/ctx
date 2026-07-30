use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, CertifiedSource, SourceKey};

use super::{
    decode_certificate, hydrate_batch, hydrate_single, DirectJsonlCheckpoint,
    DirectJsonlDisposition, DirectJsonlHydrationCatalog, DirectJsonlInventoryLeaf,
    DirectJsonlScanReceipt, DirectJsonlSourceAdapter, DirectJsonlSourceBackedError,
    DirectJsonlTerminalEvidenceSet,
};
use crate::provider::source_backed::{
    executable_route, invalid_route, route_coordinator_error, route_error,
    source_backed_base_removals, source_backed_base_sources, ParallelLeafScanBegin,
    ParallelLeafScanCancelled, ParallelLeafScanComplete, ParallelLeafScanEmitter,
    ParallelLeafScanError, ParallelLeafScanWorkerError, SourceBackedCoordinatorError,
    SourceBackedCoordinatorResult, SourceBackedGenerationSink, SourceBackedProviderRegistry,
    SourceBackedRevalidationTarget, SourceBackedRouteDriver, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResult, SourceBackedRouteSelection,
    SourceBackedSelectorAuthority,
};
use crate::ProviderSource;

#[cfg(test)]
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Barrier,
};

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirectJsonlRegistrationTestEvent {
    BeginSource,
    BeginSourceAppend,
    SourceRevalidated,
    CompleteInventoryAccepted,
    CompleteInventoryRejected,
}

#[cfg(test)]
pub(super) type DirectJsonlRegistrationTestObserver =
    Arc<dyn Fn(DirectJsonlRegistrationTestEvent) + Send + Sync>;

const DIRECT_JSONL_MAX_SCANNER_WORKERS: usize = 16;

#[cfg(test)]
std::thread_local! {
    static DIRECT_JSONL_LIFECYCLE_WORK: std::cell::Cell<DirectJsonlLifecycleWork> =
        const { std::cell::Cell::new(DirectJsonlLifecycleWork {
            base_certificate_decodes: 0,
            base_index_entries: 0,
            base_index_lookups: 0,
            current_index_entries: 0,
            retirement_lookups: 0,
        }) };
    static DIRECT_JSONL_SCANNER_WORKERS_OVERRIDE: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
    static DIRECT_JSONL_SCANNER_ACTIVITY: std::cell::Cell<DirectJsonlScannerActivity> =
        const { std::cell::Cell::new(DirectJsonlScannerActivity {
            worker_count: 0,
            sources_started: 0,
            sources_completed: 0,
            peak_active_scanners: 0,
        }) };
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DirectJsonlScannerActivity {
    pub(crate) worker_count: usize,
    pub(crate) sources_started: usize,
    pub(crate) sources_completed: usize,
    pub(crate) peak_active_scanners: usize,
}

#[cfg(test)]
pub(super) fn reset_lifecycle_work() {
    DIRECT_JSONL_LIFECYCLE_WORK.set(DirectJsonlLifecycleWork::default());
}

#[cfg(test)]
pub(super) fn lifecycle_work() -> DirectJsonlLifecycleWork {
    DIRECT_JSONL_LIFECYCLE_WORK.get()
}

#[cfg(test)]
pub(super) fn with_scanner_workers<T>(workers: usize, run: impl FnOnce() -> T) -> T {
    struct ResetScannerWorkers(Option<usize>);

    impl Drop for ResetScannerWorkers {
        fn drop(&mut self) {
            DIRECT_JSONL_SCANNER_WORKERS_OVERRIDE.set(self.0);
        }
    }

    let previous = DIRECT_JSONL_SCANNER_WORKERS_OVERRIDE.replace(Some(workers));
    let _reset = ResetScannerWorkers(previous);
    DIRECT_JSONL_SCANNER_ACTIVITY.set(DirectJsonlScannerActivity::default());
    run()
}

#[cfg(test)]
pub(super) fn scanner_activity() -> DirectJsonlScannerActivity {
    DIRECT_JSONL_SCANNER_ACTIVITY.get()
}

#[cfg(test)]
fn record_lifecycle_work(
    base_certificate_decodes: usize,
    base_index_entries: usize,
    base_index_lookups: usize,
    current_index_entries: usize,
    retirement_lookups: usize,
) {
    let work = DIRECT_JSONL_LIFECYCLE_WORK.get();
    DIRECT_JSONL_LIFECYCLE_WORK.set(DirectJsonlLifecycleWork {
        base_certificate_decodes: work
            .base_certificate_decodes
            .saturating_add(base_certificate_decodes),
        base_index_entries: work.base_index_entries.saturating_add(base_index_entries),
        base_index_lookups: work.base_index_lookups.saturating_add(base_index_lookups),
        current_index_entries: work
            .current_index_entries
            .saturating_add(current_index_entries),
        retirement_lookups: work.retirement_lookups.saturating_add(retirement_lookups),
    });
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DirectJsonlLifecycleWork {
    pub(crate) base_certificate_decodes: usize,
    pub(crate) base_index_entries: usize,
    pub(crate) base_index_lookups: usize,
    pub(crate) current_index_entries: usize,
    pub(crate) retirement_lookups: usize,
}

pub(crate) fn register(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    #[cfg(test)]
    {
        register_inner(registry, source, selection, None)
    }
    #[cfg(not(test))]
    register_inner(registry, source, selection)
}

#[cfg(test)]
pub(super) fn register_with_test_observer(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    observer: DirectJsonlRegistrationTestObserver,
) -> SourceBackedCoordinatorResult<()> {
    register_inner(registry, source, selection, Some(observer))
}

fn register_inner(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    #[cfg(test)] test_observer: Option<DirectJsonlRegistrationTestObserver>,
) -> SourceBackedCoordinatorResult<()> {
    let adapter = adapter(source.provider).ok_or_else(|| {
        invalid_route(
            source.provider,
            "provider is not a member of the direct native-JSONL adapter family",
        )
    })?;
    let root = source.path.clone();
    let capture_root = root.clone();
    let source_revalidation_root = root.clone();
    let inventory_revalidation_root = root.clone();
    let hydration_root = root.clone();
    let batch_hydration_root = root;
    let provider = source.provider;
    let certified_source_format = adapter.source_format();
    let terminal_evidence = Arc::new(DirectJsonlTerminalEvidenceSet::default());
    let capture_terminal_evidence = Arc::clone(&terminal_evidence);
    let revalidation_terminal_evidence = Arc::clone(&terminal_evidence);
    let inventory_terminal_evidence = terminal_evidence;
    #[cfg(test)]
    let capture_test_observer = test_observer.clone();
    #[cfg(test)]
    let revalidation_test_observer = test_observer.clone();
    #[cfg(test)]
    let inventory_test_observer = test_observer;
    let route_sources = Arc::new(Mutex::new(None::<HashMap<[u8; 32], SourceKey>>));
    let capture_route_sources = Arc::clone(&route_sources);
    let owns_route_sources = Arc::clone(&route_sources);
    let hydration_catalog = Arc::new(Mutex::new(None::<DirectJsonlHydrationCatalog>));
    let single_hydration_catalog = Arc::clone(&hydration_catalog);
    let batch_hydration_catalog = Arc::clone(&hydration_catalog);
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            #[cfg(test)]
            {
                capture(
                    adapter,
                    &capture_root,
                    &capture_terminal_evidence,
                    &capture_route_sources,
                    sink,
                    capture_test_observer.as_ref(),
                )
            }
            #[cfg(not(test))]
            capture(
                adapter,
                &capture_root,
                &capture_terminal_evidence,
                &capture_route_sources,
                sink,
            )
        },
        move |source| {
            source.provider() == provider.as_str()
                && source.source_format() == certified_source_format
                && owns_route_sources.lock().is_ok_and(|sources| {
                    sources.as_ref().is_none_or(|sources| {
                        sources
                            .get(&source.exact_descriptor_digest())
                            .is_some_and(|owned| owned.exact_descriptor_eq(source))
                    })
                })
        },
        move |target| {
            #[cfg(test)]
            let is_source = matches!(&target, SourceBackedRevalidationTarget::Source(_));
            let valid = revalidate_target(
                adapter,
                &source_revalidation_root,
                &revalidation_terminal_evidence,
                target,
            )
            .unwrap_or(false);
            #[cfg(test)]
            if valid && is_source {
                notify_test_observer(
                    revalidation_test_observer.as_ref(),
                    DirectJsonlRegistrationTestEvent::SourceRevalidated,
                );
            }
            valid
        },
        move |request| hydrate_single(adapter, &hydration_root, &single_hydration_catalog, request),
    )
    .with_batch_hydration(move |request| {
        hydrate_batch(
            adapter,
            &batch_hydration_root,
            &batch_hydration_catalog,
            request,
        )
    })
    .with_complete_inventory_revalidation(move |expected| {
        let valid = adapter
            .revalidate_inventory_with_evidence(
                &inventory_revalidation_root,
                &inventory_terminal_evidence,
                expected,
            )
            .unwrap_or(false);
        #[cfg(test)]
        notify_test_observer(
            inventory_test_observer.as_ref(),
            if valid {
                DirectJsonlRegistrationTestEvent::CompleteInventoryAccepted
            } else {
                DirectJsonlRegistrationTestEvent::CompleteInventoryRejected
            },
        );
        valid
    });
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn adapter(provider: CaptureProvider) -> Option<DirectJsonlSourceAdapter> {
    match provider {
        CaptureProvider::Antigravity => Some(super::super::antigravity_source_backed_adapter()),
        CaptureProvider::CopilotCli => Some(super::super::copilot_source_backed_adapter()),
        CaptureProvider::FactoryAiDroid => {
            Some(super::super::factory_droid_source_backed_adapter())
        }
        CaptureProvider::Qoder => Some(super::super::qoder_source_backed_adapter()),
        CaptureProvider::QwenCode => Some(super::super::qwen_code_source_backed_adapter()),
        CaptureProvider::Tabnine => Some(super::super::tabnine_source_backed_adapter()),
        CaptureProvider::Windsurf => Some(super::super::windsurf_source_backed_adapter()),
        _ => None,
    }
}

fn capture(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    terminal_evidence: &DirectJsonlTerminalEvidenceSet,
    route_sources: &Mutex<Option<HashMap<[u8; 32], SourceKey>>>,
    sink: &mut SourceBackedGenerationSink<'_>,
    #[cfg(test)] test_observer: Option<&DirectJsonlRegistrationTestObserver>,
) -> SourceBackedRouteResult<()> {
    terminal_evidence.reset().map_err(route_error)?;
    let inventory = adapter.discover(root).map_err(route_error)?;
    if inventory.root_missing() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "direct JSONL route root is temporarily unavailable",
        ));
    }
    if !inventory.failures().is_empty() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "direct JSONL inventory contains inaccessible sources",
        ));
    }
    let (base_sources, base_sources_by_path) = indexed_base_sources(adapter, root, sink);
    let mut owned_sources = base_sources
        .iter()
        .map(|base| {
            (
                base.observation().source().exact_descriptor_digest(),
                base.observation().source().clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    for removal in source_backed_base_removals(sink) {
        if adapter.deletion_belongs_to_root(root, removal.deletion()) {
            owned_sources.insert(
                removal.source().exact_descriptor_digest(),
                removal.source().clone(),
            );
        }
    }
    let scanned = scan_leaves(
        adapter,
        inventory.leaves(),
        &base_sources,
        &base_sources_by_path,
        &mut owned_sources,
        sink,
        #[cfg(test)]
        test_observer,
    )?;
    let mut sources = Vec::with_capacity(scanned.receipts.len());
    let mut current_sources =
        HashMap::<[u8; 32], SourceKey>::with_capacity(inventory.leaves().len());
    for (path, _) in &scanned.admission_rejections {
        terminal_evidence
            .record_rejected_path(path.clone())
            .map_err(route_error)?;
    }
    for receipt in scanned.receipts {
        terminal_evidence
            .record(receipt.terminal_evidence())
            .map_err(route_error)?;
        let source = receipt.source().clone();
        current_sources
            .entry(source.exact_descriptor_digest())
            .or_insert_with(|| source.clone());
        #[cfg(test)]
        record_lifecycle_work(0, 0, 0, 1, 0);
        sources.push(source);
    }
    if sources.is_empty() && !scanned.admission_rejections.is_empty() {
        let rejected_records = scanned
            .admission_rejections
            .iter()
            .map(|(_, rejections)| rejections.len())
            .sum::<usize>();
        let (path, rejections) = &scanned.admission_rejections[0];
        let first_reason = rejections
            .first()
            .map(|rejection| rejection.reason.as_str())
            .unwrap_or("provider-native session identity was rejected");
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            format!(
                "direct JSONL route rejected {rejected_records} records across {} sources; first rejection in {}: {first_reason}",
                scanned.admission_rejections.len(),
                path.display()
            ),
        ));
    }
    let closing = adapter.discover(root).map_err(route_error)?;
    let certified_inventory = inventory
        .certify_against(&closing, sources)
        .map_err(route_error)?;
    sink.certify_complete_inventory(certified_inventory.clone())
        .map_err(route_coordinator_error)?;
    for base in base_sources {
        #[cfg(test)]
        record_lifecycle_work(0, 0, 0, 0, 1);
        let base_source = base.observation().source();
        let remains_current = current_sources
            .get(&base_source.exact_descriptor_digest())
            .is_some_and(|current| current.exact_descriptor_eq(base_source));
        if !remains_current {
            let deletion = ctx_history_core::CertifiedSourceDeletion::from_inventory(
                base_source.clone(),
                &certified_inventory,
            )
            .map_err(route_error)?;
            sink.delete_source(deletion, certified_inventory.clone())
                .map_err(route_coordinator_error)?;
        }
    }
    *route_sources
        .lock()
        .map_err(|_| route_error("direct JSONL route source lock was poisoned"))? =
        Some(owned_sources);
    Ok(())
}

struct DirectJsonlScanBatch {
    receipts: Vec<DirectJsonlScanReceipt>,
    admission_rejections: Vec<(PathBuf, Vec<super::super::DirectJsonlRejection>)>,
}

struct DirectJsonlLeafJob {
    leaf: DirectJsonlInventoryLeaf,
    base: Option<CertifiedSource>,
}

enum DirectJsonlLeafResult {
    Scanned(Box<DirectJsonlScanReceipt>),
    Rejected {
        path: PathBuf,
        rejections: Vec<super::super::DirectJsonlRejection>,
    },
}

fn scan_leaves(
    adapter: DirectJsonlSourceAdapter,
    leaves: &[DirectJsonlInventoryLeaf],
    base_sources: &[CertifiedSource],
    base_sources_by_path: &HashMap<PathBuf, (usize, DirectJsonlCheckpoint)>,
    owned_sources: &mut HashMap<[u8; 32], SourceKey>,
    sink: &mut SourceBackedGenerationSink<'_>,
    #[cfg(test)] test_observer: Option<&DirectJsonlRegistrationTestObserver>,
) -> SourceBackedRouteResult<DirectJsonlScanBatch> {
    let recommended = sink.recommended_leaf_workers(leaves.len());
    let worker_count = direct_jsonl_scanner_worker_count(recommended, leaves.len());
    if leaves.is_empty() {
        #[cfg(test)]
        record_scanner_activity(worker_count, None);
        return Ok(DirectJsonlScanBatch {
            receipts: Vec::new(),
            admission_rejections: Vec::new(),
        });
    }
    let jobs = leaves
        .iter()
        .map(|leaf| DirectJsonlLeafJob {
            leaf: leaf.clone(),
            base: base_for_leaf(leaf, base_sources, base_sources_by_path)
                .map(|(base, _checkpoint)| base.clone()),
        })
        .collect::<Vec<_>>();

    #[cfg(test)]
    let scanner_probe = direct_jsonl_scanner_probe(worker_count);
    let scan_result =
        sink.run_parallel_leaf_scans_discovering_sources(jobs, worker_count, |job, emitter| {
            #[cfg(test)]
            let _active_scanner = scanner_probe.as_ref().map(|probe| probe.enter());
            scan_parallel_leaf(
                adapter,
                job,
                emitter,
                #[cfg(test)]
                test_observer,
            )
        });
    #[cfg(test)]
    record_scanner_activity(worker_count, scanner_probe.as_deref());
    let results = scan_result.map_err(map_parallel_leaf_error)?;
    let mut receipts = Vec::with_capacity(results.len());
    let mut admission_rejections = Vec::new();
    for result in results {
        match result {
            DirectJsonlLeafResult::Scanned(receipt) => {
                register_owned_source(owned_sources, receipt.source())?;
                receipts.push(*receipt);
            }
            DirectJsonlLeafResult::Rejected { path, rejections } => {
                admission_rejections.push((path, rejections));
            }
        }
    }
    Ok(DirectJsonlScanBatch {
        receipts,
        admission_rejections,
    })
}

fn scan_parallel_leaf(
    adapter: DirectJsonlSourceAdapter,
    job: &DirectJsonlLeafJob,
    emitter: &mut ParallelLeafScanEmitter<'_, DirectJsonlLeafResult, SourceBackedRouteError>,
    #[cfg(test)] test_observer: Option<&DirectJsonlRegistrationTestObserver>,
) -> Result<(), ParallelLeafScanWorkerError<SourceBackedRouteError>> {
    let mut reader =
        match adapter.open_leaf(&job.leaf, DateTime::<Utc>::UNIX_EPOCH, job.base.as_ref()) {
            Ok(reader) => reader,
            Err(DirectJsonlSourceBackedError::RejectedSource { path, rejections })
                if job.base.is_none() =>
            {
                emitter.complete(ParallelLeafScanComplete::skipped(
                    DirectJsonlLeafResult::Rejected { path, rejections },
                ))?;
                return Ok(());
            }
            Err(error) => {
                return Err(ParallelLeafScanWorkerError::provider(route_error(error)));
            }
        };
    let source = reader.source().clone();
    match reader.disposition() {
        DirectJsonlDisposition::Unchanged | DirectJsonlDisposition::Append => {
            #[cfg(test)]
            notify_test_observer(
                test_observer,
                DirectJsonlRegistrationTestEvent::BeginSourceAppend,
            );
            let base = job.base.clone().ok_or_else(|| {
                ParallelLeafScanWorkerError::provider(route_error(
                    DirectJsonlSourceBackedError::CountMismatch,
                ))
            })?;
            emitter.begin(ParallelLeafScanBegin::append(source, base))?;
        }
        DirectJsonlDisposition::Cold | DirectJsonlDisposition::Replace => {
            #[cfg(test)]
            notify_test_observer(test_observer, DirectJsonlRegistrationTestEvent::BeginSource);
            emitter.begin(ParallelLeafScanBegin::replace(source))?;
        }
    }

    let mut cancellation = None::<ParallelLeafScanCancelled>;
    let visited = reader.visit_documents(&mut |document| {
        emitter.emit_document(document).map_err(|error| {
            cancellation = Some(error);
            DirectJsonlSourceBackedError::Publication(
                "direct JSONL parallel scan was cancelled".to_owned(),
            )
        })
    });
    if let Some(cancellation) = cancellation {
        return Err(cancellation.into());
    }
    visited
        .map_err(route_error)
        .map_err(ParallelLeafScanWorkerError::provider)?;
    let receipt = reader
        .finish()
        .map_err(route_error)
        .map_err(ParallelLeafScanWorkerError::provider)?;
    validate_scan_receipt(&receipt).map_err(ParallelLeafScanWorkerError::provider)?;
    if let Some(append) = receipt.append().cloned() {
        emitter.complete(ParallelLeafScanComplete::append(
            append,
            DirectJsonlLeafResult::Scanned(Box::new(receipt)),
        ))?;
    } else {
        emitter.complete(ParallelLeafScanComplete::replace(
            receipt.certificate().clone(),
            DirectJsonlLeafResult::Scanned(Box::new(receipt)),
        ))?;
    }
    Ok(())
}

fn validate_scan_receipt(receipt: &DirectJsonlScanReceipt) -> SourceBackedRouteResult<()> {
    if u64::try_from(receipt.rejections().len()).map_or(true, |details| {
        details > receipt.certificate().counts().rejected_records
    }) {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "direct JSONL typed rejection details exceed the certified rejection count",
        ));
    }
    Ok(())
}

fn base_for_leaf<'a>(
    leaf: &DirectJsonlInventoryLeaf,
    base_sources: &'a [CertifiedSource],
    base_sources_by_path: &'a HashMap<PathBuf, (usize, DirectJsonlCheckpoint)>,
) -> Option<(&'a CertifiedSource, &'a DirectJsonlCheckpoint)> {
    #[cfg(test)]
    record_lifecycle_work(0, 0, 1, 0, 0);
    base_sources_by_path
        .get(&leaf.path)
        .map(|(index, checkpoint)| (&base_sources[*index], checkpoint))
}

fn register_owned_source(
    owned_sources: &mut HashMap<[u8; 32], SourceKey>,
    source: &SourceKey,
) -> SourceBackedRouteResult<()> {
    if owned_sources
        .insert(source.exact_descriptor_digest(), source.clone())
        .is_some_and(|previous| !previous.exact_descriptor_eq(source))
    {
        return Err(route_error(
            "direct JSONL route source descriptor digest collision",
        ));
    }
    Ok(())
}

fn map_parallel_leaf_error(
    error: ParallelLeafScanError<SourceBackedRouteError>,
) -> SourceBackedRouteError {
    match error {
        ParallelLeafScanError::Worker { source, .. } => source,
        ParallelLeafScanError::Sink {
            source:
                SourceBackedCoordinatorError::Index(ctx_history_index::IndexError::DuplicateSource(_)),
            ..
        } => SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::InvalidSource,
            "direct JSONL inventory resolved multiple leaves to one exact source",
        ),
        other => {
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, other.to_string())
        }
    }
}

pub(super) fn direct_jsonl_scanner_worker_count_policy(
    recommended: usize,
    leaf_count: usize,
    requested_workers: Option<usize>,
) -> usize {
    requested_workers
        .map_or(recommended, |workers| {
            workers.clamp(1, DIRECT_JSONL_MAX_SCANNER_WORKERS)
        })
        .min(leaf_count)
}

fn direct_jsonl_scanner_worker_count(recommended: usize, leaf_count: usize) -> usize {
    #[cfg(test)]
    {
        DIRECT_JSONL_SCANNER_WORKERS_OVERRIDE.with(|workers| {
            direct_jsonl_scanner_worker_count_policy(recommended, leaf_count, workers.get())
        })
    }
    #[cfg(not(test))]
    {
        direct_jsonl_scanner_worker_count_policy(recommended, leaf_count, None)
    }
}

#[cfg(test)]
fn record_scanner_activity(worker_count: usize, probe: Option<&DirectJsonlScannerProbe>) {
    DIRECT_JSONL_SCANNER_ACTIVITY.set(
        probe.map_or_else(DirectJsonlScannerActivity::default, |probe| {
            probe.snapshot(worker_count)
        }),
    );
}

#[cfg(test)]
struct DirectJsonlScannerProbe {
    active: AtomicUsize,
    peak: AtomicUsize,
    arrivals: AtomicUsize,
    sources_started: AtomicUsize,
    sources_completed: AtomicUsize,
    rendezvous_target: usize,
    rendezvous: Barrier,
}

#[cfg(test)]
impl DirectJsonlScannerProbe {
    fn enter(&self) -> DirectJsonlActiveScanner<'_> {
        self.sources_started.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        self.peak.fetch_max(active, Ordering::SeqCst);
        if self.arrivals.fetch_add(1, Ordering::SeqCst) < self.rendezvous_target {
            self.rendezvous.wait();
        }
        DirectJsonlActiveScanner { probe: self }
    }

    fn snapshot(&self, worker_count: usize) -> DirectJsonlScannerActivity {
        debug_assert_eq!(self.active.load(Ordering::SeqCst), 0);
        DirectJsonlScannerActivity {
            worker_count,
            sources_started: self.sources_started.load(Ordering::SeqCst),
            sources_completed: self.sources_completed.load(Ordering::SeqCst),
            peak_active_scanners: self.peak.load(Ordering::SeqCst),
        }
    }
}

#[cfg(test)]
struct DirectJsonlActiveScanner<'probe> {
    probe: &'probe DirectJsonlScannerProbe,
}

#[cfg(test)]
impl Drop for DirectJsonlActiveScanner<'_> {
    fn drop(&mut self) {
        self.probe.sources_completed.fetch_add(1, Ordering::SeqCst);
        self.probe.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
fn direct_jsonl_scanner_probe(worker_count: usize) -> Option<Arc<DirectJsonlScannerProbe>> {
    DIRECT_JSONL_SCANNER_WORKERS_OVERRIDE
        .with(std::cell::Cell::get)
        .map(|_| {
            let rendezvous_target = worker_count.clamp(1, 4);
            Arc::new(DirectJsonlScannerProbe {
                active: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                arrivals: AtomicUsize::new(0),
                sources_started: AtomicUsize::new(0),
                sources_completed: AtomicUsize::new(0),
                rendezvous_target,
                rendezvous: Barrier::new(rendezvous_target),
            })
        })
}

fn indexed_base_sources(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    sink: &SourceBackedGenerationSink<'_>,
) -> (
    Vec<CertifiedSource>,
    HashMap<PathBuf, (usize, DirectJsonlCheckpoint)>,
) {
    let candidates = source_backed_base_sources(sink, |source| adapter.owns(source));
    let mut base_sources = Vec::with_capacity(candidates.len());
    let mut base_sources_by_path = HashMap::with_capacity(candidates.len());
    for source in candidates {
        #[cfg(test)]
        record_lifecycle_work(1, 0, 0, 0, 0);
        let Ok(checkpoint) = decode_certificate(adapter, &source) else {
            continue;
        };
        let path = checkpoint.physical.identity().source_path().clone();
        if !path.starts_with(root) {
            continue;
        }
        let index = base_sources.len();
        base_sources_by_path
            .entry(path)
            .or_insert((index, checkpoint));
        base_sources.push(source);
        #[cfg(test)]
        record_lifecycle_work(0, 1, 0, 0, 0);
    }
    (base_sources, base_sources_by_path)
}

fn revalidate_target(
    adapter: DirectJsonlSourceAdapter,
    root: &Path,
    terminal_evidence: &DirectJsonlTerminalEvidenceSet,
    target: SourceBackedRevalidationTarget<'_>,
) -> super::DirectJsonlSourceBackedResult<bool> {
    match target {
        SourceBackedRevalidationTarget::Source(expected) => {
            adapter.revalidate_certificate(terminal_evidence, expected)
        }
        SourceBackedRevalidationTarget::Deletion(deletion) => {
            adapter.revalidate_deletion(root, deletion)
        }
    }
}

#[cfg(test)]
fn notify_test_observer(
    observer: Option<&DirectJsonlRegistrationTestObserver>,
    event: DirectJsonlRegistrationTestEvent,
) {
    if let Some(observer) = observer {
        observer(event);
    }
}
