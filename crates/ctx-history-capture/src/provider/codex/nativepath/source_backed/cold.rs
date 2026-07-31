use super::*;

#[derive(Debug, Default)]
struct ColdScannerActivityV0 {
    sources_started: AtomicU64,
    sources_completed: AtomicU64,
    active_scanners: AtomicU64,
    peak_active_scanners: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
struct ColdScannerActivitySnapshotV0 {
    sources_started: u64,
    sources_completed: u64,
    active_scanners: u64,
    peak_active_scanners: u64,
}

impl ColdScannerActivityV0 {
    fn activate(&self, scanner: CodexNativeScanner) -> ActiveColdScannerV0<'_> {
        self.sources_started.fetch_add(1, AtomicOrdering::Relaxed);
        let active_scanners = self
            .active_scanners
            .fetch_add(1, AtomicOrdering::Relaxed)
            .saturating_add(1);
        self.peak_active_scanners
            .fetch_max(active_scanners, AtomicOrdering::Relaxed);
        ActiveColdScannerV0 {
            scanner: Some(scanner),
            activity: self,
        }
    }

    fn snapshot(&self) -> ColdScannerActivitySnapshotV0 {
        ColdScannerActivitySnapshotV0 {
            sources_started: self.sources_started.load(AtomicOrdering::Relaxed),
            sources_completed: self.sources_completed.load(AtomicOrdering::Relaxed),
            active_scanners: self.active_scanners.load(AtomicOrdering::Relaxed),
            peak_active_scanners: self.peak_active_scanners.load(AtomicOrdering::Relaxed),
        }
    }
}

struct ActiveColdScannerV0<'activity> {
    scanner: Option<CodexNativeScanner>,
    activity: &'activity ColdScannerActivityV0,
}

impl ActiveColdScannerV0<'_> {
    fn scanner_mut(&mut self) -> CodexSourceBackedResultV0<&mut CodexNativeScanner> {
        self.scanner
            .as_mut()
            .ok_or(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                "active scanner lost its scanner instance",
            ))
    }

    fn finish(mut self) -> CodexSourceBackedResultV0<CodexSourceScan> {
        let scanner = self
            .scanner
            .take()
            .ok_or(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                "active scanner finished more than once",
            ))?;
        let scan = scanner.finish()?;
        self.activity
            .sources_completed
            .fetch_add(1, AtomicOrdering::Relaxed);
        Ok(scan)
    }
}

impl Drop for ActiveColdScannerV0<'_> {
    fn drop(&mut self) {
        if let Some(scanner) = self.scanner.take() {
            drop(scanner);
        }
        self.activity
            .active_scanners
            .fetch_sub(1, AtomicOrdering::Relaxed);
    }
}

