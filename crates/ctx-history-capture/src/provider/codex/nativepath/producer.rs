//! Provider-local bounded preparation for Codex NativePath.
//!
//! Workers own provider files and scanners. Parser state and every prepared
//! window are charged to one hard byte budget while a fixed-capacity channel
//! bounds message count. The coordinator consumes windows in source/page order
//! and is the only caller allowed to publish through SQLite.

use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};

use super::vertical::{CodexNativeProducerStep, CodexNativeProducerTask, CodexNativeVerticalError};
use super::CodexCatalogSource;
use crate::{CaptureError, Result};

pub(crate) const CODEX_PRODUCER_MAX_WORKERS: usize = 16;
pub(crate) const CODEX_PREPARATION_HARD_BYTES: usize = 256 * 1024 * 1024;
const CODEX_PARSER_INPUT_BYTES: usize = 16 * 1024 * 1024;
const CODEX_PENDING_LOOKAHEAD_BYTES: usize = 8 * 1024 * 1024;
const CODEX_RETAINED_WINDOW_BYTES: usize = 8 * 1024 * 1024;
const CODEX_PRODUCER_STATE_BYTES: usize = CODEX_PARSER_INPUT_BYTES + CODEX_PENDING_LOOKAHEAD_BYTES;
const CODEX_PREPARE_RESERVATION_BYTES: usize =
    CODEX_PRODUCER_STATE_BYTES + CODEX_RETAINED_WINDOW_BYTES;
const CODEX_PREPARATION_QUEUE_MAX_WINDOWS: usize = 64;
const CODEX_SOURCE_MAX_QUEUED_WINDOWS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodexProducerConfig {
    worker_count: usize,
    preparation_bytes: usize,
}

impl CodexProducerConfig {
    pub(crate) fn for_host() -> Self {
        let logical_cpus = thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(2);
        Self::for_logical_cpus(logical_cpus)
    }

