use super::*;

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ColdParallelOptionsV0 {
    pub(super) scanner_workers: Option<usize>,
    #[cfg(test)]
    pub(super) fail_source_index: Option<usize>,
    #[cfg(test)]
    pub(super) before_commit_revalidation: Option<fn(&Path)>,
}

#[derive(Debug)]
struct ColdSourcePlanV0 {
    source_key: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
}

#[derive(Debug)]
struct ColdSourceJobV0 {
    source_index: usize,
    source: CodexCatalogSource,
    source_key: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
}

#[derive(Debug)]
struct ColdPreparedPageV0 {
    source_index: usize,
    page_index: u64,
    documents: Vec<LexicalDocument>,
}

#[derive(Debug)]
struct ColdSourceCompleteV0 {
    source_index: usize,
    page_count: u64,
    staged_documents: u64,
    scan: super::CodexSourceScan,
    worker_busy: Duration,
}

// Completion is emitted once per source. Boxing its 1,032-byte owned scan solely
// to match the 40-byte page message has no measured throughput benefit.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum ColdLaneMessageV0 {
    Page(ColdPreparedPageV0),
    Complete(ColdSourceCompleteV0),
}

#[derive(Debug)]
struct ColdWorkerFailureV0 {
    error: CodexSourceBackedErrorV0,
}

#[derive(Debug)]
struct ColdLaneStateV0 {
    source_indices: Vec<usize>,
    next_source: usize,
    next_page: u64,
    staged_documents: u64,
    last_event_sequence: Option<u64>,
}

impl ColdLaneStateV0 {
    fn expected_source(&self) -> Option<usize> {
        self.source_indices.get(self.next_source).copied()
    }

    fn complete_source(&mut self) {
        self.next_source = self.next_source.saturating_add(1);
        self.next_page = 0;
        self.staged_documents = 0;
        self.last_event_sequence = None;
    }
}

pub(super) fn cold_scanner_worker_count(
    source_count: u64,
    indexer_threads: usize,
    override_workers: Option<usize>,
) -> CodexSourceBackedResultV0<usize> {
    let source_count =
        usize::try_from(source_count).map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    let available = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let reserved = indexer_threads.clamp(1, 8).saturating_add(2);
    let automatic = available.saturating_sub(reserved).max(1);
    Ok(override_workers
        .unwrap_or(automatic)
        .clamp(1, MAX_CODEX_SCANNER_WORKERS)
        .min(source_count.max(1)))
}

