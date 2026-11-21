#[cfg(test)]
use std::cell::Cell;

use std::{
    error::Error as StdError,
    io,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
        Arc,
    },
    thread,
    time::Duration,
};

use super::{SourceBackedGenerationSink, SourceBackedSourceOutcome};
use crate::{CaptureLifecycleSink, CorePreparationPort};
use ctx_history_core::SourceKey;

mod protocol;

#[cfg(test)]
use protocol::ParallelLeafProtocolMessage;
use protocol::{
    apply_parallel_leaf_message, finalize_parallel_leaf_diagnostics, state_mut, validate_worker,
    ParallelLeafJobState, ParallelLeafWorkerEvent,
};
#[allow(unused_imports)]
pub use protocol::{
    ParallelLeafScanBegin, ParallelLeafScanCancelled, ParallelLeafScanComplete,
    ParallelLeafScanEmitError, ParallelLeafScanEmitter, ParallelLeafScanError, ParallelLeafScanJob,
    ParallelLeafScanMessageKind, ParallelLeafScanMode, ParallelLeafScanProtocolError,
    ParallelLeafScanWorkerError, ParallelLeafSinkOperation,
};

const MAX_PARALLEL_LEAF_WORKERS: usize = 16;
const INDEXER_THREAD_CAP: usize = 8;
const RUNTIME_THREAD_RESERVATION: usize = 1;
const SOURCE_WORKER_THREAD_PREFIX: &str = "ctx-src-scan";
const INTERMEDIATE_ACTIVITY_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Default Tantivy writer memory budget (512 MiB) when no explicit indexing
/// memory limit is configured.
pub const DEFAULT_INDEXING_MEMORY_BYTES: usize = 512 * 1024 * 1024;
const INDEX_MEMORY_MIN_PER_THREAD_BYTES: usize = 15_000_000;
const CONSTRAINED_MEMORY_BYTES_PER_WORKER: usize = 64 * 1024 * 1024;

/// Optional user-configured memory and worker concurrency bounds for Core
/// indexing. Leaving both fields `None` preserves the existing automatic CPU
/// split, 512 MiB writer heap, and 256 MiB route output envelope.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct IndexingResourceLimits {
    /// Optional explicit indexing writer memory budget in bytes.
    pub memory_bytes: Option<usize>,
    /// Optional explicit indexing worker concurrency bound.
    pub workers: Option<usize>,
}

impl IndexingResourceLimits {
    pub const fn new(memory_bytes: Option<usize>, workers: Option<usize>) -> Self {
        Self {
            memory_bytes,
            workers,
        }
    }

    pub const fn from_memory_mb_and_workers(
        memory_mb: Option<usize>,
        workers: Option<usize>,
    ) -> Self {
        let memory_bytes = match memory_mb {
            Some(mb) => mb.checked_mul(1024 * 1024),
            None => None,
        };
        Self {
            memory_bytes,
            workers,
        }
    }

    /// Effective writer memory budget in bytes, guaranteed to satisfy the
    /// single-thread Tantivy heap minimum.
    pub fn effective_memory_bytes(self) -> usize {
        self.memory_bytes
            .unwrap_or(DEFAULT_INDEXING_MEMORY_BYTES)
            .max(INDEX_MEMORY_MIN_PER_THREAD_BYTES)
    }

    /// Live prepared Core-record output envelope scaled proportionally to the
    /// indexing memory budget while remaining within `[16 MiB, 256 MiB]`.
    pub fn core_route_max_live_output_bytes(self) -> u64 {
        let Some(memory_bytes) = self.memory_bytes else {
            return crate::CORE_ROUTE_MAX_LIVE_OUTPUT_BYTES;
        };
        let half = u64::try_from(memory_bytes / 2).unwrap_or(u64::MAX);
        half.clamp(
            crate::CORE_ROUTE_MIN_LIVE_OUTPUT_BYTES,
            crate::CORE_ROUTE_MAX_LIVE_OUTPUT_BYTES,
        )
    }

