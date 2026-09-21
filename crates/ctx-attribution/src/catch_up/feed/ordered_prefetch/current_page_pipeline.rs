use super::*;

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

pub(in super::super) struct PreparedCurrentRecord {
    pub(in super::super) record: ctx_history_core::CoreRecord,
    pub(in super::super) core_record_sha256: String,
    pub(in super::super) content_bytes: usize,
}

pub(in super::super) struct PreparedCurrentPage {
    pub(in super::super) records: Vec<PreparedCurrentRecord>,
    pub(in super::super) terminal: bool,
    pub(in super::super) _encoded_credit: EncodedPageCredit,
}

pub(in super::super) struct KeyedPreparedCurrentPage {
    pub(in super::super) source_ordinal: usize,
    pub(in super::super) page_index: u32,
    pub(in super::super) result: Result<PreparedCurrentPage>,
}

pub(in super::super) struct CurrentSourcePrefetchJob {
    pub(in super::super) source_ordinal: usize,
    pub(in super::super) source: CoreSourceState,
    pub(in super::super) lane: SyncSender<KeyedPreparedCurrentPage>,
    pub(in super::super) controls: Receiver<CurrentPrefetchControl>,
}

pub(in super::super) struct PrefetchedCurrentPageLane {
    pub(in super::super) results: Receiver<KeyedPreparedCurrentPage>,
    pub(in super::super) controls: SyncSender<CurrentPrefetchControl>,
}

#[derive(Clone, Copy)]
pub(in super::super) enum CurrentPrefetchControl {
    // Result lanes are rendezvous channels: a future source's page stays with
    // its worker and remains reclaimable until the coordinator requests it.
    Consume,
    YieldForOversize,
    Cancel,
}

pub(in super::super) struct CurrentPrefetchControls {
    pub(in super::super) lanes: Vec<Option<SyncSender<CurrentPrefetchControl>>>,
}

impl CurrentPrefetchControls {
    pub(in super::super) fn yield_later_sources(&self, source_ordinal: usize) {
        for controls in self
            .lanes
            .iter()
            .skip(source_ordinal.saturating_add(1))
            .flatten()
        {
            let _ = controls.try_send(CurrentPrefetchControl::YieldForOversize);
        }
    }

    pub(in super::super) fn cancel_all(&self) {
        for controls in self.lanes.iter().flatten() {
            let _ = controls.try_send(CurrentPrefetchControl::Cancel);
        }
    }
}

pub(in super::super) struct SequentialCurrentPageStream<'a> {
    pub(in super::super) index: &'a dyn CoreFeedSnapshot,
    pub(in super::super) source: CoreSourceState,
    pub(in super::super) cursor: Option<CoreFeedRecordCursor>,
    pub(in super::super) page_index: u32,
    pub(in super::super) credits: Arc<EncodedPageCredits>,
    pub(in super::super) instrumentation: Arc<CorePrefetchInstrumentation>,
}

pub(in super::super) enum CurrentPageStream<'a> {
    Removed,
    Sequential(Box<SequentialCurrentPageStream<'a>>),
    Prefetched {
        source_ordinal: usize,
        page_index: u32,
        lane: PrefetchedCurrentPageLane,
    },
}

