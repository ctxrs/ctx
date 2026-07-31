use super::*;
use crate::provider_sources::EventFileInventory;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

const OPENHANDS_EVENT_FILE_FRONTIER: &str = "openhands-event-file-full-snapshot-v1";

#[derive(Clone, Default)]
struct OpenHandsParallelOptions {
    forced_worker_count: Option<usize>,
    #[cfg(test)]
    probe: Option<OpenHandsParallelTestProbe>,
}

#[cfg(test)]
#[derive(Clone)]
struct OpenHandsParallelTestProbe {
    active: Arc<std::sync::atomic::AtomicUsize>,
    peak: Arc<std::sync::atomic::AtomicUsize>,
    work: Arc<std::sync::atomic::AtomicUsize>,
    barrier: Arc<std::sync::Barrier>,
}

#[cfg(test)]
impl OpenHandsParallelTestProbe {
    fn new(worker_count: usize) -> Self {
        Self {
            active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            peak: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            work: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            barrier: Arc::new(std::sync::Barrier::new(worker_count)),
        }
    }

    fn enter(&self) -> OpenHandsParallelTestWork {
        use std::sync::atomic::Ordering;

        self.work.fetch_add(1, Ordering::Relaxed);
        let active = self.active.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        self.peak.fetch_max(active, Ordering::AcqRel);
        self.barrier.wait();
        OpenHandsParallelTestWork {
            active: Arc::clone(&self.active),
        }
    }

    fn counts(&self) -> (usize, usize, usize) {
        use std::sync::atomic::Ordering;

        (
            self.work.load(Ordering::Acquire),
            self.peak.load(Ordering::Acquire),
            self.active.load(Ordering::Acquire),
        )
    }
}