    /// Chooses Tantivy indexer threads for the given CPU count while honoring
    /// explicit worker/memory bounds and guaranteeing the per-thread heap floor.
    pub fn indexer_threads_for_parallelism(self, available_parallelism: usize) -> usize {
        let memory_bytes = self.effective_memory_bytes();
        let max_threads_for_memory =
            (memory_bytes / INDEX_MEMORY_MIN_PER_THREAD_BYTES).clamp(1, INDEXER_THREAD_CAP);
        match self.workers {
            Some(workers) => {
                let effective_total = workers
                    .clamp(1, MAX_PARALLEL_LEAF_WORKERS)
                    .min(available_parallelism.max(1));
                (effective_total / 2)
                    .clamp(1, INDEXER_THREAD_CAP)
                    .min(max_threads_for_memory)
            }
            None => {
                let cpu_threads =
                    source_backed_refresh_indexer_threads_for_parallelism(available_parallelism);
                let constrained_cap = if memory_bytes < DEFAULT_INDEXING_MEMORY_BYTES {
                    (memory_bytes / CONSTRAINED_MEMORY_BYTES_PER_WORKER)
                        .clamp(1, INDEXER_THREAD_CAP)
                } else {
                    INDEXER_THREAD_CAP
                };
                cpu_threads.min(constrained_cap).min(max_threads_for_memory)
            }
        }
    }

    /// Chooses leaf scanner concurrency for the given indexer thread count and
    /// CPU count while honoring explicit worker and memory constraints.
    pub fn leaf_worker_budget_for_parallelism(
        self,
        indexer_threads: usize,
        available_parallelism: usize,
    ) -> usize {
        let cpu_budget = leaf_worker_budget_for_parallelism(indexer_threads, available_parallelism);
        let memory_bytes = self.effective_memory_bytes();
        let memory_cap = if memory_bytes < DEFAULT_INDEXING_MEMORY_BYTES {
            (memory_bytes / CONSTRAINED_MEMORY_BYTES_PER_WORKER).clamp(1, MAX_PARALLEL_LEAF_WORKERS)
        } else {
            MAX_PARALLEL_LEAF_WORKERS
        };
        let base_budget = cpu_budget.min(memory_cap);
        match self.workers {
            Some(workers) => {
                let effective_total = workers
                    .clamp(1, MAX_PARALLEL_LEAF_WORKERS)
                    .min(available_parallelism.max(1));
                let remaining_for_scanners = effective_total
                    .saturating_sub(indexer_threads.clamp(1, INDEXER_THREAD_CAP))
                    .clamp(1, MAX_PARALLEL_LEAF_WORKERS);
                base_budget.min(remaining_for_scanners)
            }
            None => base_budget,
        }
    }
}

#[derive(Clone)]
struct ParallelLeafWorkerContext<P: CorePreparationPort> {
    resources: super::SourceBackedRouteResources,
    core_record_preparer: P,
}

#[cfg(test)]
thread_local! {
    static INJECT_WORKER_SPAWN_FAILURE_AT: Cell<Option<usize>> = const { Cell::new(None) };
}

pub fn source_backed_refresh_work_budget(indexer_threads: usize) -> usize {
    source_backed_refresh_work_budget_with_limits(
        indexer_threads,
        IndexingResourceLimits::default(),
    )
}

pub fn source_backed_refresh_work_budget_with_limits(
    indexer_threads: usize,
    limits: IndexingResourceLimits,
) -> usize {
    let available_parallelism = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    limits.leaf_worker_budget_for_parallelism(indexer_threads, available_parallelism)
}

pub fn source_backed_refresh_indexer_threads_with_limits(limits: IndexingResourceLimits) -> usize {
    let available_parallelism = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    limits.indexer_threads_for_parallelism(available_parallelism)
}

