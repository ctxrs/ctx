use std::{
    error::Error as StdError,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread,
};

use super::SourceBackedGenerationSink;
use ctx_history_core::SourceKey;

mod protocol;

#[cfg(test)]
use protocol::ParallelLeafProtocolMessage;
use protocol::{
    apply_parallel_leaf_message, state_mut, validate_worker, ParallelLeafJobState,
    ParallelLeafWorkerEvent,
};
#[allow(unused_imports)]
pub use protocol::{
    ParallelLeafScanBegin, ParallelLeafScanCancelled, ParallelLeafScanComplete,
    ParallelLeafScanEmitter, ParallelLeafScanError, ParallelLeafScanJob,
    ParallelLeafScanMessageKind, ParallelLeafScanMode, ParallelLeafScanProtocolError,
    ParallelLeafScanWorkerError, ParallelLeafSinkOperation,
};

const MAX_PARALLEL_LEAF_WORKERS: usize = 16;
const INDEXER_THREAD_CAP: usize = 8;
const RUNTIME_THREAD_RESERVATION: usize = 2;
const SOURCE_WORKER_THREAD_PREFIX: &str = "ctx-src-scan";

pub(crate) fn source_backed_leaf_worker_budget(indexer_threads: usize) -> usize {
    let available_parallelism = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    leaf_worker_budget_for_parallelism(indexer_threads, available_parallelism)
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

impl SourceBackedGenerationSink<'_> {
    /// Recommends the production scanner count after reserving the clamped
    /// Tantivy indexer budget and two runtime threads.
    pub fn recommended_leaf_workers(&self, leaf_count: usize) -> usize {
        leaf_count.min(self.leaf_worker_budget)
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
        self.run_parallel_leaf_scans_inner(
            jobs,
            worker_count,
            |job| Some(job.source().clone()),
            scan,
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
        self.run_parallel_leaf_scans_inner(leaves, worker_count, |_| None, scan)
    }

    fn run_parallel_leaf_scans_inner<J, R, E, F, S>(
        &mut self,
        jobs: Vec<J>,
        worker_count: usize,
        expected_source: S,
        scan: F,
    ) -> Result<Vec<R>, ParallelLeafScanError<E>>
    where
        J: Send,
        R: Send,
        E: StdError + Send + 'static,
        F: Fn(
                &J,
                &mut ParallelLeafScanEmitter<'_, R, E>,
            ) -> Result<(), ParallelLeafScanWorkerError<E>>
            + Sync,
        S: Fn(&J) -> Option<SourceKey>,
    {
        if jobs.is_empty() {
            return Ok(Vec::new());
        }
        if worker_count == 0 {
            return Err(ParallelLeafScanError::InvalidWorkerCount {
                job_count: jobs.len(),
            });
        }

        let worker_count = bounded_leaf_worker_count(jobs.len(), worker_count);
        let mut states = jobs
            .iter()
            .enumerate()
            .map(|(job_index, job)| {
                ParallelLeafJobState::new(expected_source(job), job_index % worker_count)
            })
            .collect::<Vec<_>>();
        let mut results = (0..jobs.len()).map(|_| None).collect::<Vec<_>>();
        let stripes = stripe_leaf_jobs(jobs, worker_count);
        let cancellation = Arc::new(AtomicBool::new(false));

        thread::scope(|scope| {
            // A shared zero-capacity channel bounds the whole transport to one
            // accepted message while preserving each worker's FIFO job and
            // Core-record order. Cross-worker arrival order is intentionally not
            // generation identity: the writer stages by exact source and its
            // manifest canonicalizes sources before deriving the generation
            // ID. The focused 1-vs-N regression locks down that invariant.
            let (sender, receiver) = mpsc::sync_channel::<ParallelLeafWorkerEvent<R, E>>(0);
            let mut handles = Vec::with_capacity(worker_count);
            for (worker_index, jobs) in stripes.into_iter().enumerate() {
                let worker_sender = sender.clone();
                let worker_cancellation = Arc::clone(&cancellation);
                let scan = &scan;
                let worker_name = source_worker_thread_name(worker_index);
                let handle = thread::Builder::new()
                    .name(worker_name)
                    .spawn_scoped(scope, move || {
                        run_leaf_worker(
                            worker_index,
                            jobs,
                            &worker_sender,
                            &worker_cancellation,
                            scan,
                        );
                    })
                    .unwrap_or_else(|error| {
                        panic!("failed to spawn parallel source worker {worker_index}: {error}")
                    });
                handles.push((worker_index, handle));
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
        results: &mut [Option<R>],
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
                    apply_parallel_leaf_message(self, job_index, message, states, results)?;
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

fn stripe_leaf_jobs<J>(jobs: Vec<J>, worker_count: usize) -> Vec<Vec<(usize, J)>> {
    let mut stripes = (0..worker_count).map(|_| Vec::new()).collect::<Vec<_>>();
    for (job_index, job) in jobs.into_iter().enumerate() {
        stripes[job_index % worker_count].push((job_index, job));
    }
    stripes
}

fn run_leaf_worker<J, R, E, F>(
    worker_index: usize,
    jobs: Vec<(usize, J)>,
    sender: &SyncSender<ParallelLeafWorkerEvent<R, E>>,
    cancellation: &AtomicBool,
    scan: &F,
) where
    F: Fn(&J, &mut ParallelLeafScanEmitter<'_, R, E>) -> Result<(), ParallelLeafScanWorkerError<E>>,
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
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| scan(job, &mut emitter)));
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
