use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;

#[cfg(test)]
use std::sync::{Barrier, Mutex, OnceLock};

use crate::graph::segment::{
    CompactIndexedCoreEventState, EventIndexSource, EventLineageTables, IndexedCoreEventTombstone,
    SegmentRef,
};

use super::super::super::SegmentMaterializerError;
use super::super::super::model::MAX_PUBLICATION_IN_FLIGHT_BYTES;
use super::super::io::{
    CompletedSegmentPublication, PlannedSegmentWrite, write_planned_event_index_segment,
    write_planned_flat_segment,
};

pub(super) fn publication_worker_limit(_root: &Path) -> Result<usize, SegmentMaterializerError> {
    #[cfg(test)]
    if let Some(hook) = publication_test_hook(_root) {
        return Ok(hook.worker_limit);
    }
    crate::worker_budget::default_provider_worker_budget()
        .map(|budget| budget.finish_workers().max(1))
        .map_err(|_| SegmentMaterializerError::Corrupt("provider worker budget is unavailable"))
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicationTransactionTestFault {
    BeforeManifestActivation,
    CancelBeforeManifestActivation,
}

#[cfg(test)]
pub(super) struct PublicationTestHook {
    pub(super) worker_limit: usize,
    rendezvous_ordinals: BTreeSet<u32>,
    rendezvous: Option<Barrier>,
    fail_ordinal: Option<u32>,
    pub(super) transaction_fault: Option<PublicationTransactionTestFault>,
    pub(super) pre_activation_reached: std::sync::atomic::AtomicBool,
    workers_entered: AtomicUsize,
    workers_exited: AtomicUsize,
    active_workers: AtomicUsize,
    pub(super) peak_workers: AtomicUsize,
    pub(super) cleanup_root_syncs: AtomicUsize,
    pub(super) segment_writes_started: AtomicUsize,
}

#[cfg(test)]
impl PublicationTestHook {
    fn enter(self: &Arc<Self>) -> PublicationTestWorkerGuard {
        self.workers_entered.fetch_add(1, Ordering::SeqCst);
        let active = self.active_workers.fetch_add(1, Ordering::SeqCst) + 1;
        let mut peak = self.peak_workers.load(Ordering::SeqCst);
        while active > peak {
            match self.peak_workers.compare_exchange(
                peak,
                active,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(observed) => peak = observed,
            }
        }
        PublicationTestWorkerGuard {
            hook: Arc::clone(self),
        }
    }

    fn before_job(&self, ordinal: u32) -> Result<(), SegmentMaterializerError> {
        if self.rendezvous_ordinals.contains(&ordinal) {
            self.rendezvous
                .as_ref()
                .ok_or(SegmentMaterializerError::Corrupt(
                    "publication test rendezvous is unavailable",
                ))?
                .wait();
        }
        if self.fail_ordinal == Some(ordinal) {
            return Err(SegmentMaterializerError::Corrupt(
                "injected publication worker failure",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
struct PublicationTestWorkerGuard {
    hook: Arc<PublicationTestHook>,
}

#[cfg(test)]
impl Drop for PublicationTestWorkerGuard {
    fn drop(&mut self) {
        self.hook.active_workers.fetch_sub(1, Ordering::SeqCst);
        self.hook.workers_exited.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PublicationTestWorkerReceipt {
    pub(crate) workers_entered: usize,
    pub(crate) workers_exited: usize,
    pub(crate) active_workers: usize,
    pub(crate) peak_workers: usize,
    pub(crate) cleanup_root_syncs: usize,
    pub(crate) segment_writes_started: usize,
}

#[cfg(test)]
static PUBLICATION_TEST_HOOKS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<PublicationTestHook>>>> =
    OnceLock::new();

#[cfg(test)]
pub(crate) struct PublicationTestHookGuard {
    root: PathBuf,
}

#[cfg(test)]
impl Drop for PublicationTestHookGuard {
    fn drop(&mut self) {
        if let Some(hooks) = PUBLICATION_TEST_HOOKS.get() {
            hooks
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&self.root);
        }
    }
}

#[cfg(test)]
impl PublicationTestHookGuard {
    pub(crate) fn cancellation_requested(&self) -> bool {
        publication_test_hook(&self.root)
            .is_some_and(|hook| hook.pre_activation_reached.load(Ordering::SeqCst))
    }
    pub(crate) fn worker_receipt(
        &self,
    ) -> Result<PublicationTestWorkerReceipt, SegmentMaterializerError> {
        let hook = publication_test_hook(&self.root).ok_or(SegmentMaterializerError::Corrupt(
            "publication test hook disappeared",
        ))?;
        Ok(PublicationTestWorkerReceipt {
            workers_entered: hook.workers_entered.load(Ordering::SeqCst),
            workers_exited: hook.workers_exited.load(Ordering::SeqCst),
            active_workers: hook.active_workers.load(Ordering::SeqCst),
            peak_workers: hook.peak_workers.load(Ordering::SeqCst),
            cleanup_root_syncs: hook.cleanup_root_syncs.load(Ordering::SeqCst),
            segment_writes_started: hook.segment_writes_started.load(Ordering::SeqCst),
        })
    }
}

#[cfg(test)]
pub(crate) fn install_publication_test_hook(
    root: &Path,
    worker_limit: usize,
    rendezvous_ordinals: BTreeSet<u32>,
    fail_ordinal: Option<u32>,
) -> Result<PublicationTestHookGuard, SegmentMaterializerError> {
    install_publication_test_hook_inner(root, worker_limit, rendezvous_ordinals, fail_ordinal, None)
}

#[cfg(test)]
pub(crate) fn install_publication_transaction_failure_test_hook(
    root: &Path,
    fault: PublicationTransactionTestFault,
) -> Result<PublicationTestHookGuard, SegmentMaterializerError> {
    install_publication_test_hook_inner(root, 2, BTreeSet::new(), None, Some(fault))
}

#[cfg(test)]
fn install_publication_test_hook_inner(
    root: &Path,
    worker_limit: usize,
    rendezvous_ordinals: BTreeSet<u32>,
    fail_ordinal: Option<u32>,
    transaction_fault: Option<PublicationTransactionTestFault>,
) -> Result<PublicationTestHookGuard, SegmentMaterializerError> {
    if worker_limit == 0
        || worker_limit > crate::worker_budget::MAX_PUBLICATION_WORKERS
        || rendezvous_ordinals.len() > worker_limit
    {
        return Err(SegmentMaterializerError::Bounds);
    }
    let rendezvous = if rendezvous_ordinals.is_empty() {
        None
    } else {
        Some(Barrier::new(rendezvous_ordinals.len()))
    };
    let root = root.to_owned();
    let mut hooks = PUBLICATION_TEST_HOOKS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if hooks.contains_key(&root) {
        return Err(SegmentMaterializerError::Busy);
    }
    hooks.insert(
        root.clone(),
        Arc::new(PublicationTestHook {
            worker_limit,
            rendezvous_ordinals,
            rendezvous,
            fail_ordinal,
            transaction_fault,
            pre_activation_reached: std::sync::atomic::AtomicBool::new(false),
            workers_entered: AtomicUsize::new(0),
            workers_exited: AtomicUsize::new(0),
            active_workers: AtomicUsize::new(0),
            peak_workers: AtomicUsize::new(0),
            cleanup_root_syncs: AtomicUsize::new(0),
            segment_writes_started: AtomicUsize::new(0),
        }),
    );
    Ok(PublicationTestHookGuard { root })
}

#[cfg(test)]
pub(super) fn publication_test_hook(root: &Path) -> Option<Arc<PublicationTestHook>> {
    PUBLICATION_TEST_HOOKS.get().and_then(|hooks| {
        hooks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(root)
            .cloned()
    })
}

#[derive(Clone)]
pub(super) enum PublicationJobKind {
    Flat {
        records: Vec<crate::graph::segment::ServingRecord>,
        tombstones: Vec<crate::graph::segment::EventTombstone>,
    },
    EventIndex {
        sources: Vec<EventIndexSource>,
        records: Vec<CompactIndexedCoreEventState>,
        tombstones: Vec<IndexedCoreEventTombstone>,
        lineage: EventLineageTables,
    },
}

#[derive(Clone)]
pub(super) struct PublicationJob {
    pub(super) plan: PlannedSegmentWrite,
    pub(super) accounted_bytes: usize,
    pub(super) kind: PublicationJobKind,
}

impl PublicationJob {
    fn execute(self, root: &Path) -> Result<CompletedSegmentPublication, SegmentMaterializerError> {
        match self.kind {
            PublicationJobKind::Flat {
                records,
                tombstones,
            } => write_planned_flat_segment(root, self.plan, records, tombstones),
            PublicationJobKind::EventIndex {
                sources,
                records,
                tombstones,
                lineage,
            } => write_planned_event_index_segment(
                root, self.plan, sources, records, tombstones, lineage,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PublicationPipelineMetrics {
    pub(super) worker_limit: usize,
    pub(super) jobs_started: usize,
    pub(super) jobs_completed: usize,
    pub(super) peak_workers: usize,
    pub(super) in_flight_byte_limit: usize,
    pub(super) peak_in_flight_bytes: usize,
    pub(super) checked_readbacks: usize,
}

struct PublicationWorkerActivity {
    active: AtomicUsize,
    peak: AtomicUsize,
}

impl PublicationWorkerActivity {
    fn enter(self: &Arc<Self>) -> PublicationWorkerGuard {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let mut peak = self.peak.load(Ordering::SeqCst);
        while active > peak {
            match self
                .peak
                .compare_exchange(peak, active, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => break,
                Err(observed) => peak = observed,
            }
        }
        PublicationWorkerGuard {
            activity: Arc::clone(self),
        }
    }
}

struct PublicationWorkerGuard {
    activity: Arc<PublicationWorkerActivity>,
}

impl Drop for PublicationWorkerGuard {
    fn drop(&mut self) {
        self.activity.active.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) struct InFlightPublicationJob {
    ordinal: u32,
    accounted_bytes: usize,
    handle: JoinHandle<Result<CompletedSegmentPublication, SegmentMaterializerError>>,
}

pub(super) struct PublicationPipeline {
    root: Arc<PathBuf>,

    pub(super) worker_limit: usize,
    pub(super) in_flight_byte_limit: usize,
    pub(super) in_flight_bytes: usize,
    pub(super) peak_in_flight_bytes: usize,
    pub(super) in_flight: Vec<InFlightPublicationJob>,
    completed: BTreeMap<u32, SegmentRef>,
    pending: BTreeSet<u32>,
    errors: BTreeMap<u32, SegmentMaterializerError>,
    pub(super) jobs_started: usize,
    pub(super) jobs_completed: usize,
    pub(super) checked_readbacks: usize,
    activity: Arc<PublicationWorkerActivity>,
    #[cfg(test)]
    pub(super) hook: Option<Arc<PublicationTestHook>>,
}

impl PublicationPipeline {
    pub(super) fn new(root: &Path, worker_limit: usize) -> Result<Self, SegmentMaterializerError> {
        if worker_limit == 0 || worker_limit > crate::worker_budget::MAX_PUBLICATION_WORKERS {
            return Err(SegmentMaterializerError::Bounds);
        }
        Ok(Self {
            root: Arc::new(root.to_owned()),

            worker_limit,
            in_flight_byte_limit: MAX_PUBLICATION_IN_FLIGHT_BYTES,
            in_flight_bytes: 0,
            peak_in_flight_bytes: 0,
            in_flight: Vec::new(),
            completed: BTreeMap::new(),
            pending: BTreeSet::new(),
            errors: BTreeMap::new(),
            jobs_started: 0,
            jobs_completed: 0,
            checked_readbacks: 0,
            activity: Arc::new(PublicationWorkerActivity {
                active: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
            }),
            #[cfg(test)]
            hook: publication_test_hook(root),
        })
    }

    pub(super) fn dispatch(&mut self, job: PublicationJob) -> Result<(), SegmentMaterializerError> {
        let ordinal = job.plan.ordinal();
        self.reap_finished();
        if job.accounted_bytes == 0 || job.accounted_bytes > self.in_flight_byte_limit {
            return self.reject_dispatch(ordinal, SegmentMaterializerError::Bounds);
        }
        self.fail_if_needed()?;
        while self.in_flight.len() >= self.worker_limit
            || self
                .in_flight_bytes
                .checked_add(job.accounted_bytes)
                .is_none_or(|bytes| bytes > self.in_flight_byte_limit)
        {
            self.join_one(0);
            self.fail_if_needed()?;
        }
        if !self.pending.insert(ordinal) {
            return self.reject_dispatch(
                ordinal,
                SegmentMaterializerError::Corrupt("publication job ordinal is duplicated"),
            );
        }
        let root = Arc::clone(&self.root);

        let activity = Arc::clone(&self.activity);
        #[cfg(test)]
        let hook = self.hook.clone();
        let accounted_bytes = job.accounted_bytes;
        let jobs_started = match self.jobs_started.checked_add(1) {
            Some(jobs_started) => jobs_started,
            None => return self.reject_dispatch(ordinal, SegmentMaterializerError::Bounds),
        };
        let in_flight_bytes = match self.in_flight_bytes.checked_add(accounted_bytes) {
            Some(in_flight_bytes) if in_flight_bytes <= self.in_flight_byte_limit => {
                in_flight_bytes
            }
            _ => return self.reject_dispatch(ordinal, SegmentMaterializerError::Bounds),
        };
        let error_path = Arc::clone(&root);
        let handle = match std::thread::Builder::new()
            .name(format!("ctx-pro-publish-{ordinal}"))
            .spawn(move || {
                let job = job;
                let _worker = activity.enter();
                #[cfg(test)]
                let _test_worker = hook.as_ref().map(PublicationTestHook::enter);
                #[cfg(test)]
                if let Some(hook) = hook.as_ref() {
                    hook.before_job(ordinal)?;
                }
                job.execute(root.as_path())
            }) {
            Ok(handle) => handle,
            Err(source) => {
                return self.reject_dispatch(
                    ordinal,
                    SegmentMaterializerError::Io {
                        operation: "spawn publication worker for",
                        path: error_path.as_ref().clone(),
                        source,
                    },
                );
            }
        };
        self.jobs_started = jobs_started;
        self.in_flight_bytes = in_flight_bytes;
        self.peak_in_flight_bytes = self.peak_in_flight_bytes.max(self.in_flight_bytes);
        self.in_flight.push(InFlightPublicationJob {
            ordinal,
            accounted_bytes,
            handle,
        });
        Ok(())
    }

    fn reject_dispatch(
        &mut self,
        ordinal: u32,
        error: SegmentMaterializerError,
    ) -> Result<(), SegmentMaterializerError> {
        self.errors.entry(ordinal).or_insert(error);
        self.fail_if_needed()
    }

    pub(super) fn drain_pending(
        &mut self,
        references: &mut [SegmentRef],
    ) -> Result<(), SegmentMaterializerError> {
        self.drain();
        self.fail_if_needed()?;
        let pending = std::mem::take(&mut self.pending);
        for ordinal in pending {
            let index = usize::try_from(ordinal).map_err(|_| SegmentMaterializerError::Bounds)?;
            let completed =
                self.completed
                    .remove(&ordinal)
                    .ok_or(SegmentMaterializerError::Corrupt(
                        "publication worker omitted an ordered reference",
                    ))?;
            let placeholder =
                references
                    .get_mut(index)
                    .ok_or(SegmentMaterializerError::Corrupt(
                        "publication reference ordinal is outside the manifest",
                    ))?;
            if placeholder.ordinal != ordinal
                || placeholder.generation_id != completed.generation_id
                || placeholder.role != completed.role
                || placeholder.file_name != completed.file_name
            {
                return Err(SegmentMaterializerError::Corrupt(
                    "publication worker changed its predetermined reference",
                ));
            }
            *placeholder = completed;
        }
        if !self.completed.is_empty()
            || self.jobs_started != self.jobs_completed
            || self.jobs_completed != self.checked_readbacks
        {
            return Err(SegmentMaterializerError::Corrupt(
                "publication worker completion accounting is inconsistent",
            ));
        }
        Ok(())
    }

    pub(super) fn metrics(&self) -> Result<PublicationPipelineMetrics, SegmentMaterializerError> {
        if !self.in_flight.is_empty()
            || self.in_flight_bytes != 0
            || !self.completed.is_empty()
            || !self.pending.is_empty()
            || !self.errors.is_empty()
            || self.activity.active.load(Ordering::SeqCst) != 0
            || self.jobs_started != self.jobs_completed
            || self.jobs_completed != self.checked_readbacks
        {
            return Err(SegmentMaterializerError::Corrupt(
                "publication metrics requested before the pipeline drained",
            ));
        }
        Ok(PublicationPipelineMetrics {
            worker_limit: self.worker_limit,
            jobs_started: self.jobs_started,
            jobs_completed: self.jobs_completed,
            peak_workers: self.activity.peak.load(Ordering::SeqCst),
            in_flight_byte_limit: self.in_flight_byte_limit,
            peak_in_flight_bytes: self.peak_in_flight_bytes,
            checked_readbacks: self.checked_readbacks,
        })
    }

    fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.in_flight.len() {
            if self.in_flight[index].handle.is_finished() {
                self.join_one(index);
            } else {
                index += 1;
            }
        }
    }

    pub(super) fn drain(&mut self) {
        while !self.in_flight.is_empty() {
            self.join_one(0);
        }
    }

    fn join_one(&mut self, index: usize) {
        let job = self.in_flight.remove(index);
        self.in_flight_bytes = self.in_flight_bytes.saturating_sub(job.accounted_bytes);
        match job.handle.join() {
            Ok(Ok(completed)) => {
                if completed.reference.ordinal != job.ordinal
                    || completed.checked_readbacks != 1
                    || self
                        .completed
                        .insert(job.ordinal, completed.reference)
                        .is_some()
                {
                    self.errors
                        .entry(job.ordinal)
                        .or_insert(SegmentMaterializerError::Corrupt(
                            "publication worker returned inconsistent completion",
                        ));
                    return;
                }
                self.jobs_completed = self.jobs_completed.saturating_add(1);
                self.checked_readbacks = self
                    .checked_readbacks
                    .saturating_add(completed.checked_readbacks as usize);
            }
            Ok(Err(error)) => {
                self.errors.entry(job.ordinal).or_insert(error);
            }
            Err(_) => {
                self.errors
                    .entry(job.ordinal)
                    .or_insert(SegmentMaterializerError::Corrupt(
                        "publication worker terminated",
                    ));
            }
        }
    }

    fn fail_if_needed(&mut self) -> Result<(), SegmentMaterializerError> {
        if self.errors.is_empty() {
            return Ok(());
        }
        self.drain();
        let ordinal = *self
            .errors
            .keys()
            .next()
            .ok_or(SegmentMaterializerError::Corrupt(
                "publication worker error was lost",
            ))?;
        Err(self
            .errors
            .remove(&ordinal)
            .ok_or(SegmentMaterializerError::Corrupt(
                "publication worker error was lost",
            ))?)
    }
}

impl Drop for PublicationPipeline {
    fn drop(&mut self) {
        self.drain();
    }
}