/// Chooses one coordinated production budget for source parsing and indexing.
///
/// Source-backed refresh runs leaf scanners and Tantivy indexers at the same
/// time. Giving Tantivy every visible CPU before sizing the scanner pool left
/// ordinary hosts with one parser. Split the CPUs remaining after the caller
/// thread between the two stages, while retaining the existing indexer cap.
pub fn source_backed_refresh_indexer_threads_for_parallelism(
    available_parallelism: usize,
) -> usize {
    if available_parallelism <= 2 {
        // Keep the established low-core writer allocation. The route pipeline
        // still has one bounded scanner plus its caller, but reducing the
        // indexer allocation cannot create a second scanner worker at this
        // scale.
        return available_parallelism.clamp(1, INDEXER_THREAD_CAP);
    }
    available_parallelism
        .saturating_sub(RUNTIME_THREAD_RESERVATION)
        .checked_div(2)
        .unwrap_or(0)
        .clamp(1, INDEXER_THREAD_CAP)
}

#[cfg(test)]
pub fn source_backed_leaf_worker_budget(indexer_threads: usize) -> usize {
    source_backed_refresh_work_budget(indexer_threads)
}

fn leaf_worker_budget_for_parallelism(
    indexer_threads: usize,
    available_parallelism: usize,
) -> usize {
    let reserved = indexer_threads
        .clamp(1, INDEXER_THREAD_CAP)
        .saturating_add(RUNTIME_THREAD_RESERVATION);
    available_parallelism
        .saturating_sub(reserved)
        .clamp(1, MAX_PARALLEL_LEAF_WORKERS)
}

fn bounded_leaf_worker_count(job_count: usize, requested_workers: usize) -> usize {
    requested_workers
        .min(job_count)
        .min(MAX_PARALLEL_LEAF_WORKERS)
}

fn source_worker_thread_name(worker_index: usize) -> String {
    debug_assert!(worker_index < MAX_PARALLEL_LEAF_WORKERS);
    format!("{SOURCE_WORKER_THREAD_PREFIX}{worker_index:02}")
}

#[cfg(test)]
fn worker_spawn_failure_is_injected(worker_index: usize) -> bool {
    INJECT_WORKER_SPAWN_FAILURE_AT.with(|injected| injected.get() == Some(worker_index))
}

#[cfg(not(test))]
fn worker_spawn_failure_is_injected(_worker_index: usize) -> bool {
    false
}