pub(super) fn ingest_codex_cold_parallel_v0(
    sources: Vec<(CodexCatalogSource, SourceKey, String)>,
    writer: &mut GenerationWriter,
    revalidation: &mut HashMap<SourceKey, (CodexCatalogSource, CodexFileObservation)>,
    timings: &mut CodexSourceBackedPhaseTimingsV0,
    counters: &mut CodexSourceBackedCountersV0,
    worker_count: usize,
    cold_options: ColdParallelOptionsV0,
) -> CodexSourceBackedResultV0<()> {
    let mut plans = Vec::with_capacity(sources.len());
    let mut lane_jobs = (0..worker_count).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut lane_source_indices = (0..worker_count).map(|_| Vec::new()).collect::<Vec<_>>();

    for (source_index, (source, source_key, native_session_id)) in sources.into_iter().enumerate() {
        writer.begin_source(source_key.clone())?;
        let session_id = codex_session_identity(&source_key, &native_session_id)?;
        plans.push(ColdSourcePlanV0 {
            source_key: source_key.clone(),
            native_session_id: native_session_id.clone(),
            session_id,
        });
        let lane_index = source_index % worker_count;
        lane_source_indices[lane_index].push(source_index);
        lane_jobs[lane_index].push(ColdSourceJobV0 {
            source_index,
            source,
            source_key,
            native_session_id,
            session_id,
        });
    }

    counters.scanner_workers =
        u64::try_from(worker_count).map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    let cancellation = Arc::new(AtomicBool::new(false));
    let pipeline_started = Instant::now();
    let pipeline_result = thread::scope(|scope| {
        let (failure_sender, failure_receiver) = mpsc::channel::<ColdWorkerFailureV0>();
        let mut receivers = Vec::with_capacity(worker_count);
        let mut handles = Vec::with_capacity(worker_count);

        for (lane_index, jobs) in lane_jobs.into_iter().enumerate() {
            let (sender, receiver) = mpsc::sync_channel::<ColdLaneMessageV0>(0);
            receivers.push(receiver);
            let worker_cancellation = Arc::clone(&cancellation);
            let worker_failure_sender = failure_sender.clone();
            handles.push((
                lane_index,
                scope.spawn(move || {
                    let outcome = catch_unwind(AssertUnwindSafe(|| {
                        run_cold_scan_lane_v0(
                            lane_index,
                            jobs,
                            &sender,
                            &worker_cancellation,
                            cold_options,
                        )
                    }));
                    match outcome {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            let _ = worker_failure_sender.send(ColdWorkerFailureV0 { error });
                            worker_cancellation.store(true, AtomicOrdering::Release);
                        }
                        Err(_) => {
                            let _ = worker_failure_sender.send(ColdWorkerFailureV0 {
                                error: CodexSourceBackedErrorV0::ColdWorkerPanicked {
                                    lane: lane_index,
                                },
                            });
                            worker_cancellation.store(true, AtomicOrdering::Release);
                        }
                    }
                }),
            ));
        }
        drop(failure_sender);

        let mut lane_states = lane_source_indices
            .into_iter()
            .map(|source_indices| ColdLaneStateV0 {
                source_indices,
                next_source: 0,
                next_page: 0,
                staged_documents: 0,
                last_event_sequence: None,
            })
            .collect::<Vec<_>>();
        let mut result = consume_cold_lanes_v0(
            &receivers,
            &failure_receiver,
            &cancellation,
            &mut lane_states,
            &plans,
            writer,
            revalidation,
            timings,
            counters,
        );
        if result.is_err() {
            cancellation.store(true, AtomicOrdering::Release);
        }
        drop(receivers);

        let mut join_error = None;
        for (lane_index, handle) in handles {
            if handle.join().is_err() && join_error.is_none() {
                join_error =
                    Some(CodexSourceBackedErrorV0::ColdWorkerPanicked { lane: lane_index });
            }
        }
        if result.is_ok() {
            if let Ok(failure) = failure_receiver.try_recv() {
                result = Err(failure.error);
            } else if let Some(error) = join_error {
                result = Err(error);
            }
        }
        result
    });
    timings.scan_and_stage += pipeline_started.elapsed();
    pipeline_result
}