    pub(crate) fn for_logical_cpus(logical_cpus: usize) -> Self {
        Self {
            worker_count: logical_cpus
                .saturating_sub(2)
                .clamp(1, CODEX_PRODUCER_MAX_WORKERS),
            preparation_bytes: CODEX_PREPARATION_HARD_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexProducerStats {
    pub(crate) worker_count: usize,
    pub(crate) max_concurrent_producers: usize,
    pub(crate) peak_preparation_bytes: usize,
    pub(crate) blocked_reservations: usize,
    pub(crate) peak_queued_windows: usize,
}

#[allow(dead_code)]
// Each bounded page is moved directly into the ordered consumer. Boxing it
// would add allocator traffic to every Codex publication window.
#[allow(clippy::large_enum_variant)]
pub(crate) enum CodexOrderedProducerItem {
    Step {
        source_ordinal: usize,
        page_ordinal: u64,
        source: CodexCatalogSource,
        step: CodexNativeProducerStep,
    },
    Failed {
        source_ordinal: usize,
        page_ordinal: u64,
        source: CodexCatalogSource,
        error: CodexNativeVerticalError,
    },
}

pub(crate) fn run_codex_bounded_producers(
    tasks: Vec<CodexNativeProducerTask>,
    config: CodexProducerConfig,
    mut consume: impl FnMut(CodexOrderedProducerItem) -> Result<()>,
) -> Result<CodexProducerStats> {
    if tasks.is_empty() {
        return Ok(CodexProducerStats::default());
    }
    let source_count = tasks.len();
    // Every admitted worker retains its parser allocation for the source
    // lifetime and must be able to reserve at least one full output window.
    // Capping admission by the combined reservation makes that progress
    // invariant independent of thread scheduling.
    let budget_worker_limit = config.preparation_bytes / CODEX_PREPARE_RESERVATION_BYTES;
    let worker_count = config
        .worker_count
        .min(budget_worker_limit)
        .min(tasks.len());
    let cancellation = Arc::new(CodexCancellation::default());
    let metrics = Arc::new(CodexProducerMetrics::default());
    let budget = Arc::new(CodexPreparationBudget::new(
        config.preparation_bytes,
        Arc::clone(&metrics),
    ));
    let (task_sender, task_receiver) = mpsc::sync_channel::<(usize, CodexNativeProducerTask)>(0);
    let task_receiver = Arc::new(Mutex::new(task_receiver));
    let (sender, receiver) = mpsc::sync_channel(CODEX_PREPARATION_QUEUE_MAX_WINDOWS);
    let run = thread::scope(|scope| -> Result<()> {
        for worker_ordinal in 0..worker_count {
            let sender = sender.clone();
            let task_receiver = Arc::clone(&task_receiver);
            let worker_cancellation = Arc::clone(&cancellation);
            let worker_budget = Arc::clone(&budget);
            let metrics = Arc::clone(&metrics);
            if let Err(error) = thread::Builder::new()
                .name(format!("ctx-codex-prepare-{worker_ordinal}"))
                .spawn_scoped(scope, move || {
                    worker_loop(
                        worker_ordinal,
                        task_receiver,
                        sender,
                        worker_cancellation,
                        worker_budget,
                        metrics,
                    );
                })
            {
                cancellation.cancel();
                budget.wake();
                drop(task_sender);
                return Err(CaptureError::SystemIo {
                    operation: "creating a Codex NativePath preparation worker",
                    source: error,
                });
            }
        }
        drop(sender);

        let mut tasks = tasks.into_iter().enumerate();
        let mut dispatched_sources = 0_usize;
        let mut task_sender = Some(task_sender);
        while dispatched_sources < worker_count {
            let Some(task) = tasks.next() else {
                break;
            };
            task_sender
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex source handoff closed before initial dispatch",
                ))?
                .send(task)
                .map_err(|_| {
                    cancellation.cancel();
                    budget.wake();
                    contract_error("Codex source handoff closed during initial dispatch")
                })?;
            dispatched_sources = dispatched_sources.saturating_add(1);
        }
        if dispatched_sources == source_count {
            task_sender.take();
        }

        let mut pending = BTreeMap::<(usize, u64), ProducerMessage>::new();
        let mut expected_source = 0_usize;
        let mut expected_page = 0_u64;
        let mut workers_done = 0_usize;
        while expected_source < source_count || workers_done < worker_count {
            while let Some(message) = pending.remove(&(expected_source, expected_page)) {
                match consume_message(
                    message,
                    expected_source,
                    expected_page,
                    &mut consume,
                    &cancellation,
                    &budget,
                )? {
                    OrderedAdvance::Page => {
                        expected_page = expected_page.saturating_add(1);
                    }
                    OrderedAdvance::Source => {
                        expected_source = expected_source.saturating_add(1);
                        expected_page = 0;
                        if let Some(task) = tasks.next() {
                            task_sender
                                .as_ref()
                                .ok_or(CaptureError::SystemInvariant(
                                    "Codex source handoff closed before bounded dispatch",
                                ))?
                                .send(task)
                                .map_err(|_| {
                                    cancellation.cancel();
                                    budget.wake();
                                    contract_error(
                                        "Codex source handoff closed during bounded dispatch",
                                    )
                                })?;
                            dispatched_sources = dispatched_sources.saturating_add(1);
                            if dispatched_sources == source_count {
                                task_sender.take();
                            }
                        }
                    }
                }
            }
            if workers_done == worker_count {
                break;
            }
            let message = receiver.recv().map_err(|_| {
                cancellation.cancel();
                budget.wake();
                contract_error("Codex producer channel closed before ordered completion")
            })?;
            match message {
                ProducerMessage::Done => {
                    workers_done = workers_done.saturating_add(1);
                }
                ProducerMessage::Panicked {
                    worker_ordinal,
                    source_ordinal,
                } => {
                    cancellation.cancel();
                    budget.wake();
                    let _ = (worker_ordinal, source_ordinal);
                    return Err(CaptureError::WorkerPanicked(
                        "Codex NativePath source preparation",
                    ));
                }
                ProducerMessage::Failed { error, .. } if error.requires_immediate_propagation() => {
                    cancellation.cancel();
                    budget.wake();
                    return Err(error.into_capture_error());
                }
                message => {
                    let source_ordinal =
                        message
                            .source_ordinal()
                            .ok_or(CaptureError::SystemInvariant(
                                "Codex producer message has no source ordinal",
                            ))?;
                    let page_ordinal = message.page_ordinal().ok_or(
                        CaptureError::SystemInvariant("Codex producer message has no page ordinal"),
                    )?;
                    if source_ordinal < expected_source
                        || (source_ordinal == expected_source && page_ordinal < expected_page)
                        || pending
                            .insert((source_ordinal, page_ordinal), message)
                            .is_some()
                    {
                        cancellation.cancel();
                        budget.wake();
                        return Err(contract_error(
                            "Codex producer emitted duplicate or stale source work",
                        ));
                    }
                }
            }
        }
        if !pending.is_empty() {
            cancellation.cancel();
            budget.wake();
            return Err(contract_error(
                "Codex producer finished with an ordered result gap",
            ));
        }
        if expected_source != source_count {
            return Err(contract_error(
                "Codex producer finished before every source reached an ordered outcome",
            ));
        }
        if dispatched_sources != source_count {
            return Err(contract_error(
                "Codex producer did not dispatch every bounded source",
            ));
        }
        Ok(())
    });
    if run.is_err() {
        cancellation.cancel();
        budget.wake();
    }
    run?;
    Ok(CodexProducerStats {
        worker_count,
        max_concurrent_producers: metrics.max_active.load(Ordering::Acquire),
        peak_preparation_bytes: metrics.peak_bytes.load(Ordering::Acquire),
        blocked_reservations: metrics.blocked.load(Ordering::Acquire),
        peak_queued_windows: metrics.peak_queued_windows.load(Ordering::Acquire),
    })
}

fn consume_message(
    message: ProducerMessage,
    expected_source: usize,
    expected_page: u64,
    consume: &mut impl FnMut(CodexOrderedProducerItem) -> Result<()>,
    cancellation: &CodexCancellation,
    budget: &CodexPreparationBudget,
) -> Result<OrderedAdvance> {
    let (item, source_done, _permits) = match message {
        ProducerMessage::Step {
            source_ordinal,
            page_ordinal,
            source,
            step,
            permit,
            window_slot,
            ..
        } => {
            if source_ordinal != expected_source || page_ordinal != expected_page {
                cancellation.cancel();
                budget.wake();
                return Err(contract_error(
                    "Codex producer violated deterministic source/page order",
                ));
            }
            let source_done = matches!(
                &step,
                CodexNativeProducerStep::Noop(_)
                    | CodexNativeProducerStep::Window {
                        source_done: true,
                        ..
                    }
            );
            (
                CodexOrderedProducerItem::Step {
                    source_ordinal,
                    page_ordinal,
                    source,
                    step,
                },
                source_done,
                Some((permit, window_slot)),
            )
        }
        ProducerMessage::Failed {
            source_ordinal,
            page_ordinal,
            source,
            error,
        } => {
            if source_ordinal != expected_source || page_ordinal != expected_page {
                cancellation.cancel();
                budget.wake();
                return Err(contract_error(
                    "Codex failed producer violated deterministic source/page order",
                ));
            }
            (
                CodexOrderedProducerItem::Failed {
                    source_ordinal,
                    page_ordinal,
                    source,
                    error,
                },
                true,
                None,
            )
        }
        ProducerMessage::Done | ProducerMessage::Panicked { .. } => {
            return Err(contract_error(
                "Codex coordinator attempted to consume a control message as source work",
            ));
        }
    };
    if let Err(error) = consume(item) {
        cancellation.cancel();
        budget.wake();
        return Err(error);
    }
    Ok(if source_done {
        OrderedAdvance::Source
    } else {
        OrderedAdvance::Page
    })
}

fn worker_loop(
    worker_ordinal: usize,
    task_receiver: Arc<Mutex<mpsc::Receiver<(usize, CodexNativeProducerTask)>>>,
    sender: mpsc::SyncSender<ProducerMessage>,
    cancellation: Arc<CodexCancellation>,
    budget: Arc<CodexPreparationBudget>,
    metrics: Arc<CodexProducerMetrics>,
) {
    loop {
        if cancellation.is_cancelled() {
            break;
        }
        let received = task_receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recv_timeout(Duration::from_millis(10));
        let (source_ordinal, task) = match received {
            Ok(task) => task,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if cancellation.is_cancelled() {
            break;
        }
        let source = task.source().clone();
        let result = catch_unwind(AssertUnwindSafe(|| {
            produce_source(
                source_ordinal,
                source,
                task,
                &sender,
                &cancellation,
                &budget,
                &metrics,
            )
        }));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(())) => break,
            Err(_) => {
                let _ = sender.send(ProducerMessage::Panicked {
                    worker_ordinal,
                    source_ordinal,
                });
                break;
            }
        }
    }
    let _ = sender.send(ProducerMessage::Done);
}

