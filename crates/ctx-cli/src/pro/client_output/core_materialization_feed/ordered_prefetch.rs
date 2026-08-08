use super::*;

pub(super) const CORE_PREFETCH_ENCODED_BYTE_BUDGET: usize = 64 * 1024 * 1024;
pub(super) const CORE_PREFETCH_PAGE_ENCODED_BYTE_BUDGET: usize = 8 * 1024 * 1024;
pub(super) const CORE_PREFETCH_RECORD_PAGE_BUDGET: CoreEventPageBudget = CoreEventPageBudget::new(
    CORE_PREFETCH_PAGE_ENCODED_BYTE_BUDGET,
    MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
);

impl CoreWorkerLaunchSelection {
    pub(super) fn execution_options(self) -> CoreFeedExecutionOptions {
        CoreFeedExecutionOptions {
            prefetch_parallelism: self.budget.host_prefetch_workers,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CoreFeedExecutionOptions {
    pub(super) prefetch_parallelism: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct OrderedReconciliationOptions {
    pub(super) prefetch_parallelism: usize,
    pub(super) exchange_mode: EventDeltaExchangeMode,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CorePrefetchInstrumentationSnapshot {
    pub(super) configured_parallelism: usize,
    pub(super) workers_launched: usize,
    pub(super) maximum_active_workers: usize,
    pub(super) planned_pages: usize,
    pub(super) materialized_pages: usize,
    pub(super) decoded_records: usize,
    pub(super) decoded_record_bytes: usize,
    pub(super) record_payload_sha256_traversals: usize,
    pub(super) record_payload_sha256_bytes: usize,
    pub(super) encoded_credit_high_water_bytes: usize,
    pub(super) encoded_credit_final_bytes: usize,
    pub(super) cancelled_waits_or_sends: usize,
}

#[derive(Default)]
pub(super) struct CorePrefetchInstrumentation {
    configured_parallelism: AtomicUsize,
    workers_launched: AtomicUsize,
    active_workers: AtomicUsize,
    maximum_active_workers: AtomicUsize,
    planned_pages: AtomicUsize,
    materialized_pages: AtomicUsize,
    decoded_records: AtomicUsize,
    decoded_record_bytes: AtomicUsize,
    #[cfg(test)]
    record_payload_sha256_traversals: AtomicUsize,
    #[cfg(test)]
    record_payload_sha256_bytes: AtomicUsize,
    cancelled_waits_or_sends: AtomicUsize,
}

impl CorePrefetchInstrumentation {
    fn configured(&self, parallelism: usize, workers_launched: usize) {
        self.configured_parallelism
            .store(parallelism, AtomicOrdering::Relaxed);
        self.workers_launched
            .store(workers_launched, AtomicOrdering::Relaxed);
    }

    fn worker_started(&self) {
        let active = self.active_workers.fetch_add(1, AtomicOrdering::Relaxed) + 1;
        self.maximum_active_workers
            .fetch_max(active, AtomicOrdering::Relaxed);
    }

    fn worker_finished(&self) {
        self.active_workers.fetch_sub(1, AtomicOrdering::Relaxed);
    }

    fn page_planned(&self) {
        self.planned_pages.fetch_add(1, AtomicOrdering::Relaxed);
    }

    fn page_materialized(&self) {
        self.materialized_pages
            .fetch_add(1, AtomicOrdering::Relaxed);
    }

    fn records_decoded(&self, records: usize, record_bytes: usize) {
        self.decoded_records
            .fetch_add(records, AtomicOrdering::Relaxed);
        self.decoded_record_bytes
            .fetch_add(record_bytes, AtomicOrdering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn record_payload_sha256_traversed(&self, encoded_bytes: usize) {
        self.record_payload_sha256_traversals
            .fetch_add(1, AtomicOrdering::Relaxed);
        self.record_payload_sha256_bytes
            .fetch_add(encoded_bytes, AtomicOrdering::Relaxed);
    }

    fn cancelled(&self) {
        self.cancelled_waits_or_sends
            .fetch_add(1, AtomicOrdering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn snapshot(
        &self,
        encoded_credit_high_water_bytes: usize,
        encoded_credit_final_bytes: usize,
    ) -> CorePrefetchInstrumentationSnapshot {
        CorePrefetchInstrumentationSnapshot {
            configured_parallelism: self.configured_parallelism.load(AtomicOrdering::Relaxed),
            workers_launched: self.workers_launched.load(AtomicOrdering::Relaxed),
            maximum_active_workers: self.maximum_active_workers.load(AtomicOrdering::Relaxed),
            planned_pages: self.planned_pages.load(AtomicOrdering::Relaxed),
            materialized_pages: self.materialized_pages.load(AtomicOrdering::Relaxed),
            decoded_records: self.decoded_records.load(AtomicOrdering::Relaxed),
            decoded_record_bytes: self.decoded_record_bytes.load(AtomicOrdering::Relaxed),
            record_payload_sha256_traversals: self
                .record_payload_sha256_traversals
                .load(AtomicOrdering::Relaxed),
            record_payload_sha256_bytes: self
                .record_payload_sha256_bytes
                .load(AtomicOrdering::Relaxed),
            encoded_credit_high_water_bytes,
            encoded_credit_final_bytes,
            cancelled_waits_or_sends: self.cancelled_waits_or_sends.load(AtomicOrdering::Relaxed),
        }
    }
}

#[derive(Default)]
struct EncodedPageCreditState {
    in_use: usize,
    high_water: usize,
    cancelled: bool,
    // Ordered oversized requests stop later speculative pages from occupying
    // credits that the coordinator cannot release before reaching the request.
    oversize_waiters: BTreeSet<usize>,
}

pub(super) struct EncodedPageCredits {
    capacity: usize,
    state: Mutex<EncodedPageCreditState>,
    available: Condvar,
}

impl EncodedPageCredits {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(EncodedPageCreditState::default()),
            available: Condvar::new(),
        }
    }

    fn acquire(self: &Arc<Self>, bytes: usize) -> Result<Option<EncodedPageCredit>> {
        self.acquire_ordered(bytes, None, None)
    }

    fn acquire_prefetched(
        self: &Arc<Self>,
        bytes: usize,
        source_ordinal: usize,
        controls: &CurrentPrefetchControls,
    ) -> Result<Option<EncodedPageCredit>> {
        self.acquire_ordered(bytes, Some(source_ordinal), Some(controls))
    }

    fn acquire_ordered(
        self: &Arc<Self>,
        bytes: usize,
        source_ordinal: Option<usize>,
        controls: Option<&CurrentPrefetchControls>,
    ) -> Result<Option<EncodedPageCredit>> {
        if bytes > self.capacity {
            bail!(
                "invalid_request: planned Core page requires {bytes} encoded bytes, exceeding the {}-byte prefetch budget",
                self.capacity
            );
        }
        let oversize_source =
            source_ordinal.filter(|_| bytes > CORE_PREFETCH_PAGE_ENCODED_BYTE_BUDGET);
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        if let Some(source_ordinal) = oversize_source {
            state.oversize_waiters.insert(source_ordinal);
            drop(state);
            // Later workers retain their prepared pages until ordered demand,
            // so they can discard that speculation and release exact credits.
            if let Some(controls) = controls {
                controls.yield_later_sources(source_ordinal);
            }
            state = self
                .state
                .lock()
                .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        }
        while !state.cancelled {
            let first_oversize_waiter = state.oversize_waiters.first().copied();
            let waits_for_earlier_oversize = match (source_ordinal, oversize_source) {
                (Some(source_ordinal), Some(_)) => first_oversize_waiter != Some(source_ordinal),
                (Some(source_ordinal), None) => {
                    first_oversize_waiter.is_some_and(|waiter| waiter < source_ordinal)
                }
                (None, _) => false,
            };
            let exceeds_capacity = state
                .in_use
                .checked_add(bytes)
                .is_none_or(|total| total > self.capacity);
            if !waits_for_earlier_oversize && !exceeds_capacity {
                break;
            }
            state = self
                .available
                .wait(state)
                .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        }
        if state.cancelled {
            if let Some(source_ordinal) = oversize_source {
                state.oversize_waiters.remove(&source_ordinal);
            }
            return Ok(None);
        }
        if let Some(source_ordinal) = oversize_source {
            state.oversize_waiters.remove(&source_ordinal);
        }
        state.in_use = state
            .in_use
            .checked_add(bytes)
            .ok_or_else(|| anyhow!("internal: Core prefetch credit overflowed"))?;
        state.high_water = state.high_water.max(state.in_use);
        Ok(Some(EncodedPageCredit {
            owner: Arc::clone(self),
            bytes,
        }))
    }

    fn should_yield_to_oversize(&self, source_ordinal: usize) -> Result<bool> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        if state.cancelled {
            bail!("internal: Core prefetch was cancelled");
        }
        Ok(state
            .oversize_waiters
            .first()
            .is_some_and(|waiter| *waiter < source_ordinal))
    }

    fn cancel(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.cancelled = true;
            state.oversize_waiters.clear();
        }
        self.available.notify_all();
    }

    pub(super) fn snapshot(&self) -> Result<(usize, usize)> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("internal: Core prefetch credit lock poisoned"))?;
        Ok((state.in_use, state.high_water))
    }

    fn release(&self, bytes: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.in_use = state.in_use.saturating_sub(bytes);
        }
        self.available.notify_all();
    }
}

pub(super) struct EncodedPageCredit {
    owner: Arc<EncodedPageCredits>,
    bytes: usize,
}

impl Drop for EncodedPageCredit {
    fn drop(&mut self) {
        self.owner.release(self.bytes);
    }
}

struct ActivePreparation {
    instrumentation: Arc<CorePrefetchInstrumentation>,
}

impl ActivePreparation {
    fn new(instrumentation: &Arc<CorePrefetchInstrumentation>) -> Self {
        instrumentation.worker_started();
        Self {
            instrumentation: Arc::clone(instrumentation),
        }
    }
}

impl Drop for ActivePreparation {
    fn drop(&mut self) {
        self.instrumentation.worker_finished();
    }
}

#[derive(Debug)]
pub(super) enum PreparedCoreRecordJson {
    Stored(StoredCoreRecordJson),
    #[cfg(test)]
    Shared {
        encoded: Arc<[u8]>,
        content_bytes: usize,
    },
}

impl PreparedCoreRecordJson {
    pub(super) fn encoded_core_record(&self) -> Result<&[u8]> {
        match self {
            Self::Stored(stored) => Ok(stored.encoded_core_record()?),
            #[cfg(test)]
            Self::Shared { encoded, .. } => Ok(encoded),
        }
    }

    pub(super) fn bytes(&self) -> Result<&[u8]> {
        self.encoded_core_record()
    }

    pub(super) fn content_bytes(&self) -> usize {
        match self {
            Self::Stored(stored) => stored.content_bytes,
            #[cfg(test)]
            Self::Shared { content_bytes, .. } => *content_bytes,
        }
    }
}

pub(super) struct PreparedCurrentRecord {
    pub(super) record: ctx_history_core::CoreRecord,
    pub(super) stored_json: PreparedCoreRecordJson,
    pub(super) core_record_sha256: String,
}

pub(super) struct PreparedCurrentPage {
    pub(super) records: Vec<PreparedCurrentRecord>,
    pub(super) terminal: bool,
    pub(super) _encoded_credit: EncodedPageCredit,
}

struct KeyedPreparedCurrentPage {
    source_ordinal: usize,
    page_index: u32,
    result: Result<PreparedCurrentPage>,
}

struct CurrentSourcePrefetchJob {
    source_ordinal: usize,
    source: CoreSourceState,
    lane: SyncSender<KeyedPreparedCurrentPage>,
    controls: Receiver<CurrentPrefetchControl>,
}

pub(super) struct PrefetchedCurrentPageLane {
    results: Receiver<KeyedPreparedCurrentPage>,
    controls: SyncSender<CurrentPrefetchControl>,
}

#[derive(Clone, Copy)]
enum CurrentPrefetchControl {
    // Result lanes are rendezvous channels: a future source's page stays with
    // its worker and remains reclaimable until the coordinator requests it.
    Consume,
    YieldForOversize,
    Cancel,
}

struct CurrentPrefetchControls {
    lanes: Vec<Option<SyncSender<CurrentPrefetchControl>>>,
}

impl CurrentPrefetchControls {
    fn yield_later_sources(&self, source_ordinal: usize) {
        for controls in self
            .lanes
            .iter()
            .skip(source_ordinal.saturating_add(1))
            .flatten()
        {
            let _ = controls.try_send(CurrentPrefetchControl::YieldForOversize);
        }
    }

    fn cancel_all(&self) {
        for controls in self.lanes.iter().flatten() {
            let _ = controls.try_send(CurrentPrefetchControl::Cancel);
        }
    }
}

pub(super) struct SequentialCurrentPageStream<'a> {
    index: &'a VerifiedIndex,
    source: CoreSourceState,
    cursor: Option<SourceEventCursor>,
    page_index: u32,
    credits: Arc<EncodedPageCredits>,
    instrumentation: Arc<CorePrefetchInstrumentation>,
}

pub(super) enum CurrentPageStream<'a> {
    Removed,
    Sequential(Box<SequentialCurrentPageStream<'a>>),
    Prefetched {
        source_ordinal: usize,
        page_index: u32,
        lane: PrefetchedCurrentPageLane,
    },
}

impl CurrentPageStream<'_> {
    pub(super) fn initially_terminal(&self) -> bool {
        matches!(self, Self::Removed)
    }