#[cfg(test)]
struct OpenHandsParallelTestWork {
    active: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl Drop for OpenHandsParallelTestWork {
    fn drop(&mut self) {
        self.active
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

#[derive(Debug)]
enum OpenHandsGroupScanMode {
    ExactReplay(Box<CertifiedSource>),
    Replacement,
}

#[derive(Debug)]
struct OpenHandsGroupScanJob {
    plan: OpenHandsEventFileSourcePlan,
    mode: OpenHandsGroupScanMode,
}

struct OpenHandsTerminalScan {
    adapter: OpenHandsEventFileAdapterV2,
    inventory: Arc<EventFileInventory>,
    certificates: BTreeMap<SourceKey, CertifiedSource>,
    complete_inventory: CertifiedSourceInventory,
}

pub(super) fn register_openhands_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    register_openhands_route_with_options(
        registry,
        source,
        selection,
        OpenHandsParallelOptions::default(),
    )
}

fn register_openhands_route_with_options(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    parallel_options: OpenHandsParallelOptions,
) -> SourceBackedCoordinatorResult<()> {
    let adapter = OpenHandsEventFileAdapterV2::new(source.path.clone());
    let scan_adapter = adapter.clone();
    let hydration_adapter = adapter.clone();
    let batch_hydration_adapter = adapter;
    let terminal_state = Arc::new(Mutex::new(None::<OpenHandsTerminalScan>));
    let scan_terminal_state = Arc::clone(&terminal_state);
    let source_terminal_state = Arc::clone(&terminal_state);
    let inventory_terminal_state = terminal_state;

    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            reset_openhands_terminal_state(&scan_terminal_state)?;
            let inventory = Arc::new(
                scan_adapter
                    .open_inventory()
                    .map_err(openhands_route_error)?,
            );
            let inventory_plan = scan_adapter
                .plan_inventory(inventory.as_ref())
                .map_err(openhands_route_error)?;
            let base_sources = sink
                .writer
                .base_manifest()
                .map(|manifest| {
                    manifest
                        .sources
                        .iter()
                        .filter(|certificate| {
                            openhands_owns_source(certificate.observation().source())
                        })
                        .map(|certificate| {
                            (
                                certificate.observation().source().clone(),
                                certificate.clone(),
                            )
                        })
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            let jobs = inventory_plan
                .source_plans()
                .iter()
                .map(|plan| {
                    let base = base_sources.get(&plan.source);
                    let mode = match base {
                        Some(base)
                            if openhands_exact_replay_matches(&scan_adapter, base, plan)? =>
                        {
                            OpenHandsGroupScanMode::ExactReplay(Box::new(base.clone()))
                        }
                        _ => OpenHandsGroupScanMode::Replacement,
                    };
                    Ok(ParallelLeafScanJob::new(
                        plan.source.clone(),
                        OpenHandsGroupScanJob {
                            plan: plan.clone(),
                            mode,
                        },
                    ))
                })
                .collect::<SourceBackedRouteResult<Vec<_>>>()?;
            let worker_count = parallel_options
                .forced_worker_count
                .unwrap_or_else(|| sink.recommended_leaf_workers(jobs.len()));
            let certificates_in_order = sink
                .run_parallel_leaf_scans(jobs, worker_count, |job, emitter| {
                    #[cfg(test)]
                    let _work = parallel_options
                        .probe
                        .as_ref()
                        .map(OpenHandsParallelTestProbe::enter);
                    run_openhands_group_scan(&scan_adapter, inventory.as_ref(), job.leaf(), emitter)
                })
                .map_err(openhands_parallel_error)?;
            let mut certificates = BTreeMap::new();
            for (plan, certificate) in inventory_plan
                .source_plans()
                .iter()
                .zip(certificates_in_order)
            {
                if !plan
                    .source
                    .exact_descriptor_eq(certificate.observation().source())
                {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::SourceChanged,
                        "OpenHands parallel result order no longer matches its planned source",
                    ));
                }
                if certificates
                    .insert(plan.source.clone(), certificate)
                    .is_some()
                {
                    return Err(openhands_internal(
                        "OpenHands parallel results duplicated an exact source",
                    ));
                }
            }

            let complete_inventory = inventory_plan.complete_inventory().clone();
            sink.certify_complete_inventory(complete_inventory.clone())
                .map_err(route_coordinator_error)?;
            for base in base_sources.values() {
                let source = base.observation().source();
                if !complete_inventory.contains(source) {
                    let deletion = CertifiedSourceDeletion::from_inventory(
                        source.clone(),
                        &complete_inventory,
                    )
                    .map_err(openhands_internal)?;
                    sink.delete_source(deletion, complete_inventory.clone())
                        .map_err(route_coordinator_error)?;
                }
            }

            let mut terminal = scan_terminal_state
                .lock()
                .map_err(|_| openhands_internal("OpenHands terminal state lock was poisoned"))?;
            *terminal = Some(OpenHandsTerminalScan {
                adapter: scan_adapter.clone(),
                inventory,
                certificates,
                complete_inventory,
            });
            Ok(())
        },
        openhands_owns_source,
        move |target| bind_openhands_target(&source_terminal_state, target),
        move |request| hydration_adapter.hydrate_event(request),
    )
    .with_complete_inventory_revalidation(move |expected| {
        revalidate_openhands_inventory(&inventory_terminal_state, expected)
    })
    .with_batch_hydration(move |request| batch_hydration_adapter.hydrate_batch(request));

    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn run_openhands_group_scan(
    adapter: &OpenHandsEventFileAdapterV2,
    inventory: &EventFileInventory,
    job: &OpenHandsGroupScanJob,
    emitter: &mut ParallelLeafScanEmitter<'_, CertifiedSource, SourceBackedRouteError>,
) -> Result<(), ParallelLeafScanWorkerError<SourceBackedRouteError>> {
    match &job.mode {
        OpenHandsGroupScanMode::ExactReplay(base) => {
            emitter.begin(ParallelLeafScanBegin::append(
                job.plan.source.clone(),
                base.as_ref().clone(),
            ))?;
            let append =
                openhands_replay_append(base).map_err(ParallelLeafScanWorkerError::provider)?;
            emitter.complete(ParallelLeafScanComplete::append(
                append,
                base.as_ref().clone(),
            ))?;
        }
        OpenHandsGroupScanMode::Replacement => {
            emitter.begin(ParallelLeafScanBegin::replace(job.plan.source.clone()))?;
            let group = inventory
                .group_at(job.plan.group_ordinal())
                .ok_or_else(|| {
                    ParallelLeafScanWorkerError::provider(openhands_internal(
                        "OpenHands group ordinal is missing",
                    ))
                })?;
            let certificate = adapter.project_replacement(group, &job.plan, emitter)?;
            let replayable = openhands_replay_certificate(&certificate)
                .map_err(ParallelLeafScanWorkerError::provider)?;
            emitter.complete(ParallelLeafScanComplete::replace(
                replayable.clone(),
                replayable,
            ))?;
        }
    }
    Ok(())
}

fn reset_openhands_terminal_state(
    state: &Mutex<Option<OpenHandsTerminalScan>>,
) -> SourceBackedRouteResult<()> {
    let mut state = state
        .lock()
        .map_err(|_| openhands_internal("OpenHands terminal state lock was poisoned"))?;
    *state = None;
    Ok(())
}

fn bind_openhands_target(
    state: &Mutex<Option<OpenHandsTerminalScan>>,
    target: SourceBackedRevalidationTarget<'_>,
) -> bool {
    let Ok(state) = state.lock() else {
        return false;
    };
    let Some(scan) = state.as_ref() else {
        return false;
    };
    match target {
        SourceBackedRevalidationTarget::Source(expected) => scan
            .certificates
            .get(expected.observation().source())
            .is_some_and(|certificate| certificate == expected),
        SourceBackedRevalidationTarget::Deletion(deletion) => {
            deletion.verifies(&scan.complete_inventory)
                && !scan.certificates.contains_key(deletion.source())
        }
    }
}

fn revalidate_openhands_inventory(
    state: &Mutex<Option<OpenHandsTerminalScan>>,
    expected: &CertifiedSourceInventory,
) -> bool {
    let Ok(state) = state.lock() else {
        return false;
    };
    let Some(scan) = state.as_ref() else {
        return false;
    };
    scan.complete_inventory == *expected
        && scan
            .adapter
            .revalidate_inventory(scan.inventory.as_ref())
            .is_ok()
}

fn openhands_exact_replay_matches(
    adapter: &OpenHandsEventFileAdapterV2,
    base: &CertifiedSource,
    plan: &OpenHandsEventFileSourcePlan,
) -> SourceBackedRouteResult<bool> {
    if base.frontier().is_none() {
        return Ok(false);
    }
    let provider_certificate = CertifiedSource::certify(
        base.observation().clone(),
        base.observation().clone(),
        base.parser_revision(),
        *base.content_digest(),
        base.counts(),
    )
    .map_err(openhands_internal)?;
    Ok(adapter.exact_replay_matches(&provider_certificate, plan))
}

fn openhands_replay_append(
    base: &CertifiedSource,
) -> SourceBackedRouteResult<CertifiedSourceAppend> {
    let frontier = base.frontier().ok_or_else(|| {
        openhands_internal("OpenHands exact-replay base has no shared replay frontier")
    })?;
    CertifiedSourceAppend::certify(
        base,
        base.clone(),
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .map_err(openhands_internal)
}

fn openhands_replay_certificate(
    certificate: &CertifiedSource,
) -> SourceBackedRouteResult<CertifiedSource> {
    let digest = *certificate.content_digest();
    let counts = certificate.counts();
    let frontier = SourceFrontier::new(
        OPENHANDS_EVENT_FILE_FRONTIER,
        TypedKey::bytes(digest.to_vec()).map_err(openhands_internal)?,
        counts.certified_bytes,
        digest,
    )
    .map_err(openhands_internal)?;
    CertifiedSource::certify_with_frontier(
        certificate.observation().clone(),
        certificate.observation().clone(),
        certificate.parser_revision(),
        digest,
        counts,
        Some(frontier),
    )
    .map_err(openhands_internal)
}

fn openhands_internal(error: impl fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

fn openhands_parallel_error(
    error: ParallelLeafScanError<SourceBackedRouteError>,
) -> SourceBackedRouteError {
    match error {
        ParallelLeafScanError::Worker { source, .. } => source,
        ParallelLeafScanError::Sink { source, .. } => route_coordinator_error(source),
        error => openhands_internal(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider_sources::count_event_file_io, ProviderCatalogSupport, ProviderImportSupport,
        ProviderSourceKind, ProviderSourceStatus, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
    };
    use ctx_history_index::{EventRecord, VerifiedIndex};
    use std::{fs, path::Path};

    #[derive(Debug, PartialEq, Eq)]
    struct OpenHandsParallelSummary {
        generation_id: String,
        indexed_documents: u64,
        sources: Vec<CertifiedSource>,
        events: Vec<Vec<EventRecord>>,
    }

    #[test]
    fn openhands_direct_route_replays_without_reopening_or_reading_bodies() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let selected = temp.path().join("openhands");
        write_message(&selected, "conversation-replay", "event-1", "stable body");
        let index = temp.path().join("index");
        let mut registry = SourceBackedProviderRegistry::new();
        register_openhands_route(
            &mut registry,
            provider_source(&selected),
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();

        let (cold, cold_io) = count_event_file_io(|| {
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap()
        });
        assert_eq!(cold.commit.indexed_documents, 1);
        assert_eq!(cold_io.inventory_opens, 1);
        assert_eq!(cold_io.inventory_walks, 2);
        assert_eq!(cold_io.body_reads, 1);
        assert_eq!(cold_io.leaf_lookups, 1);
        assert_eq!(cold_io.group_digest_builds, 2);
        assert_eq!(cold_io.inventory_digest_builds, 1);

        let (replay, replay_io) = count_event_file_io(|| {
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap()
        });
        assert_eq!(replay.commit.indexed_documents, 1);
        assert_eq!(replay_io.inventory_opens, 1);
        assert_eq!(replay_io.inventory_walks, 2);
        assert_eq!(replay_io.body_reads, 0);
        assert_eq!(replay_io.leaf_lookups, 0);
        assert_eq!(replay_io.group_digest_builds, 2);
        assert_eq!(replay_io.inventory_digest_builds, 1);
    }

    #[test]
    fn openhands_direct_route_reads_only_one_changed_group_once_per_leaf() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let selected = temp.path().join("openhands");
        let changed = write_message(&selected, "conversation-a", "event-a1", "before");
        write_message(&selected, "conversation-a", "event-a2", "stable sibling");
        write_message(&selected, "conversation-b", "event-b1", "unchanged");
        let index = temp.path().join("index");
        let mut registry = SourceBackedProviderRegistry::new();
        register_openhands_route(
            &mut registry,
            provider_source(&selected),
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();

        fs::write(
            changed,
            serde_json::to_vec(&message("event-a1", "after")).unwrap(),
        )
        .unwrap();
        let (changed, io) = count_event_file_io(|| {
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap()
        });
        assert_eq!(changed.commit.indexed_documents, 3);
        assert_eq!(io.inventory_opens, 1);
        assert_eq!(io.inventory_walks, 2);
        assert_eq!(io.body_reads, 2);
        assert_eq!(io.leaf_lookups, 2);
        assert_eq!(io.group_digest_builds, 4);
        assert_eq!(io.inventory_digest_builds, 1);
    }

    #[test]
    fn openhands_one_and_four_workers_have_exact_parity_and_bounded_work() {
        const GROUP_COUNT: usize = 8;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let selected = temp.path().join("openhands");
        for group in 0..GROUP_COUNT {
            write_message(
                &selected,
                &format!("conversation-{group:02}"),
                "event-1",
                &format!("first body {group}"),
            );
            write_message(
                &selected,
                &format!("conversation-{group:02}"),
                "event-2",
                &format!("second body {group}"),
            );
        }

        let one_probe = OpenHandsParallelTestProbe::new(1);
        let (one, one_io) = count_event_file_io(|| {
            parallel_summary(
                &selected,
                &temp.path().join("index-one"),
                1,
                one_probe.clone(),
            )
        });
        let four_probe = OpenHandsParallelTestProbe::new(4);
        let (four, four_io) = count_event_file_io(|| {
            parallel_summary(
                &selected,
                &temp.path().join("index-four"),
                4,
                four_probe.clone(),
            )
        });
        let four_again_probe = OpenHandsParallelTestProbe::new(4);
        let (four_again, four_again_io) = count_event_file_io(|| {
            parallel_summary(
                &selected,
                &temp.path().join("index-four-again"),
                4,
                four_again_probe.clone(),
            )
        });

        assert_eq!(one, four);
        assert_eq!(four, four_again);
        assert_eq!(one_probe.counts(), (GROUP_COUNT, 1, 0));
        assert_eq!(four_probe.counts(), (GROUP_COUNT, 4, 0));
        assert_eq!(four_again_probe.counts(), (GROUP_COUNT, 4, 0));
        for io in [one_io, four_io, four_again_io] {
            assert_eq!(io.inventory_opens, 1);
            assert_eq!(io.inventory_walks, 2);
            assert_eq!(io.body_reads, GROUP_COUNT * 2);
            assert_eq!(io.leaf_lookups, GROUP_COUNT * 2);
        }
    }

    #[test]
    fn openhands_changed_groups_run_concurrently_and_read_each_leaf_once() {
        const GROUP_COUNT: usize = 8;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let selected = temp.path().join("openhands");
        let mut changed = Vec::new();
        for group in 0..GROUP_COUNT {
            changed.push(write_message(
                &selected,
                &format!("conversation-{group:02}"),
                "event-1",
                "before",
            ));
            write_message(
                &selected,
                &format!("conversation-{group:02}"),
                "event-2",
                "stable sibling",
            );
        }
        let index = temp.path().join("index");
        refresh_source_backed_generation(&index, &registry(&selected), WriterOptions::default())
            .unwrap();
        for (group, path) in changed.into_iter().enumerate() {
            fs::write(
                path,
                serde_json::to_vec(&message(
                    "event-1",
                    &format!("changed body with distinct length {group}"),
                ))
                .unwrap(),
            )
            .unwrap();
        }

        let probe = OpenHandsParallelTestProbe::new(4);
        let mut changed_registry = SourceBackedProviderRegistry::new();
        register_openhands_route_with_options(
            &mut changed_registry,
            provider_source(&selected),
            SourceBackedRouteSelection::Automatic,
            OpenHandsParallelOptions {
                forced_worker_count: Some(4),
                probe: Some(probe.clone()),
            },
        )
        .unwrap();
        let (receipt, io) = count_event_file_io(|| {
            refresh_source_backed_generation(&index, &changed_registry, WriterOptions::default())
                .unwrap()
        });

        assert_eq!(receipt.commit.indexed_documents, (GROUP_COUNT * 2) as u64);
        assert_eq!(probe.counts(), (GROUP_COUNT, 4, 0));
        assert_eq!(io.inventory_opens, 1);
        assert_eq!(io.inventory_walks, 2);
        assert_eq!(io.body_reads, GROUP_COUNT * 2);
        assert_eq!(io.leaf_lookups, GROUP_COUNT * 2);
        assert_eq!(io.group_digest_builds, GROUP_COUNT * 2);
        assert_eq!(io.inventory_digest_builds, 1);
    }

    #[test]
    fn openhands_failed_terminal_duplicate_and_parse_refreshes_preserve_old_generation() {
        let temp = crate::test_support_paths::tempdir().unwrap();

        let delayed_root = temp.path().join("delayed");
        let delayed_event =
            write_message(&delayed_root, "conversation-delayed", "event-1", "before");
        let delayed_index = temp.path().join("delayed-index");
        let delayed_registry = registry(&delayed_root);
        let retained = refresh_source_backed_generation(
            &delayed_index,
            &delayed_registry,
            WriterOptions::default(),
        )
        .unwrap()
        .commit
        .generation_id;
        let delayed = delayed_event.clone();
        let result = refresh_source_backed_generation_with_progress(
            &delayed_index,
            &delayed_registry,
            WriterOptions::default(),
            |progress| {
                if progress.phase == "verifying" {
                    fs::write(
                        &delayed,
                        serde_json::to_vec(&message("event-1", "mutated after scan")).unwrap(),
                    )
                    .unwrap();
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(
            VerifiedIndex::open(&delayed_index).unwrap().generation_id(),
            retained
        );

        let duplicate_root = temp.path().join("duplicate");
        write_message(
            &duplicate_root,
            "conversation-duplicate",
            "event-1",
            "before",
        );
        let duplicate_index = temp.path().join("duplicate-index");
        let duplicate_registry = registry(&duplicate_root);
        let retained = refresh_source_backed_generation(
            &duplicate_index,
            &duplicate_registry,
            WriterOptions::default(),
        )
        .unwrap()
        .commit
        .generation_id;
        write_message(
            &duplicate_root,
            "conversation-duplicate",
            "event-1-copy",
            "duplicate",
        );
        let duplicate_path = duplicate_root
            .join("v1_conversations")
            .join("conversation-duplicate")
            .join("event-1-copy.json");
        fs::write(
            duplicate_path,
            serde_json::to_vec(&message("event-1", "duplicate")).unwrap(),
        )
        .unwrap();
        assert!(refresh_source_backed_generation(
            &duplicate_index,
            &duplicate_registry,
            WriterOptions::default(),
        )
        .is_err());
        assert_eq!(
            VerifiedIndex::open(&duplicate_index)
                .unwrap()
                .generation_id(),
            retained
        );

        let parse_root = temp.path().join("parse");
        let parse_event = write_message(&parse_root, "conversation-parse-00", "event-1", "before");
        for group in 1..8 {
            write_message(
                &parse_root,
                &format!("conversation-parse-{group:02}"),
                "event-1",
                "stable peer",
            );
        }
        let parse_index = temp.path().join("parse-index");
        let parse_registry = registry(&parse_root);
        let retained = refresh_source_backed_generation(
            &parse_index,
            &parse_registry,
            WriterOptions::default(),
        )
        .unwrap()
        .commit
        .generation_id;
        fs::write(parse_event, b"{not-json").unwrap();
        let mut failing_registry = SourceBackedProviderRegistry::new();
        register_openhands_route_with_options(
            &mut failing_registry,
            provider_source(&parse_root),
            SourceBackedRouteSelection::Automatic,
            OpenHandsParallelOptions {
                forced_worker_count: Some(4),
                probe: None,
            },
        )
        .unwrap();
        assert!(refresh_source_backed_generation(
            &parse_index,
            &failing_registry,
            WriterOptions::default(),
        )
        .is_err());
        assert_eq!(
            VerifiedIndex::open(&parse_index).unwrap().generation_id(),
            retained
        );
    }

    #[test]
    fn openhands_terminal_inventory_revalidation_is_fresh_on_every_call() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let selected = temp.path().join("openhands");
        let event = write_message(&selected, "conversation-terminal", "event-1", "before");
        let state = terminal_state(&selected);
        let inventory = state
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .complete_inventory
            .clone();

        assert!(revalidate_openhands_inventory(&state, &inventory));
        fs::write(
            &event,
            serde_json::to_vec(&message("event-1", "changed-between-callbacks")).unwrap(),
        )
        .unwrap();
        assert!(!revalidate_openhands_inventory(&state, &inventory));
        assert!(!revalidate_openhands_inventory(&state, &inventory));
    }

    fn terminal_state(selected: &Path) -> Mutex<Option<OpenHandsTerminalScan>> {
        let adapter = OpenHandsEventFileAdapterV2::new(selected);
        let inventory = Arc::new(adapter.open_inventory().unwrap());
        let complete_inventory = adapter
            .plan_inventory(inventory.as_ref())
            .unwrap()
            .complete_inventory()
            .clone();
        Mutex::new(Some(OpenHandsTerminalScan {
            adapter,
            inventory,
            certificates: BTreeMap::new(),
            complete_inventory,
        }))
    }

    fn provider_source(path: &Path) -> ProviderSource {
        ProviderSource {
            provider: CaptureProvider::OpenHands,
            path: path.to_path_buf(),
            exists: true,
            source_format: OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        }
    }

    fn registry(path: &Path) -> SourceBackedProviderRegistry {
        let mut registry = SourceBackedProviderRegistry::new();
        register_openhands_route(
            &mut registry,
            provider_source(path),
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
        registry
    }

    fn parallel_summary(
        selected: &Path,
        index: &Path,
        worker_count: usize,
        probe: OpenHandsParallelTestProbe,
    ) -> OpenHandsParallelSummary {
        let mut registry = SourceBackedProviderRegistry::new();
        register_openhands_route_with_options(
            &mut registry,
            provider_source(selected),
            SourceBackedRouteSelection::Automatic,
            OpenHandsParallelOptions {
                forced_worker_count: Some(worker_count),
                probe: Some(probe),
            },
        )
        .unwrap();
        let receipt =
            refresh_source_backed_generation(index, &registry, WriterOptions::default()).unwrap();
        let verified = VerifiedIndex::open(index).unwrap();
        let events = receipt
            .sources
            .iter()
            .map(|certificate| {
                verified
                    .source_event_page(certificate.observation().source(), None, 8)
                    .unwrap()
                    .items
            })
            .collect();
        OpenHandsParallelSummary {
            generation_id: receipt.commit.generation_id,
            indexed_documents: receipt.commit.indexed_documents,
            sources: receipt.sources,
            events,
        }
    }

    fn write_message(root: &Path, conversation: &str, id: &str, body: &str) -> std::path::PathBuf {
        let path = root
            .join("v1_conversations")
            .join(conversation)
            .join(format!("{id}.json"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec(&message(id, body)).unwrap()).unwrap();
        path
    }

    fn message(id: &str, body: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "timestamp": "2026-07-28T12:00:00Z",
            "kind": "MessageEvent",
            "source": "agent",
            "llm_message": {
                "role": "assistant",
                "content": body,
            },
        })
    }
}