impl<L: CaptureLifecycleSink> SourceBackedGenerationSink<'_, L> {
    /// Recommends the production scanner count after reserving the clamped
    /// Tantivy indexer budget and the route-protocol caller thread.
    pub fn recommended_leaf_workers(&self, leaf_count: usize) -> usize {
        leaf_count.min(self.resources.leaf_worker_budget())
    }

    /// Runs provider-owned leaf scans on scoped workers while this caller
    /// thread exclusively applies their typed protocol to the generation.
    pub fn run_parallel_leaf_scans<J, R, E, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<J>>,
        worker_count: usize,
        scan: F,
    ) -> Result<Vec<R>, ParallelLeafScanError<E, L::Error>>
    where
        J: Send,
        R: Send,
        E: StdError + Send + 'static,
        F: Fn(
                &ParallelLeafScanJob<J>,
                &mut ParallelLeafScanEmitter<'_, R, E, L::Preparation>,
            ) -> Result<(), ParallelLeafScanWorkerError<E>>
            + Sync,
    {
        let worker_count = bounded_leaf_worker_count(
            jobs.len(),
            worker_count.min(self.resources.leaf_worker_budget()),
        );
        let mut worker_states = (0..worker_count).map(|_| ()).collect::<Vec<_>>();
        self.run_parallel_leaf_scans_inner(
            jobs,
            worker_count,
            |job| Some(job.source().clone()),
            ParallelLeafScanJob::worker_affinity,
            &mut worker_states,
            |_, job, emitter| scan(job, emitter),
        )?
        .into_iter()
        .map(|outcome| match outcome {
            SourceBackedSourceOutcome::Success(result) => Ok(result),
            SourceBackedSourceOutcome::Failed(_) => Err(ParallelLeafScanError::Protocol(
                ParallelLeafScanProtocolError::UnexpectedSourceFailure,
            )),
        })
        .collect()
    }

    /// Runs source-bound scans by borrowing one persistent state slot per
    /// active worker. Callers may reuse the same bounded slots across joined
    /// dependency phases without sharing mutable state between threads.
    pub fn run_parallel_leaf_scans_with_worker_states<J, R, E, W, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<J>>,
        worker_states: &mut [W],
        scan: F,
    ) -> Result<Vec<R>, ParallelLeafScanError<E, L::Error>>
    where
        J: Send,
        R: Send,
        E: StdError + Send + 'static,
        W: Send,
        F: Fn(
                &mut W,
                &ParallelLeafScanJob<J>,
                &mut ParallelLeafScanEmitter<'_, R, E, L::Preparation>,
            ) -> Result<(), ParallelLeafScanWorkerError<E>>
            + Sync,
    {
        self.run_parallel_leaf_scans_with_worker_states_and_source_outcomes(
            jobs,
            worker_states,
            scan,
        )?
        .into_iter()
        .map(|outcome| match outcome {
            SourceBackedSourceOutcome::Success(result) => Ok(result),
            SourceBackedSourceOutcome::Failed(_) => Err(ParallelLeafScanError::Protocol(
                ParallelLeafScanProtocolError::UnexpectedSourceFailure,
            )),
        })
        .collect()
    }

    /// Runs source-bound scans with persistent worker state while preserving
    /// logical source failures as typed per-source outcomes.
    pub fn run_parallel_leaf_scans_with_worker_states_and_source_outcomes<J, R, E, W, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<J>>,
        worker_states: &mut [W],
        scan: F,
    ) -> Result<Vec<SourceBackedSourceOutcome<R>>, ParallelLeafScanError<E, L::Error>>
    where
        J: Send,
        R: Send,
        E: StdError + Send + 'static,
        W: Send,
        F: Fn(
                &mut W,
                &ParallelLeafScanJob<J>,
                &mut ParallelLeafScanEmitter<'_, R, E, L::Preparation>,
            ) -> Result<(), ParallelLeafScanWorkerError<E>>
            + Sync,
    {
        self.run_parallel_leaf_scans_inner(
            jobs,
            worker_states.len(),
            |job| Some(job.source().clone()),
            ParallelLeafScanJob::worker_affinity,
            worker_states,
            scan,
        )
    }

    pub fn run_parallel_leaf_scans_with_source_outcomes<J, R, E, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<J>>,
        worker_count: usize,
        scan: F,
    ) -> Result<Vec<SourceBackedSourceOutcome<R>>, ParallelLeafScanError<E, L::Error>>
    where
        J: Send,
        R: Send,
        E: StdError + Send + 'static,
        F: Fn(
                &ParallelLeafScanJob<J>,
                &mut ParallelLeafScanEmitter<'_, R, E, L::Preparation>,
            ) -> Result<(), ParallelLeafScanWorkerError<E>>
            + Sync,
    {
        let worker_count = bounded_leaf_worker_count(
            jobs.len(),
            worker_count.min(self.resources.leaf_worker_budget()),
        );
        let mut worker_states = (0..worker_count).map(|_| ()).collect::<Vec<_>>();
        self.run_parallel_leaf_scans_inner(
            jobs,
            worker_count,
            |job| Some(job.source().clone()),
            ParallelLeafScanJob::worker_affinity,
            &mut worker_states,
            |_, job, emitter| scan(job, emitter),
        )
    }

    /// Runs leaf scans whose exact source is discovered by the worker that
    /// opens the leaf. The first Begin message binds that source for all
    /// subsequent protocol validation, while results remain in input order.
    pub fn run_parallel_leaf_scans_discovering_sources<J, R, E, F>(
        &mut self,
        leaves: Vec<J>,
        worker_count: usize,
        scan: F,
    ) -> Result<Vec<R>, ParallelLeafScanError<E, L::Error>>
    where
        J: Send,
        R: Send,
        E: StdError + Send + 'static,
        F: Fn(
                &J,
                &mut ParallelLeafScanEmitter<'_, R, E, L::Preparation>,
            ) -> Result<(), ParallelLeafScanWorkerError<E>>
            + Sync,
    {
        let worker_count = bounded_leaf_worker_count(
            leaves.len(),
            worker_count.min(self.resources.leaf_worker_budget()),
        );
        let mut worker_states = (0..worker_count).map(|_| ()).collect::<Vec<_>>();
        self.run_parallel_leaf_scans_inner(
            leaves,
            worker_count,
            |_| None,
            |_| None,
            &mut worker_states,
            |_, leaf, emitter| scan(leaf, emitter),
        )?
        .into_iter()
        .map(|outcome| match outcome {
            SourceBackedSourceOutcome::Success(result) => Ok(result),
            SourceBackedSourceOutcome::Failed(_) => Err(ParallelLeafScanError::Protocol(
                ParallelLeafScanProtocolError::UnexpectedSourceFailure,
            )),
        })
        .collect()
    }

    fn run_parallel_leaf_scans_inner<J, R, E, W, F, S, A>(
        &mut self,
        jobs: Vec<J>,
        worker_count: usize,
        expected_source: S,
        worker_affinity: A,
        worker_states: &mut [W],
        scan: F,
    ) -> Result<Vec<SourceBackedSourceOutcome<R>>, ParallelLeafScanError<E, L::Error>>
    where
        J: Send,
        R: Send,
        E: StdError + Send + 'static,
        W: Send,
        F: Fn(
                &mut W,
                &J,
                &mut ParallelLeafScanEmitter<'_, R, E, L::Preparation>,
            ) -> Result<(), ParallelLeafScanWorkerError<E>>
            + Sync,
        S: Fn(&J) -> Option<SourceKey>,
        A: Fn(&J) -> Option<u64>,
    {
        if jobs.is_empty() {
            return Ok(Vec::new());
        }
        if worker_count == 0 {
            return Err(ParallelLeafScanError::InvalidWorkerCount {
                job_count: jobs.len(),
            });
        }

        let worker_slots = worker_count
            .min(worker_states.len())
            .min(self.resources.leaf_worker_budget())
            .min(MAX_PARALLEL_LEAF_WORKERS);
        let has_worker_affinity = jobs.iter().any(|job| worker_affinity(job).is_some());
        let worker_count = if has_worker_affinity {
            worker_slots
        } else {
            bounded_leaf_worker_count(jobs.len(), worker_slots)
        };
        if worker_count == 0 {
            return Err(ParallelLeafScanError::InvalidWorkerCount {
                job_count: jobs.len(),
            });
        }
        let worker_assignments = jobs
            .iter()
            .enumerate()
            .map(|(job_index, job)| {
                worker_affinity(job)
                    .map(|affinity| (affinity as usize) % worker_count)
                    .unwrap_or(job_index % worker_count)
            })
            .collect::<Vec<_>>();
        let mut states = jobs
            .iter()
            .enumerate()
            .map(|(job_index, job)| {
                ParallelLeafJobState::new(expected_source(job), worker_assignments[job_index])
            })
            .collect::<Vec<_>>();
        let mut results = (0..jobs.len()).map(|_| None).collect::<Vec<_>>();
        let stripes = stripe_leaf_jobs(jobs, worker_count, &worker_assignments);
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_context = ParallelLeafWorkerContext {
            resources: self
                .resources
                .clone()
                .with_scan_cancellation(Arc::clone(&cancellation)),
            core_record_preparer: self.core_record_preparer.clone(),
        };
        let initial_activity_generation = self.resources.intermediate_activity_generation();

        thread::scope(|scope| {
            // One shared rendezvous accepts whichever worker is ready without
            // buffering a Core record. Per-worker FIFO preserves each job's
            // protocol order, while result slots and the canonical manifest
            // keep scheduling order out of generation identity.
            let (sender, receiver) =
                mpsc::sync_channel::<ParallelLeafWorkerEvent<R, E, L::Preparation>>(0);
            let mut handles = Vec::with_capacity(worker_count);
            for ((worker_index, jobs), worker_state) in stripes
                .into_iter()
                .enumerate()
                .zip(worker_states.iter_mut())
            {
                let worker_sender = sender.clone();
                let worker_cancellation = Arc::clone(&cancellation);
                let worker_context = worker_context.clone();
                let scan = &scan;
                let worker_name = source_worker_thread_name(worker_index);
                let spawn = if worker_spawn_failure_is_injected(worker_index) {
                    Err(io::Error::other(
                        "injected parallel source worker spawn failure",
                    ))
                } else {
                    thread::Builder::new()
                        .name(worker_name)
                        .spawn_scoped(scope, move || {
                            run_leaf_worker(
                                worker_index,
                                jobs,
                                &worker_sender,
                                &worker_cancellation,
                                worker_context,
                                worker_state,
                                scan,
                            );
                        })
                };
                match spawn {
                    Ok(handle) => handles.push((worker_index, handle)),
                    Err(source) => {
                        cancellation.store(true, Ordering::Release);
                        drop(receiver);
                        drop(sender);
                        for (_, handle) in handles {
                            let _ = handle.join();
                        }
                        return Err(ParallelLeafScanError::WorkerSpawn {
                            worker_index,
                            source,
                        });
                    }
                }
            }
            drop(sender);

            let mut result = self.consume_parallel_leaf_events(
                &receiver,
                &mut states,
                &mut results,
                initial_activity_generation,
            );
            if result.is_err() {
                cancellation.store(true, Ordering::Release);
            }
            drop(receiver);

            let mut join_error = None;
            for (worker_index, handle) in handles {
                if handle.join().is_err() && join_error.is_none() {
                    join_error = Some(ParallelLeafScanError::WorkerJoinPanicked { worker_index });
                }
            }
            if let Some(join_error) = join_error {
                if result.is_ok()
                    || matches!(
                        result,
                        Err(ParallelLeafScanError::Protocol(
                            ParallelLeafScanProtocolError::TransportDisconnected { .. }
                        ))
                    )
                {
                    result = Err(join_error);
                }
            }
            result?;
            finalize_parallel_leaf_diagnostics(self, &results)?;

            results
                .into_iter()
                .enumerate()
                .map(|(job_index, result)| {
                    result.ok_or_else(|| {
                        ParallelLeafScanError::Protocol(
                            ParallelLeafScanProtocolError::MissingCompletion { job_index },
                        )
                    })
                })
                .collect()
        })
    }

    fn consume_parallel_leaf_events<R, E>(
        &mut self,
        receiver: &Receiver<ParallelLeafWorkerEvent<R, E, L::Preparation>>,
        states: &mut [ParallelLeafJobState],
        results: &mut [Option<SourceBackedSourceOutcome<R>>],
        mut observed_activity_generation: u64,
    ) -> Result<(), ParallelLeafScanError<E, L::Error>>
    where
        E: StdError + 'static,
    {
        let mut returned_jobs = 0_usize;
        while returned_jobs < states.len() {
            let event = match receiver.recv_timeout(INTERMEDIATE_ACTIVITY_POLL_INTERVAL) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(activity) = self
                        .resources
                        .intermediate_activity_after(observed_activity_generation)
                    {
                        observed_activity_generation = activity.generation;
                        self.report_intermediate_activity(activity.stage)
                            .map_err(|source| ParallelLeafScanError::Activity { source })?;
                    } else {
                        self.flush_intermediate_activity()
                            .map_err(|source| ParallelLeafScanError::Activity { source })?;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ParallelLeafScanProtocolError::TransportDisconnected {
                        unfinished_jobs: states.len().saturating_sub(returned_jobs),
                    }
                    .into());
                }
            };
            match event {
                ParallelLeafWorkerEvent::Protocol {
                    worker_index,
                    job_index,
                    message,
                } => {
                    validate_worker(states, job_index, worker_index)?;
                    apply_parallel_leaf_message(self, job_index, *message, states, results)?;
                }
                ParallelLeafWorkerEvent::Returned {
                    worker_index,
                    job_index,
                } => {
                    let state = state_mut(states, job_index)?;
                    if state.worker_index != worker_index {
                        return Err(ParallelLeafScanProtocolError::WrongWorker {
                            job_index,
                            expected_worker: state.worker_index,
                            observed_worker: worker_index,
                        }
                        .into());
                    }
                    if state.returned {
                        return Err(
                            ParallelLeafScanProtocolError::DuplicateReturn { job_index }.into()
                        );
                    }
                    if state.completion.is_none() {
                        return Err(
                            ParallelLeafScanProtocolError::MissingCompletion { job_index }.into(),
                        );
                    }
                    state.returned = true;
                    returned_jobs = returned_jobs.saturating_add(1);
                }
                ParallelLeafWorkerEvent::Failed {
                    worker_index,
                    job_index,
                    error,
                } => {
                    return Err(ParallelLeafScanError::Worker {
                        worker_index,
                        job_index,
                        source: error,
                    });
                }
                ParallelLeafWorkerEvent::Panicked {
                    worker_index,
                    job_index,
                } => {
                    return Err(ParallelLeafScanError::WorkerPanicked {
                        worker_index,
                        job_index,
                    });
                }
                ParallelLeafWorkerEvent::Cancelled {
                    worker_index,
                    job_index,
                } => {
                    return Err(ParallelLeafScanError::WorkerCancelled {
                        worker_index,
                        job_index,
                    });
                }
            }
        }
        Ok(())
    }
}