    pub(super) fn next_page(&mut self) -> Result<PreparedCurrentPage> {
        match self {
            Self::Removed => bail!("internal: removed Core source requested a current page"),
            Self::Sequential(stream) => {
                let plan = plan_current_page(
                    stream.index,
                    &stream.source,
                    stream.cursor.as_ref(),
                    CORE_RECORD_PAGE_BUDGET,
                    &stream.instrumentation,
                )?;
                let prepared = prepare_planned_current_page(
                    stream.index,
                    &stream.source,
                    plan,
                    CORE_RECORD_PAGE_BUDGET,
                    &stream.credits,
                    &stream.instrumentation,
                    None,
                )?
                .ok_or_else(|| anyhow!("internal: sequential Core prefetch was cancelled"))?;
                stream.cursor = prepared.1;
                stream.page_index = stream
                    .page_index
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("invalid_request: Core source page index overflowed"))?;
                Ok(prepared.0)
            }
            Self::Prefetched {
                source_ordinal,
                page_index,
                lane,
            } => {
                lane.controls
                    .send(CurrentPrefetchControl::Consume)
                    .map_err(|_| {
                        anyhow!("internal: Core prefetch worker closed before ordered demand")
                    })?;
                let keyed = lane.results.recv().map_err(|_| {
                    anyhow!("internal: Core prefetch lane closed before its ordered page")
                })?;
                if keyed.source_ordinal != *source_ordinal || keyed.page_index != *page_index {
                    bail!("internal: Core prefetch result escaped source/page order");
                }
                *page_index = page_index
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("invalid_request: Core source page index overflowed"))?;
                keyed.result
            }
        }
    }
}