#[cfg(test)]
std::thread_local! {
    static LAST_COLD_SCANNER_ACTIVITY_V0: std::cell::Cell<Option<(u64, u64, u64)>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn take_cold_scanner_activity_v0() -> Option<(u64, u64, u64)> {
    LAST_COLD_SCANNER_ACTIVITY_V0.take()
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ColdParallelOptionsV0 {
    pub(super) scanner_workers: Option<usize>,
    #[cfg(test)]
    pub(super) fail_source_index: Option<usize>,
    #[cfg(test)]
    pub(super) before_commit_revalidation: Option<fn(&Path)>,
    #[cfg(test)]
    pub(super) scanner_rendezvous: Option<usize>,
}

#[derive(Debug)]
pub(super) struct ChangedSourceV0 {
    pub(super) source: CodexCatalogSource,
    pub(super) source_key: SourceKey,
    pub(super) native_session_id: String,
    pub(super) base: Option<CertifiedSource>,
    pub(super) proof: Option<CodexAppendProof>,
}

#[derive(Debug)]
struct ColdSourcePlanV0 {
    source_key: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
    base: Option<CertifiedSource>,
}

#[derive(Debug)]
struct ColdSourceJobV0 {
    source_index: usize,
    source: CodexCatalogSource,
    source_key: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
    proof: Option<CodexAppendProof>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangedSourceModeV0 {
    FullGeneration,
    AppendDelta,
}

#[derive(Debug)]
struct ChangedSourceStartV0 {
    source_index: usize,
    mode: ChangedSourceModeV0,
}

#[derive(Debug)]
struct ColdPreparedPageV0 {
    source_index: usize,
    page_index: u64,
    documents: Vec<CodexCoreDocument>,
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
    Start(ChangedSourceStartV0),
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
    mode: Option<ChangedSourceModeV0>,
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
        self.mode = None;
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
    Ok(cold_scanner_worker_count_for_parallelism(
        source_count,
        indexer_threads,
        override_workers,
        available,
    ))
}

pub(super) fn cold_scanner_worker_count_for_parallelism(
    source_count: usize,
    indexer_threads: usize,
    override_workers: Option<usize>,
    available_parallelism: usize,
) -> usize {
    let reserved = indexer_threads.clamp(1, 8).saturating_add(2);
    let automatic = available_parallelism.saturating_sub(reserved).max(1);
    override_workers
        .unwrap_or(automatic)
        .clamp(1, MAX_CODEX_SCANNER_WORKERS)
        .min(source_count.max(1))
}

pub(super) fn ingest_codex_cold_parallel_v0(
    sources: Vec<ChangedSourceV0>,
    writer: &mut GenerationWriter,
    revalidation: &mut HashMap<SourceKey, CodexTerminalSourceEvidenceV0>,
    timings: &mut CodexSourceBackedPhaseTimingsV0,
    counters: &mut CodexSourceBackedCountersV0,
    worker_count: usize,
    cold_options: ColdParallelOptionsV0,
) -> CodexSourceBackedResultV0<()> {
    let mut plans = Vec::with_capacity(sources.len());
    let mut lane_jobs = (0..worker_count).map(|_| Vec::new()).collect::<Vec<_>>();
    let mut lane_source_indices = (0..worker_count).map(|_| Vec::new()).collect::<Vec<_>>();

    for (source_index, source) in sources.into_iter().enumerate() {
        let ChangedSourceV0 {
            source,
            source_key,
            native_session_id,
            base,
            proof,
        } = source;
        let session_id = codex_session_identity(&source_key, &native_session_id)?;
        plans.push(ColdSourcePlanV0 {
            source_key: source_key.clone(),
            native_session_id: native_session_id.clone(),
            session_id,
            base: base.clone(),
        });
        let lane_index = source_index % worker_count;
        lane_source_indices[lane_index].push(source_index);
        lane_jobs[lane_index].push(ColdSourceJobV0 {
            source_index,
            source,
            source_key,
            native_session_id,
            session_id,
            proof,
        });
    }

    counters.scanner_workers =
        u64::try_from(worker_count).map_err(|_| CodexSourceBackedErrorV0::CountOverflow)?;
    let cancellation = Arc::new(AtomicBool::new(false));
    let scanner_activity = Arc::new(ColdScannerActivityV0::default());
    #[cfg(test)]
    let scanner_rendezvous = cold_options
        .scanner_rendezvous
        .map(|requested| Arc::new(std::sync::Barrier::new(requested.clamp(1, worker_count))));
    let pipeline_started = Instant::now();
    let pipeline_result = thread::scope(|scope| {
        let (failure_sender, failure_receiver) = mpsc::channel::<ColdWorkerFailureV0>();
        let mut receivers = Vec::with_capacity(worker_count);
        let mut handles = Vec::with_capacity(worker_count);

        for (lane_index, jobs) in lane_jobs.into_iter().enumerate() {
            let (sender, receiver) = mpsc::sync_channel::<ColdLaneMessageV0>(0);
            receivers.push(receiver);
            let worker_cancellation = Arc::clone(&cancellation);
            let worker_scanner_activity = Arc::clone(&scanner_activity);
            let worker_failure_sender = failure_sender.clone();
            #[cfg(test)]
            let worker_scanner_rendezvous = scanner_rendezvous.clone();
            handles.push((
                lane_index,
                scope.spawn(move || {
                    let outcome = catch_unwind(AssertUnwindSafe(|| {
                        run_cold_scan_lane_v0(
                            lane_index,
                            jobs,
                            &sender,
                            &worker_cancellation,
                            &worker_scanner_activity,
                            cold_options,
                            #[cfg(test)]
                            worker_scanner_rendezvous.as_deref(),
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
                mode: None,
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
    let activity = scanner_activity.snapshot();
    if activity.active_scanners != 0 {
        return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
            "scanner activity remained after worker joins",
        ));
    }
    counters.scanner_sources_started = counters
        .scanner_sources_started
        .checked_add(activity.sources_started)
        .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
    counters.scanner_sources_completed = counters
        .scanner_sources_completed
        .checked_add(activity.sources_completed)
        .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
    counters.peak_active_scanners = counters
        .peak_active_scanners
        .max(activity.peak_active_scanners);
    #[cfg(test)]
    LAST_COLD_SCANNER_ACTIVITY_V0.with(|last| {
        last.set(Some((
            activity.sources_started,
            activity.sources_completed,
            activity.peak_active_scanners,
        )));
    });
    pipeline_result
}

fn run_cold_scan_lane_v0(
    lane_index: usize,
    jobs: Vec<ColdSourceJobV0>,
    sender: &SyncSender<ColdLaneMessageV0>,
    cancellation: &AtomicBool,
    scanner_activity: &ColdScannerActivityV0,
    cold_options: ColdParallelOptionsV0,
    #[cfg(test)] scanner_rendezvous: Option<&std::sync::Barrier>,
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
        let (mode, scanner) = changed_source_scanner_v0(&job)?;
        let mut scanner = scanner_activity.activate(scanner);
        #[cfg(test)]
        if let Some(scanner_rendezvous) = scanner_rendezvous {
            scanner_rendezvous.wait();
        }
        worker_busy += busy_started.elapsed();
        if !send_cold_lane_message_v0(
            sender,
            ColdLaneMessageV0::Start(ChangedSourceStartV0 {
                source_index: job.source_index,
                mode,
            }),
            cancellation,
            lane_index,
        )? {
            return Ok(());
        }
        let mut page_index = 0_u64;
        let mut staged_documents = 0_u64;
        let mut repository_attributor =
            crate::repository_attribution::RepositoryAttributor::default();

        loop {
            if cancellation.load(AtomicOrdering::Acquire) {
                return Ok(());
            }
            let busy_started = Instant::now();
            let page = scanner.scanner_mut()?.next_page()?;
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
                        &mut repository_attributor,
                    )?);
                    staged_documents = staged_documents
                        .checked_add(1)
                        .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
                }
            }
            scanner.scanner_mut()?.release_transient_record_buffer();
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

fn changed_source_scanner_v0(
    job: &ColdSourceJobV0,
) -> CodexSourceBackedResultV0<(ChangedSourceModeV0, CodexNativeScanner)> {
    if let Some(proof) = job.proof.as_ref() {
        if job.source.catalog_observation.len > proof.checkpoint.observation.len {
            match CodexNativeScanner::new_source_backed_v0(job.source.clone(), Some(proof)) {
                Ok(scanner) => return Ok((ChangedSourceModeV0::AppendDelta, scanner)),
                Err(error) if invalid_changed_append_proof_v0(&error) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok((
        ChangedSourceModeV0::FullGeneration,
        CodexNativeScanner::new_source_backed_v0(job.source.clone(), None)?,
    ))
}

fn invalid_changed_append_proof_v0(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::InvalidPayload(detail)
            if detail.starts_with("invalid Codex append proof:")
    )
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
    revalidation: &mut HashMap<SourceKey, CodexTerminalSourceEvidenceV0>,
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
            ColdLaneMessageV0::Start(start) => {
                if start.source_index != expected_source
                    || lane_state.mode.is_some()
                    || lane_state.next_page != 0
                    || lane_state.staged_documents != 0
                {
                    return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "source started out of order or more than once",
                    ));
                }
                let plan = plans.get(start.source_index).ok_or(
                    CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "start references an unknown source",
                    ),
                )?;
                match start.mode {
                    ChangedSourceModeV0::FullGeneration => {
                        writer.begin_source(plan.source_key.clone())?;
                    }
                    ChangedSourceModeV0::AppendDelta => {
                        let base = plan.base.as_ref().ok_or(
                            CodexSourceBackedErrorV0::ColdProtocolMismatch(
                                "append source has no certified base",
                            ),
                        )?;
                        let writer_base = writer.begin_source_append(plan.source_key.clone())?;
                        if writer_base != base {
                            return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
                        }
                    }
                }
                counters.writer_mutated_sources = counters.writer_mutated_sources.saturating_add(1);
                lane_state.mode = Some(start.mode);
            }
            ColdLaneMessageV0::Page(page) => {
                if page.source_index != expected_source
                    || page.page_index != lane_state.next_page
                    || lane_state.mode.is_none()
                {
                    return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "source page arrived before start or out of order",
                    ));
                }
                let plan = plans.get(page.source_index).ok_or(
                    CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "page references an unknown source",
                    ),
                )?;
                for document in page.documents {
                    if !document
                        .document
                        .source
                        .exact_descriptor_eq(&plan.source_key)
                        || document.document.session_id != plan.session_id
                        || document.document.provider_session_id.as_deref()
                            != Some(plan.native_session_id.as_str())
                    {
                        return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                            "document identity does not match its assigned source",
                        ));
                    }
                    let (_, _, _, physical_ordinal) =
                        validate_codex_locator(&document.document.locator)?;
                    if physical_ordinal != document.document.event_sequence
                        || lane_state
                            .last_event_sequence
                            .is_some_and(|last| document.document.event_sequence <= last)
                    {
                        return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                            "document event sequence is not strictly increasing",
                        ));
                    }
                    let event_sequence = document.document.event_sequence;
                    let add_started = Instant::now();
                    let add_result =
                        writer.add_document_with_annotation(document.document, document.annotation);
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
                let mode =
                    lane_state
                        .mode
                        .ok_or(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                            "source completed before start",
                        ))?;
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
                let expected_disposition = match mode {
                    ChangedSourceModeV0::FullGeneration => CodexParseDisposition::FullGeneration,
                    ChangedSourceModeV0::AppendDelta => CodexParseDisposition::AppendDelta,
                };
                if complete.scan.disposition != expected_disposition
                    || complete.scan.source.catalog_native_session_id.as_deref()
                        != Some(plan.native_session_id.as_str())
                {
                    return Err(CodexSourceBackedErrorV0::ColdProtocolMismatch(
                        "changed scanner completed with the wrong source or disposition",
                    ));
                }
                let scan_counters = complete.scan.counters;
                let certification_started = Instant::now();
                match mode {
                    ChangedSourceModeV0::FullGeneration => {
                        let current = certify_scan(
                            &plan.source_key,
                            &complete.scan,
                            None,
                            complete.staged_documents,
                            scan_counters,
                        )?;
                        writer.certify_source(current)?;
                        if plan.base.is_some() {
                            counters.replaced_sources = counters.replaced_sources.saturating_add(1);
                        } else {
                            counters.cold_sources = counters.cold_sources.saturating_add(1);
                        }
                    }
                    ChangedSourceModeV0::AppendDelta => {
                        let base = plan.base.as_ref().ok_or(
                            CodexSourceBackedErrorV0::ColdProtocolMismatch(
                                "append completion has no certified base",
                            ),
                        )?;
                        let current = certify_scan(
                            &plan.source_key,
                            &complete.scan,
                            Some(base),
                            complete.staged_documents,
                            scan_counters,
                        )?;
                        let base_frontier = base
                            .frontier()
                            .ok_or(CodexSourceBackedErrorV0::MissingCheckpoint)?;
                        let append = CertifiedSourceAppend::certify(
                            base,
                            current,
                            base_frontier.certified_prefix_bytes(),
                            *base_frontier.certified_prefix_digest(),
                        )?;
                        writer.certify_source_append(append)?;
                        counters.appended_sources = counters.appended_sources.saturating_add(1);
                    }
                }
                timings.certification += certification_started.elapsed();
                timings.scanner_worker_busy += complete.worker_busy;
                counters.add_scan(scan_counters);
                counters.staged_documents = counters
                    .staged_documents
                    .checked_add(complete.staged_documents)
                    .ok_or(CodexSourceBackedErrorV0::CountOverflow)?;
                let evidence = CodexTerminalSourceEvidenceV0::new(
                    complete.scan.source,
                    complete.scan.after_observation,
                    complete.scan.before_observation.len,
                    complete.scan.full_revision_sha256,
                );
                revalidation.insert(plan.source_key.clone(), evidence);
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