#[allow(clippy::too_many_arguments)]
fn produce_source(
    source_ordinal: usize,
    source: CodexCatalogSource,
    task: CodexNativeProducerTask,
    sender: &mpsc::SyncSender<ProducerMessage>,
    cancellation: &CodexCancellation,
    budget: &Arc<CodexPreparationBudget>,
    metrics: &Arc<CodexProducerMetrics>,
) -> std::result::Result<(), ()> {
    let mut page_ordinal = 0_u64;
    let window_slots = Arc::new(CodexSourceWindowSlots::new(Arc::clone(metrics)));
    // The source-lifetime reservation includes the coalescer's retained
    // lookahead, so a full returned window never leaves an uncharged second
    // window in `pending_step`.
    let _producer_state_permit = match budget.reserve(CODEX_PRODUCER_STATE_BYTES, cancellation) {
        Ok(permit) => permit,
        Err(_) => return Err(()),
    };
    metrics.started();
    let opened = task.open();
    metrics.finished();
    let mut producer = match opened {
        Ok(producer) => producer,
        Err(error) => {
            return send_failure(source_ordinal, page_ordinal, source, error, sender);
        }
    };
    loop {
        if cancellation.is_cancelled() {
            return Err(());
        }
        let window_slot = match window_slots.reserve(cancellation) {
            Ok(permit) => permit,
            Err(_) => return Err(()),
        };
        let mut permit = match budget.reserve(CODEX_RETAINED_WINDOW_BYTES, cancellation) {
            Ok(permit) => permit,
            Err(_) => {
                drop(window_slot);
                return Err(());
            }
        };
        metrics.started();
        let step = producer.next_window();
        metrics.finished();
        let step = match step {
            Ok(step) => step,
            Err(error) => {
                drop(permit);
                drop(window_slot);
                return send_failure(source_ordinal, page_ordinal, source, error, sender);
            }
        };
        let source_done = matches!(
            &step,
            CodexNativeProducerStep::Noop(_)
                | CodexNativeProducerStep::Window {
                    source_done: true,
                    ..
                }
        );
        let retained_bytes = step.retained_bytes();
        if retained_bytes > CODEX_RETAINED_WINDOW_BYTES || permit.shrink_to(retained_bytes).is_err()
        {
            cancellation.cancel();
            budget.wake();
            return Err(());
        }
        if sender
            .send(ProducerMessage::Step {
                source_ordinal,
                page_ordinal,
                source: source.clone(),
                step,
                permit,
                window_slot,
            })
            .is_err()
        {
            return Err(());
        }
        if source_done {
            return Ok(());
        }
        page_ordinal = page_ordinal.saturating_add(1);
    }
}

