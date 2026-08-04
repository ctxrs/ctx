#[cfg(test)]
use std::cell::Cell;

use std::{
    error::Error as StdError,
    io,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread,
};

use super::{SourceBackedGenerationSink, SourceBackedSourceOutcome};
use ctx_history_core::SourceKey;
use ctx_history_index::CoreRecordPreparer;

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
const RUNTIME_THREAD_RESERVATION: usize = 2;
const SOURCE_WORKER_THREAD_PREFIX: &str = "ctx-src-scan";

#[derive(Clone)]
struct ParallelLeafWorkerContext {
    resources: super::SourceBackedRouteResources,
    core_record_preparer: CoreRecordPreparer,
}

#[cfg(test)]
thread_local! {
    static INJECT_WORKER_SPAWN_FAILURE_AT: Cell<Option<usize>> = const { Cell::new(None) };
}

pub fn source_backed_refresh_work_budget(indexer_threads: usize) -> usize {
    let available_parallelism = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    leaf_worker_budget_for_parallelism(indexer_threads, available_parallelism)
}

#[cfg(test)]
pub(crate) fn source_backed_leaf_worker_budget(indexer_threads: usize) -> usize {
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

impl SourceBackedGenerationSink<'_> {
    /// Recommends the production scanner count after reserving the clamped
    /// Tantivy indexer budget and two runtime threads.
    pub fn recommended_leaf_workers(&self, leaf_count: usize) -> usize {
        leaf_count.min(self.resources.leaf_worker_budget())
    }

    /// Runs provider-owned leaf scans on scoped workers while this caller
    /// thread exclusively applies their typed protocol to the generation.
    pub fn run_parallel_leaf_scans<L, R, E, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<L>>,
        worker_count: usize,
        scan: F,
    ) -> Result<Vec<R>, ParallelLeafScanError<E>>
    where
        L: Send,
        R: Send,
        E: StdError + Send + 'static,
        F: Fn(
                &ParallelLeafScanJob<L>,
                &mut ParallelLeafScanEmitter<'_, R, E>,
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
    pub(crate) fn run_parallel_leaf_scans_with_worker_states<L, R, E, W, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<L>>,
        worker_states: &mut [W],
        scan: F,
    ) -> Result<Vec<R>, ParallelLeafScanError<E>>
    where
        L: Send,
        R: Send,
        E: StdError + Send + 'static,
        W: Send,
        F: Fn(
                &mut W,
                &ParallelLeafScanJob<L>,
                &mut ParallelLeafScanEmitter<'_, R, E>,
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

    pub(crate) fn run_parallel_leaf_scans_with_source_outcomes<L, R, E, F>(
        &mut self,
        jobs: Vec<ParallelLeafScanJob<L>>,
        worker_count: usize,
        scan: F,
    ) -> Result<Vec<SourceBackedSourceOutcome<R>>, ParallelLeafScanError<E>>
    where
        L: Send,
        R: Send,
        E: StdError + Send + 'static,
        F: Fn(
                &ParallelLeafScanJob<L>,
                &mut ParallelLeafScanEmitter<'_, R, E>,
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
    pub fn run_parallel_leaf_scans_discovering_sources<L, R, E, F>(
        &mut self,
        leaves: Vec<L>,
        worker_count: usize,
        scan: F,
    ) -> Result<Vec<R>, ParallelLeafScanError<E>>
    where
        L: Send,
        R: Send,
        E: StdError + Send + 'static,
        F: Fn(
                &L,
                &mut ParallelLeafScanEmitter<'_, R, E>,
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
    ) -> Result<Vec<SourceBackedSourceOutcome<R>>, ParallelLeafScanError<E>>
    where
        J: Send,
        R: Send,
        E: StdError + Send + 'static,
        W: Send,
        F: Fn(
                &mut W,
                &J,
                &mut ParallelLeafScanEmitter<'_, R, E>,
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
            resources: self.resources.clone(),
            core_record_preparer: self.core_record_preparer.clone(),
        };

        thread::scope(|scope| {
            // One shared rendezvous accepts whichever worker is ready without
            // buffering a Core record. Per-worker FIFO preserves each job's
            // protocol order, while result slots and the canonical manifest
            // keep scheduling order out of generation identity.
            let (sender, receiver) = mpsc::sync_channel::<ParallelLeafWorkerEvent<R, E>>(0);
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

            let mut result =
                self.consume_parallel_leaf_events(&receiver, &mut states, &mut results);
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
        receiver: &Receiver<ParallelLeafWorkerEvent<R, E>>,
        states: &mut [ParallelLeafJobState],
        results: &mut [Option<SourceBackedSourceOutcome<R>>],
    ) -> Result<(), ParallelLeafScanError<E>>
    where
        E: StdError + 'static,
    {
        let mut returned_jobs = 0_usize;
        while returned_jobs < states.len() {
            let event = receiver.recv().map_err(|_| {
                ParallelLeafScanProtocolError::TransportDisconnected {
                    unfinished_jobs: states.len().saturating_sub(returned_jobs),
                }
            })?;
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

fn run_leaf_worker<J, R, E, W, F>(
    worker_index: usize,
    jobs: Vec<(usize, J)>,
    sender: &SyncSender<ParallelLeafWorkerEvent<R, E>>,
    cancellation: &AtomicBool,
    context: ParallelLeafWorkerContext,
    worker_state: &mut W,
    scan: &F,
) where
    F: Fn(
        &mut W,
        &J,
        &mut ParallelLeafScanEmitter<'_, R, E>,
    ) -> Result<(), ParallelLeafScanWorkerError<E>>,
    E: StdError + 'static,
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

fn send_worker_event<R, E>(
    sender: &SyncSender<ParallelLeafWorkerEvent<R, E>>,
    event: ParallelLeafWorkerEvent<R, E>,
    cancellation: &AtomicBool,
) -> Result<(), ParallelLeafScanCancelled> {
    if cancellation.load(Ordering::Acquire) {
        return Err(ParallelLeafScanCancelled);
    }
    sender.send(event).map_err(|_| ParallelLeafScanCancelled)
}

#[cfg(test)]
mod tests;