fn plan_current_page(
    index: &VerifiedIndex,
    source: &CoreSourceState,
    cursor: Option<&SourceEventCursor>,
    page_budget: CoreEventPageBudget,
    instrumentation: &Arc<CorePrefetchInstrumentation>,
) -> Result<CoreSourceEventPagePlan> {
    let plan = index.plan_core_source_event_page_with_budget(
        &source.source,
        cursor,
        MAX_CORE_EVENT_DELTA_PAGE_ITEMS,
        page_budget,
    )?;
    instrumentation.page_planned();
    Ok(plan)
}

fn prepare_planned_current_page(
    index: &VerifiedIndex,
    source: &CoreSourceState,
    plan: CoreSourceEventPagePlan,
    page_budget: CoreEventPageBudget,
    credits: &Arc<EncodedPageCredits>,
    instrumentation: &Arc<CorePrefetchInstrumentation>,
    prefetch_position: Option<(usize, &CurrentPrefetchControls)>,
) -> Result<Option<(PreparedCurrentPage, Option<SourceEventCursor>)>> {
    let planned_encoded_bytes = plan.encoded_core_bytes();
    let encoded_credit = match prefetch_position {
        Some((source_ordinal, controls)) => {
            credits.acquire_prefetched(planned_encoded_bytes, source_ordinal, controls)?
        }
        None => credits.acquire(planned_encoded_bytes)?,
    };
    let Some(encoded_credit) = encoded_credit else {
        instrumentation.cancelled();
        return Ok(None);
    };
    let _active = ActivePreparation::new(instrumentation);
    let source_page = index.materialize_stored_core_source_event_page(plan)?;
    instrumentation.page_materialized();
    if source_page.generation_id != index.generation_id()
        || !source_page.source.exact_descriptor_eq(&source.source)
    {
        bail!("core_generation_mismatch: Core record page escaped its pinned generation");
    }
    if source_page.items.len() > MAX_CORE_EVENT_DELTA_PAGE_ITEMS {
        bail!("invalid_request: Core record page exceeded its item bound");
    }
    if source_page.encoded_core_bytes != planned_encoded_bytes {
        bail!("core_generation_mismatch: planned Core page encoded bytes changed");
    }
    if source_page.encoded_core_bytes > page_budget.maximum_encoded_core_bytes
        && source_page.items.len() != 1
    {
        bail!(
            "invalid_request: Core record page exceeded the {}-byte Pro page encoded-payload target without singleton progress",
            page_budget.maximum_encoded_core_bytes
        );
    }
    if source_page.content_bytes > page_budget.maximum_content_bytes {
        bail!(
            "invalid_request: one Core record exceeds the {}-byte Pro page content bound",
            page_budget.maximum_content_bytes
        );
    }
    instrumentation.records_decoded(source_page.items.len(), source_page.encoded_core_bytes);
    let terminal = source_page.terminal;
    let next_cursor = source_page.next_cursor;
    let records = source_page
        .items
        .into_iter()
        .map(|item| {
            let encoded = item.stored_json.encoded_core_record()?;
            #[cfg(test)]
            instrumentation.record_payload_sha256_traversed(encoded.len());
            let core_record_sha256 = core_record_sha256_from_encoded(encoded);
            Ok(PreparedCurrentRecord {
                record: item.core_record,
                stored_json: PreparedCoreRecordJson::Stored(item.stored_json),
                core_record_sha256,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some((
        PreparedCurrentPage {
            records,
            terminal,
            _encoded_credit: encoded_credit,
        },
        next_cursor,
    )))
}

fn wait_for_oversized_page_demand(
    job: &CurrentSourcePrefetchJob,
    instrumentation: &CorePrefetchInstrumentation,
) -> bool {
    loop {
        match job.controls.recv() {
            Ok(CurrentPrefetchControl::Consume) => return true,
            // This page holds no encoded-byte credit yet, so an earlier
            // oversized waiter has nothing to reclaim from it.
            Ok(CurrentPrefetchControl::YieldForOversize) => {}
            Ok(CurrentPrefetchControl::Cancel) | Err(_) => {
                instrumentation.cancelled();
                return false;
            }
        }
    }
}

fn run_current_source_prefetch_worker(
    index: &VerifiedIndex,
    jobs: &Arc<Mutex<VecDeque<CurrentSourcePrefetchJob>>>,
    credits: &Arc<EncodedPageCredits>,
    controls: &CurrentPrefetchControls,
    instrumentation: &Arc<CorePrefetchInstrumentation>,
) {
    loop {
        let job = match jobs.lock() {
            Ok(mut jobs) => jobs.pop_front(),
            Err(_) => return,
        };
        let Some(job) = job else {
            return;
        };
        let mut cursor = None;
        let mut page_index = 0_u32;
        'pages: loop {
            let mut demand_consumed = false;
            let prepared = plan_current_page(
                index,
                &job.source,
                cursor.as_ref(),
                CORE_PREFETCH_RECORD_PAGE_BUDGET,
                instrumentation,
            )
            .and_then(|plan| {
                if plan.encoded_core_bytes() > CORE_PREFETCH_PAGE_ENCODED_BYTE_BUDGET {
                    if !wait_for_oversized_page_demand(&job, instrumentation) {
                        return Ok(None);
                    }
                    demand_consumed = true;
                }
                prepare_planned_current_page(
                    index,
                    &job.source,
                    plan,
                    CORE_PREFETCH_RECORD_PAGE_BUDGET,
                    credits,
                    instrumentation,
                    Some((job.source_ordinal, controls)),
                )
            });
            match prepared {
                Ok(Some((page, next_cursor))) => {
                    let terminal = page.terminal;
                    let mut keyed = Some(KeyedPreparedCurrentPage {
                        source_ordinal: job.source_ordinal,
                        page_index,
                        result: Ok(page),
                    });
                    if demand_consumed {
                        let Some(keyed) = keyed.take() else {
                            instrumentation.cancelled();
                            return;
                        };
                        if job.lane.send(keyed).is_err() {
                            instrumentation.cancelled();
                            return;
                        }
                    } else {
                        loop {
                            match job.controls.recv() {
                                Ok(CurrentPrefetchControl::Consume) => {
                                    let Some(keyed) = keyed.take() else {
                                        instrumentation.cancelled();
                                        return;
                                    };
                                    if job.lane.send(keyed).is_err() {
                                        instrumentation.cancelled();
                                        return;
                                    }
                                    break;
                                }
                                Ok(CurrentPrefetchControl::YieldForOversize) => {
                                    match credits.should_yield_to_oversize(job.source_ordinal) {
                                        Ok(true) => {
                                            drop(keyed.take());
                                            continue 'pages;
                                        }
                                        Ok(false) => {}
                                        Err(_) => {
                                            instrumentation.cancelled();
                                            return;
                                        }
                                    }
                                }
                                Ok(CurrentPrefetchControl::Cancel) | Err(_) => {
                                    instrumentation.cancelled();
                                    return;
                                }
                            }
                        }
                    }
                    if terminal {
                        break;
                    }
                    cursor = next_cursor;
                    let Some(next_page_index) = page_index.checked_add(1) else {
                        let _ = job.lane.send(KeyedPreparedCurrentPage {
                            source_ordinal: job.source_ordinal,
                            page_index,
                            result: Err(anyhow!(
                                "invalid_request: Core source page index overflowed"
                            )),
                        });
                        return;
                    };
                    page_index = next_page_index;
                }
                Ok(None) => return,
                Err(error) if demand_consumed => {
                    if job
                        .lane
                        .send(KeyedPreparedCurrentPage {
                            source_ordinal: job.source_ordinal,
                            page_index,
                            result: Err(error),
                        })
                        .is_err()
                    {
                        instrumentation.cancelled();
                    }
                    return;
                }
                Err(error) => loop {
                    match job.controls.recv() {
                        Ok(CurrentPrefetchControl::Consume) => {
                            if job
                                .lane
                                .send(KeyedPreparedCurrentPage {
                                    source_ordinal: job.source_ordinal,
                                    page_index,
                                    result: Err(error),
                                })
                                .is_err()
                            {
                                instrumentation.cancelled();
                            }
                            return;
                        }
                        Ok(CurrentPrefetchControl::YieldForOversize) => {
                            if credits
                                .should_yield_to_oversize(job.source_ordinal)
                                .is_err()
                            {
                                instrumentation.cancelled();
                                return;
                            }
                        }
                        Ok(CurrentPrefetchControl::Cancel) | Err(_) => {
                            instrumentation.cancelled();
                            return;
                        }
                    }
                },
            }
        }
    }
}

fn cancel_current_source_prefetch(
    jobs: &Arc<Mutex<VecDeque<CurrentSourcePrefetchJob>>>,
    credits: &EncodedPageCredits,
    controls: &CurrentPrefetchControls,
) {
    credits.cancel();
    if let Ok(mut jobs) = jobs.lock() {
        jobs.clear();
    }
    controls.cancel_all();
}

pub(super) fn reconcile_ordered_source_events<C: CoreMaterializationConsumer>(
    index: &VerifiedIndex,
    materialization_id: &str,
    reconciliations: Vec<CoreSourceReconciliation>,
    consumer: &mut C,
    options: OrderedReconciliationOptions,
    credits: &Arc<EncodedPageCredits>,
    instrumentation: &Arc<CorePrefetchInstrumentation>,
) -> Result<EventReconciliationReport> {
    let parallelism = options
        .prefetch_parallelism
        .clamp(1, MAX_CORE_PREFETCH_WORKERS);
    if parallelism == 1 {
        instrumentation.configured(1, 0);
        let mut aggregate = EventReconciliationReport {
            pages: 0,
            mutations: 0,
        };
        let mut pending_batch = EventDeltaPageBatchBuilder::new()?;
        for reconciliation in reconciliations {
            let current_source = match &reconciliation.delta {
                CoreSourceDelta::Present(source) => Some(source.clone()),
                CoreSourceDelta::Removed(_) => None,
            };
            let mut current_pages = match current_source {
                Some(source) => {
                    CurrentPageStream::Sequential(Box::new(SequentialCurrentPageStream {
                        index,
                        source,
                        cursor: None,
                        page_index: 0,
                        credits: Arc::clone(credits),
                        instrumentation: Arc::clone(instrumentation),
                    }))
                }
                None => CurrentPageStream::Removed,
            };
            let report = reconcile_source_events(
                index.generation_id(),
                materialization_id,
                reconciliation,
                &mut current_pages,
                consumer,
                &mut pending_batch,
                options.exchange_mode,
            )?;
            aggregate.pages = aggregate
                .pages
                .checked_add(report.pages)
                .ok_or_else(|| anyhow!("invalid_response: Core event page count overflowed"))?;
            aggregate.mutations = aggregate
                .mutations
                .checked_add(report.mutations)
                .ok_or_else(|| anyhow!("invalid_response: Core event mutation count overflowed"))?;
        }
        flush_event_delta_pages(consumer, &mut pending_batch)?;
        return Ok(aggregate);
    }

    let present_sources = reconciliations
        .iter()
        .filter(|item| matches!(&item.delta, CoreSourceDelta::Present(_)))
        .count();
    let workers = parallelism.min(present_sources);
    instrumentation.configured(parallelism, workers);
    if workers == 0 {
        return reconcile_ordered_source_events(
            index,
            materialization_id,
            reconciliations,
            consumer,
            OrderedReconciliationOptions {
                prefetch_parallelism: 1,
                exchange_mode: options.exchange_mode,
            },
            credits,
            instrumentation,
        );
    }

    thread::scope(|scope| {
        let jobs = Arc::new(Mutex::new(VecDeque::new()));
        let mut lanes = Vec::with_capacity(reconciliations.len());
        let mut control_lanes = Vec::with_capacity(reconciliations.len());
        for (source_ordinal, reconciliation) in reconciliations.iter().enumerate() {
            match &reconciliation.delta {
                CoreSourceDelta::Present(source) => {
                    let (sender, receiver) = sync_channel(0);
                    let (control, controls) = sync_channel(1);
                    jobs.lock()
                        .map_err(|_| anyhow!("internal: Core prefetch job lock poisoned"))?
                        .push_back(CurrentSourcePrefetchJob {
                            source_ordinal,
                            source: source.clone(),
                            lane: sender,
                            controls,
                        });
                    lanes.push(Some(PrefetchedCurrentPageLane {
                        results: receiver,
                        controls: control.clone(),
                    }));
                    control_lanes.push(Some(control));
                }
                CoreSourceDelta::Removed(_) => {
                    lanes.push(None);
                    control_lanes.push(None);
                }
            }
        }
        let controls = Arc::new(CurrentPrefetchControls {
            lanes: control_lanes,
        });
        for _ in 0..workers {
            let jobs = Arc::clone(&jobs);
            let credits = Arc::clone(credits);
            let controls = Arc::clone(&controls);
            let instrumentation = Arc::clone(instrumentation);
            scope.spawn(move || {
                run_current_source_prefetch_worker(
                    index,
                    &jobs,
                    &credits,
                    &controls,
                    &instrumentation,
                );
            });
        }

        let result = (|| {
            let mut aggregate = EventReconciliationReport {
                pages: 0,
                mutations: 0,
            };
            let mut pending_batch = EventDeltaPageBatchBuilder::new()?;
            for (source_ordinal, reconciliation) in reconciliations.into_iter().enumerate() {
                let mut current_pages = match lanes.get_mut(source_ordinal).and_then(Option::take) {
                    Some(lane) => CurrentPageStream::Prefetched {
                        source_ordinal,
                        page_index: 0,
                        lane,
                    },
                    None => CurrentPageStream::Removed,
                };
                let report = reconcile_source_events(
                    index.generation_id(),
                    materialization_id,
                    reconciliation,
                    &mut current_pages,
                    consumer,
                    &mut pending_batch,
                    options.exchange_mode,
                )?;
                aggregate.pages = aggregate
                    .pages
                    .checked_add(report.pages)
                    .ok_or_else(|| anyhow!("invalid_response: Core event page count overflowed"))?;
                aggregate.mutations = aggregate
                    .mutations
                    .checked_add(report.mutations)
                    .ok_or_else(|| {
                        anyhow!("invalid_response: Core event mutation count overflowed")
                    })?;
            }
            flush_event_delta_pages(consumer, &mut pending_batch)?;
            Ok(aggregate)
        })();
        if result.is_err() {
            cancel_current_source_prefetch(&jobs, credits, &controls);
        }
        drop(lanes);
        result
    })
}