impl CurrentPageStream<'_> {
    pub(in super::super) fn initially_terminal(&self) -> bool {
        matches!(self, Self::Removed)
    }

    pub(in super::super) fn next_page(&mut self) -> Result<PreparedCurrentPage> {
        match self {
            Self::Removed => bail!("internal: removed Core source requested a current page"),
            Self::Sequential(stream) => {
                let prepared = read_current_page(
                    stream.index,
                    &stream.source,
                    stream.cursor.as_ref(),
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

pub(in super::super) fn read_current_page(
    index: &dyn CoreFeedSnapshot,
    source: &CoreSourceState,
    cursor: Option<&CoreFeedRecordCursor>,
    page_budget: SnapshotPageBudget,
    credits: &Arc<EncodedPageCredits>,
    instrumentation: &Arc<CorePrefetchInstrumentation>,
    prefetch_position: Option<(usize, &CurrentPrefetchControls)>,
) -> Result<Option<(PreparedCurrentPage, Option<CoreFeedRecordCursor>)>> {
    // CoreSnapshot deliberately combines planning and materialization. Admit
    // the requested bounded page before reading it, then resize to the exact
    // immutable bytes. Rare oversized singletons use the existing ordered
    // yield path instead of serializing every ordinary reader at 64 MiB.
    instrumentation.page_planned();
    let initial_credit_bytes = page_budget.maximum_encoded_core_bytes;
    let mut encoded_credit = match prefetch_position {
        Some((source_ordinal, controls)) => {
            credits.acquire_prefetched(initial_credit_bytes, source_ordinal, controls)?
        }
        None => credits.acquire(initial_credit_bytes)?,
    };
    let Some(mut encoded_credit) = encoded_credit.take() else {
        instrumentation.cancelled();
        return Ok(None);
    };
    let _active = ActivePreparation::new(instrumentation);
    let mut source_page = index.record_page(
        &source.source,
        cursor,
        MAX_CORE_EVENT_DELTA_PAGE_ITEMS,
        page_budget,
    )?;
    instrumentation.page_materialized();
    match encoded_credit.resize_to(source_page.encoded_core_bytes, prefetch_position)? {
        EncodedCreditResize::Ready => {}
        EncodedCreditResize::Cancelled => {
            instrumentation.cancelled();
            return Ok(None);
        }
        EncodedCreditResize::RetryOrderedOversize => {
            let expected_encoded_bytes = source_page.encoded_core_bytes;
            let (source_ordinal, controls) = prefetch_position.ok_or_else(|| {
                anyhow!("internal: ordered Core prefetch retry omitted its source position")
            })?;
            drop(source_page);
            drop(encoded_credit);
            let Some(exact_credit) =
                credits.acquire_prefetched(expected_encoded_bytes, source_ordinal, controls)?
            else {
                instrumentation.cancelled();
                return Ok(None);
            };
            encoded_credit = exact_credit;
            instrumentation.page_planned();
            source_page = index.record_page(
                &source.source,
                cursor,
                MAX_CORE_EVENT_DELTA_PAGE_ITEMS,
                page_budget,
            )?;
            instrumentation.page_materialized();
            if source_page.encoded_core_bytes != expected_encoded_bytes {
                bail!("corrupt_core: pinned Core page size changed during ordered retry");
            }
        }
    }
    if source_page.generation_id != index.generation_id()
        || !source_page.source.exact_descriptor_eq(&source.source)
    {
        bail!("core_generation_mismatch: Core record page escaped its pinned generation");
    }
    if source_page.items.len() > MAX_CORE_EVENT_DELTA_PAGE_ITEMS {
        bail!("invalid_request: Core record page exceeded its item bound");
    }
    if source_page.encoded_core_bytes > page_budget.maximum_encoded_core_bytes
        && source_page.items.len() != 1
    {
        bail!(
            "invalid_request: Core record page exceeded the {}-byte attribution page encoded-payload target without singleton progress",
            page_budget.maximum_encoded_core_bytes
        );
    }
    if source_page.content_bytes > page_budget.maximum_content_bytes {
        bail!(
            "invalid_request: one Core record exceeds the {}-byte attribution page content bound",
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
            let content_bytes = item.core_record.content.encoded_content_bytes()?;
            Ok(PreparedCurrentRecord {
                record: item.core_record,
                core_record_sha256: item.core_record_sha256,
                content_bytes,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let observed_content_bytes = records.iter().try_fold(0_usize, |total, record| {
        total
            .checked_add(record.content_bytes)
            .ok_or_else(|| anyhow!("bounds: Core record page content bytes overflowed"))
    })?;
    if observed_content_bytes != source_page.content_bytes {
        bail!("corrupt_core: Core record page content-byte accounting changed");
    }
    Ok(Some((
        PreparedCurrentPage {
            records,
            terminal,
            _encoded_credit: encoded_credit,
        },
        next_cursor,
    )))
}

pub(in super::super) fn run_current_source_prefetch_worker(
    index: &dyn CoreFeedSnapshot,
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
            let prepared = read_current_page(
                index,
                &job.source,
                cursor.as_ref(),
                CORE_PREFETCH_RECORD_PAGE_BUDGET,
                credits,
                instrumentation,
                Some((job.source_ordinal, controls)),
            );
            match prepared {
                Ok(Some((page, next_cursor))) => {
                    let terminal = page.terminal;
                    let mut keyed = Some(KeyedPreparedCurrentPage {
                        source_ordinal: job.source_ordinal,
                        page_index,
                        result: Ok(page),
                    });
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

pub(in super::super) fn cancel_current_source_prefetch(
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