fn run_cold_scan_lane_v0(
    lane_index: usize,
    jobs: Vec<ColdSourceJobV0>,
    sender: &SyncSender<ColdLaneMessageV0>,
    cancellation: &AtomicBool,
    cold_options: ColdParallelOptionsV0,
) -> CodexSourceBackedResultV0<()> {
    for job in jobs {
        if cancellation.load(AtomicOrdering::Acquire) {
            return Ok(());
        }
        #[cfg(test)]
        if cold_options.fail_source_index == Some(job.source_index) {
            return Err(CodexSourceBackedErrorV0::InjectedColdWorkerFailure {
                source_index: job.source_index,
            });
        }
        #[cfg(not(test))]
        let _ = cold_options;

        let mut worker_busy = Duration::ZERO;
        let busy_started = Instant::now();
        let mut scanner = CodexNativeScanner::new_source_backed_v0(job.source.clone(), None)?;
        worker_busy += busy_started.elapsed();
        let mut page_index = 0_u64;
        let mut staged_documents = 0_u64;

        loop {
            if cancellation.load(AtomicOrdering::Acquire) {
                return Ok(());
            }
            let busy_started = Instant::now();
            let page = scanner.next_page()?;
            worker_busy += busy_started.elapsed();
            let Some(page) = page else {
                break;
            };
            let busy_started = Instant::now();
            let CodexNativeOwnedPage::Core(page) = page;
            if !page.core_rows.is_empty() {
                return Err(CodexSourceBackedErrorV0::UnexpectedLegacyRow);
            }
            let mut documents = Vec::with_capacity(page.source_backed_rows.len());
            if !page.source_backed_rows.is_empty() {
                let owner = page
                    .owner
                    .as_ref()
                    .ok_or(CodexSourceBackedErrorV0::MissingPageOwner)?;
                validate_owner(owner, &job.native_session_id)?;
                for row in page.source_backed_rows {
                    documents.push(codex_lexical_document(
                        &job.source,
                        &job.source_key,
                        job.session_id,
                        owner,
                        row,
                    )?);
                    staged_documents = staged_documents
                        .checked_add(1)
                        .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
                }
            }
            scanner.release_transient_record_buffer();
            worker_busy += busy_started.elapsed();
            if !send_cold_lane_message_v0(
                sender,
                ColdLaneMessageV0::Page(ColdPreparedPageV0 {
                    source_index: job.source_index,
                    page_index,
                    documents,
                }),
                cancellation,
                lane_index,
            )? {
                return Ok(());
            }
            page_index = page_index
                .checked_add(1)
                .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
        }

        if cancellation.load(AtomicOrdering::Acquire) {
            return Ok(());
        }
        let busy_started = Instant::now();
        let scan = scanner.finish()?;
        worker_busy += busy_started.elapsed();
        if !send_cold_lane_message_v0(
            sender,
            ColdLaneMessageV0::Complete(ColdSourceCompleteV0 {
                source_index: job.source_index,
                page_count: page_index,
                staged_documents,
                scan,
                worker_busy,
            }),
            cancellation,
            lane_index,
        )? {
            return Ok(());
        }
    }
    Ok(())
}

fn send_cold_lane_message_v0(
    sender: &SyncSender<ColdLaneMessageV0>,
    message: ColdLaneMessageV0,
    cancellation: &AtomicBool,
    lane_index: usize,
) -> CodexSourceBackedResultV0<bool> {
    match sender.send(message) {
        Ok(()) => Ok(true),
        Err(_) if cancellation.load(AtomicOrdering::Acquire) => Ok(false),
        Err(_) => Err(CodexSourceBackedErrorV0::ColdLaneDisconnected { lane: lane_index }),
    }
}

