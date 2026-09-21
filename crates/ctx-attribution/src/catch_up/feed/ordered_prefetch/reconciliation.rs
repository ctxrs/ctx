use super::*;

#[expect(
    clippy::too_many_arguments,
    reason = "Snapshot reads, materializer writes, cancellation authority, and shared prefetch credits retain separate owners."
)]
pub(in super::super) fn reconcile_ordered_source_events(
    index: &dyn CoreFeedSnapshot,
    session: &mut crate::materializer::CoreMaterializationSession<'_>,
    materialization_id: &str,
    reconciliations: Vec<CoreSourceReconciliation>,
    data_root: &Path,
    cancelled: Option<&(dyn Fn() -> bool + Sync)>,
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
        let mut pending_batch = EventDeltaPageBatchBuilder::new();
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
                session,
                data_root,
                cancelled,
                reconciliation,
                &mut current_pages,
                &mut pending_batch,
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
        flush_event_delta_pages(session, data_root, cancelled, &mut pending_batch)?;
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
            session,
            materialization_id,
            reconciliations,
            data_root,
            cancelled,
            OrderedReconciliationOptions {
                prefetch_parallelism: 1,
            },
            credits,
            instrumentation,
        );
    }

    let first_present_ordinal = reconciliations
        .iter()
        .position(|item| matches!(&item.delta, CoreSourceDelta::Present(_)))
        .ok_or_else(|| anyhow!("internal: Core prefetch worker count has no present source"))?;
    credits.advance_ordered_demand(first_present_ordinal)?;

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
            let mut pending_batch = EventDeltaPageBatchBuilder::new();
            for (source_ordinal, reconciliation) in reconciliations.into_iter().enumerate() {
                let mut current_pages = match lanes.get_mut(source_ordinal).and_then(Option::take) {
                    Some(lane) => {
                        credits.advance_ordered_demand(source_ordinal)?;
                        CurrentPageStream::Prefetched {
                            source_ordinal,
                            page_index: 0,
                            lane,
                        }
                    }
                    None => CurrentPageStream::Removed,
                };
                let report = reconcile_source_events(
                    index.generation_id(),
                    materialization_id,
                    session,
                    data_root,
                    cancelled,
                    reconciliation,
                    &mut current_pages,
                    &mut pending_batch,
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
            flush_event_delta_pages(session, data_root, cancelled, &mut pending_batch)?;
            Ok(aggregate)
        })();
        if result.is_err() {
            cancel_current_source_prefetch(&jobs, credits, &controls);
        }
        drop(lanes);
        result
    })
}