fn send_failure(
    source_ordinal: usize,
    page_ordinal: u64,
    source: CodexCatalogSource,
    error: CodexNativeVerticalError,
    sender: &mpsc::SyncSender<ProducerMessage>,
) -> std::result::Result<(), ()> {
    if sender
        .send(ProducerMessage::Failed {
            source_ordinal,
            page_ordinal,
            source,
            error,
        })
        .is_err()
    {
        return Err(());
    }
    Ok(())
}

// The message owns one byte-budget permit until ordered consumption. Keeping
// the bounded window inline avoids one allocation per handoff.
#[allow(clippy::large_enum_variant)]
enum ProducerMessage {
    Step {
        source_ordinal: usize,
        page_ordinal: u64,
        source: CodexCatalogSource,
        step: CodexNativeProducerStep,
        permit: CodexPreparationPermit,
        window_slot: CodexSourceWindowPermit,
    },
    Failed {
        source_ordinal: usize,
        page_ordinal: u64,
        source: CodexCatalogSource,
        error: CodexNativeVerticalError,
    },
    Panicked {
        worker_ordinal: usize,
        source_ordinal: usize,
    },
    Done,
}

impl ProducerMessage {
    fn source_ordinal(&self) -> Option<usize> {
        match self {
            Self::Step { source_ordinal, .. }
            | Self::Failed { source_ordinal, .. }
            | Self::Panicked { source_ordinal, .. } => Some(*source_ordinal),
            Self::Done => None,
        }
    }

    fn page_ordinal(&self) -> Option<u64> {
        match self {
            Self::Step { page_ordinal, .. } | Self::Failed { page_ordinal, .. } => {
                Some(*page_ordinal)
            }
            Self::Panicked { .. } | Self::Done => None,
        }
    }
}

enum OrderedAdvance {
    Page,
    Source,
}

#[derive(Default)]
struct CodexCancellation {
    cancelled: AtomicBool,
}

impl CodexCancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Default)]
struct CodexProducerMetrics {
    active: AtomicUsize,
    max_active: AtomicUsize,
    peak_bytes: AtomicUsize,
    blocked: AtomicUsize,
    queued_windows: AtomicUsize,
    peak_queued_windows: AtomicUsize,
}

impl CodexProducerMetrics {
    fn started(&self) {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        update_max(&self.max_active, active);
    }