#[allow(clippy::too_many_arguments)]
fn consume_cold_lanes_v0(
    receivers: &[Receiver<ColdLaneMessageV0>],
    failure_receiver: &Receiver<ColdWorkerFailureV0>,
    cancellation: &AtomicBool,
    lane_states: &mut [ColdLaneStateV0],
    plans: &[ColdSourcePlanV0],
    writer: &mut GenerationWriter,
    revalidation: &mut HashMap<SourceKey, (CodexCatalogSource, CodexFileObservation)>,
    timings: &mut CodexSourceBackedPhaseTimingsV0,
    counters: &mut CodexSourceBackedCountersV0,
) -> CodexSourceBackedResultV0<()> {
    let mut completed_sources = 0_usize;
    let mut next_lane = 0_usize;
    while completed_sources < plans.len() {
        if let Ok(failure) = failure_receiver.try_recv() {
            return Err(failure.error);
        }
        if cancellation.load(AtomicOrdering::Acquire) {
            return Err(wait_for_cold_worker_failure_v0(failure_receiver)?);
        }

        let lane_index = (0..lane_states.len())
            .map(|offset| (next_lane + offset) % lane_states.len())
            .find(|lane_index| lane_states[*lane_index].expected_source().is_some())
            .ok_or(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                "no lane owns an incomplete source",
            ))?;
        next_lane = (lane_index + 1) % lane_states.len();
        let message = match receivers[lane_index].recv_timeout(COLD_LANE_RECEIVE_TIMEOUT) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                if let Ok(failure) = failure_receiver.recv_timeout(COLD_LANE_RECEIVE_TIMEOUT) {
                    return Err(failure.error);
                }
                return Err(CodexSourceBackedErrorV0::ColdLaneDisconnected { lane: lane_index });
            }
        };

        let lane_state = &mut lane_states[lane_index];
        let expected_source =
            lane_state
                .expected_source()
                .ok_or(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                    "lane emitted after completing all assigned sources",
                ))?;
        match message {
            ColdLaneMessageV0::Page(page) => {
                if page.source_index != expected_source || page.page_index != lane_state.next_page {
                    return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "source or page arrived out of order",
                    ));
                }
                let plan = plans.get(page.source_index).ok_or(
                    CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "page references an unknown source",
                    ),
                )?;
                for document in page.documents {
                    if !document.source.exact_descriptor_eq(&plan.source_key)
                        || document.session_id != plan.session_id
                        || document.provider_session_id.as_deref()
                            != Some(plan.native_session_id.as_str())
                    {
                        return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                            "document identity does not match its assigned source",
                        ));
                    }
                    let (_, _, _, physical_ordinal) = validate_codex_locator(&document.locator)?;
                    if physical_ordinal != document.event_sequence
                        || lane_state
                            .last_event_sequence
                            .is_some_and(|last| document.event_sequence <= last)
                    {
                        return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                            "document event sequence is not strictly increasing",
                        ));
                    }
                    let event_sequence = document.event_sequence;
                    let add_started = Instant::now();
                    let add_result = writer.add_document(document);
                    timings.writer_add_document += add_started.elapsed();
                    add_result?;
                    lane_state.last_event_sequence = Some(event_sequence);
                    lane_state.staged_documents = lane_state
                        .staged_documents
                        .checked_add(1)
                        .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
                }
                lane_state.next_page = lane_state
                    .next_page
                    .checked_add(1)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
            }
            ColdLaneMessageV0::Complete(complete) => {
                if complete.source_index != expected_source
                    || complete.page_count != lane_state.next_page
                    || complete.staged_documents != lane_state.staged_documents
                {
                    return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "source completion does not match accepted pages",
                    ));
                }
                let plan = plans.get(complete.source_index).ok_or(
                    CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "completion references an unknown source",
                    ),
                )?;
                if complete.scan.disposition != CodexParseDisposition::FullGeneration
                    || complete.scan.source.catalog_native_session_id.as_deref()
                        != Some(plan.native_session_id.as_str())
                {
                    return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "cold scanner completed with the wrong source or disposition",
                    ));
                }
                let scan_counters = complete.scan.counters;
                let certification_started = Instant::now();
                let current = certify_scan(
                    &plan.source_key,
                    &complete.scan,
                    None,
                    complete.staged_documents,
                    scan_counters,
                )?;
                writer.certify_source(current)?;
                timings.certification += certification_started.elapsed();
                timings.scanner_worker_busy += complete.worker_busy;
                counters.add_scan(scan_counters);
                counters.staged_documents = counters
                    .staged_documents
                    .checked_add(complete.staged_documents)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
                counters.cold_sources = counters.cold_sources.saturating_add(1);
                let after_observation = complete.scan.after_observation.clone();
                revalidation.insert(
                    plan.source_key.clone(),
                    (complete.scan.source, after_observation),
                );
                lane_state.complete_source();
                completed_sources = completed_sources
                    .checked_add(1)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
            }
        }
    }
    Ok(())
}

fn wait_for_cold_worker_failure_v0(
    failure_receiver: &Receiver<ColdWorkerFailureV0>,
) -> CodexSourceBackedResultV0<CodexSourceBackedErrorV0> {
    match failure_receiver.recv_timeout(COLD_LANE_RECEIVE_TIMEOUT) {
        Ok(failure) => Ok(failure.error),
        Err(_) => Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
            "scanner cancellation was signaled without a worker failure",
        )),
    }
}