fn stripe_leaf_jobs<J>(
    jobs: Vec<J>,
    worker_count: usize,
    worker_assignments: &[usize],
) -> Vec<Vec<(usize, J)>> {
    let mut stripes = (0..worker_count).map(|_| Vec::new()).collect::<Vec<_>>();
    for (job_index, job) in jobs.into_iter().enumerate() {
        stripes[worker_assignments[job_index]].push((job_index, job));
    }
    stripes
}

fn run_leaf_worker<J, R, E, W, F, P>(
    worker_index: usize,
    jobs: Vec<(usize, J)>,
    sender: &SyncSender<ParallelLeafWorkerEvent<R, E, P>>,
    cancellation: &AtomicBool,
    context: ParallelLeafWorkerContext<P>,
    worker_state: &mut W,
    scan: &F,
) where
    F: Fn(
        &mut W,
        &J,
        &mut ParallelLeafScanEmitter<'_, R, E, P>,
    ) -> Result<(), ParallelLeafScanWorkerError<E>>,
    E: StdError + 'static,
    P: CorePreparationPort,
{
    for (job_index, job) in &jobs {
        if cancellation.load(Ordering::Acquire) {
            return;
        }
        let mut emitter = ParallelLeafScanEmitter {
            worker_index,
            job_index: *job_index,
            sender,
            cancellation,
            resources: context.resources.clone(),
            core_record_preparer: context.core_record_preparer.clone(),
            #[cfg(test)]
            successful_resource_acquisitions: None,
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| scan(worker_state, job, &mut emitter)));
        match outcome {
            Ok(Ok(())) => {
                if send_worker_event(
                    sender,
                    ParallelLeafWorkerEvent::Returned {
                        worker_index,
                        job_index: *job_index,
                    },
                    cancellation,
                )
                .is_err()
                {
                    return;
                }
            }
            Ok(Err(ParallelLeafScanWorkerError::Provider(error))) => {
                cancellation.store(true, Ordering::Release);
                let _ = sender.send(ParallelLeafWorkerEvent::Failed {
                    worker_index,
                    job_index: *job_index,
                    error,
                });
                return;
            }
            Ok(Err(ParallelLeafScanWorkerError::Cancelled(_))) => {
                if cancellation.load(Ordering::Acquire) {
                    return;
                }
                cancellation.store(true, Ordering::Release);
                let _ = sender.send(ParallelLeafWorkerEvent::Cancelled {
                    worker_index,
                    job_index: *job_index,
                });
                return;
            }
            Err(_) => {
                cancellation.store(true, Ordering::Release);
                let _ = sender.send(ParallelLeafWorkerEvent::Panicked {
                    worker_index,
                    job_index: *job_index,
                });
                return;
            }
        }
    }
}

fn send_worker_event<R, E, P: CorePreparationPort>(
    sender: &SyncSender<ParallelLeafWorkerEvent<R, E, P>>,
    event: ParallelLeafWorkerEvent<R, E, P>,
    cancellation: &AtomicBool,
) -> Result<(), ParallelLeafScanCancelled> {
    if cancellation.load(Ordering::Acquire) {
        return Err(ParallelLeafScanCancelled);
    }
    sender.send(event).map_err(|_| ParallelLeafScanCancelled)
}

#[cfg(test)]
mod tests;