    fn finished(&self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

struct CodexSourceWindowSlots {
    state: Mutex<usize>,
    available: Condvar,
    metrics: Arc<CodexProducerMetrics>,
}

impl CodexSourceWindowSlots {
    fn new(metrics: Arc<CodexProducerMetrics>) -> Self {
        Self {
            state: Mutex::new(0),
            available: Condvar::new(),
            metrics,
        }
    }

    fn reserve(
        self: &Arc<Self>,
        cancellation: &CodexCancellation,
    ) -> Result<CodexSourceWindowPermit> {
        let mut outstanding = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !cancellation.is_cancelled() && *outstanding >= CODEX_SOURCE_MAX_QUEUED_WINDOWS {
            outstanding = self
                .available
                .wait_timeout(outstanding, Duration::from_millis(10))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        if cancellation.is_cancelled() {
            return Err(contract_error("Codex producer was cancelled"));
        }
        *outstanding = outstanding.saturating_add(1);
        let queued = self.metrics.queued_windows.fetch_add(1, Ordering::AcqRel) + 1;
        update_max(&self.metrics.peak_queued_windows, queued);
        Ok(CodexSourceWindowPermit {
            slots: Arc::clone(self),
        })
    }
}

struct CodexSourceWindowPermit {
    slots: Arc<CodexSourceWindowSlots>,
}

impl Drop for CodexSourceWindowPermit {
    fn drop(&mut self) {
        let mut outstanding = self
            .slots
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *outstanding = outstanding.saturating_sub(1);
        self.slots
            .metrics
            .queued_windows
            .fetch_sub(1, Ordering::AcqRel);
        self.slots.available.notify_one();
    }
}

struct CodexPreparationBudget {
    capacity: usize,
    state: Mutex<CodexPreparationState>,
    available: Condvar,
    metrics: Arc<CodexProducerMetrics>,
}

#[derive(Default)]
struct CodexPreparationState {
    used: usize,
    next_ticket: u64,
    serving_ticket: u64,
}

impl CodexPreparationBudget {
    fn new(capacity: usize, metrics: Arc<CodexProducerMetrics>) -> Self {
        Self {
            capacity,
            state: Mutex::new(CodexPreparationState::default()),
            available: Condvar::new(),
            metrics,
        }
    }

    fn reserve(
        self: &Arc<Self>,
        bytes: usize,
        cancellation: &CodexCancellation,
    ) -> Result<CodexPreparationPermit> {
        if bytes > self.capacity {
            return Err(contract_error(
                "Codex producer reservation exceeds the preparation budget",
            ));
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.saturating_add(1);
        let mut blocked = false;
        while !cancellation.is_cancelled()
            && (ticket != state.serving_ticket || state.used.saturating_add(bytes) > self.capacity)
        {
            blocked = true;
            state = self
                .available
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if cancellation.is_cancelled() {
            return Err(contract_error("Codex producer was cancelled"));
        }
        if blocked {
            self.metrics.blocked.fetch_add(1, Ordering::AcqRel);
        }
        state.serving_ticket = state.serving_ticket.saturating_add(1);
        state.used = state.used.saturating_add(bytes);
        update_max(&self.metrics.peak_bytes, state.used);
        self.available.notify_all();
        Ok(CodexPreparationPermit {
            budget: Arc::clone(self),
            bytes,
        })
    }

    fn wake(&self) {
        self.available.notify_all();
    }
}

struct CodexPreparationPermit {
    budget: Arc<CodexPreparationBudget>,
    bytes: usize,
}

impl CodexPreparationPermit {
    fn shrink_to(&mut self, retained_bytes: usize) -> Result<()> {
        if retained_bytes > self.bytes {
            return Err(contract_error(
                "Codex producer permit cannot grow while retaining a window",
            ));
        }
        let released = self.bytes - retained_bytes;
        if released == 0 {
            return Ok(());
        }
        let mut state = self
            .budget
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.used = state.used.saturating_sub(released);
        self.bytes = retained_bytes;
        self.budget.available.notify_all();
        Ok(())
    }
}

impl Drop for CodexPreparationPermit {
    fn drop(&mut self) {
        let mut state = self
            .budget
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.used = state.used.saturating_sub(self.bytes);
        self.budget.available.notify_all();
    }
}

fn update_max(target: &AtomicUsize, candidate: usize) {
    let mut current = target.load(Ordering::Acquire);
    while candidate > current {
        match target.compare_exchange_weak(current, candidate, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

fn contract_error(message: &'static str) -> CaptureError {
    CaptureError::SystemInvariant(message)
}
