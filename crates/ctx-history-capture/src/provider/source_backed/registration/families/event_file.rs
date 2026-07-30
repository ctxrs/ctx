use super::*;
use crate::provider_sources::EventFileInventory;
use std::sync::{Arc, Mutex};

const OPENHANDS_EVENT_FILE_FRONTIER: &str = "openhands-event-file-full-snapshot-v1";

struct OpenHandsTerminalScan {
    adapter: OpenHandsEventFileAdapterV2,
    inventory: Arc<EventFileInventory>,
    certificates: Vec<CertifiedSource>,
    complete_inventory: CertifiedSourceInventory,
    revalidated: Option<bool>,
}

pub(super) fn register_openhands_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
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
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut certificates = Vec::with_capacity(inventory.groups().len());

            for group in inventory.groups() {
                let plan = scan_adapter
                    .bind_group(group)
                    .map_err(openhands_route_error)?;
                let base = base_sources.iter().find(|base| {
                    base.observation()
                        .source()
                        .exact_descriptor_eq(&plan.source)
                });
                let certificate = match base {
                    Some(base) if openhands_exact_replay_matches(&scan_adapter, base, &plan)? => {
                        stage_openhands_exact_replay(sink, base)?
                    }
                    _ => {
                        let certificate = scan_adapter
                            .project_replacement(group, &plan, sink)
                            .map_err(openhands_route_error)?;
                        let replayable = openhands_replay_certificate(&certificate)?;
                        sink.certify_source(replayable.clone())
                            .map_err(route_coordinator_error)?;
                        replayable
                    }
                };
                certificates.push(certificate);
            }

            let complete_inventory = scan_adapter
                .certify_complete_inventory(inventory.as_ref())
                .map_err(openhands_route_error)?;
            sink.certify_complete_inventory(complete_inventory.clone())
                .map_err(route_coordinator_error)?;
            for base in &base_sources {
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
                revalidated: None,
            });
            Ok(())
        },
        openhands_owns_source,
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                with_revalidated_openhands_scan(&source_terminal_state, |scan| {
                    scan.certificates
                        .iter()
                        .any(|certificate| certificate == expected)
                })
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                with_revalidated_openhands_scan(&source_terminal_state, |scan| {
                    deletion.verifies(&scan.complete_inventory)
                        && !scan.certificates.iter().any(|certificate| {
                            certificate
                                .observation()
                                .source()
                                .exact_descriptor_eq(deletion.source())
                        })
                })
            }
        },
        move |request| hydration_adapter.hydrate_event(request),
    )
    .with_complete_inventory_revalidation(move |expected| {
        with_revalidated_openhands_scan(&inventory_terminal_state, |scan| {
            scan.complete_inventory == *expected
        })
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

fn reset_openhands_terminal_state(
    state: &Mutex<Option<OpenHandsTerminalScan>>,
) -> SourceBackedRouteResult<()> {
    let mut state = state
        .lock()
        .map_err(|_| openhands_internal("OpenHands terminal state lock was poisoned"))?;
    *state = None;
    Ok(())
}

fn with_revalidated_openhands_scan(
    state: &Mutex<Option<OpenHandsTerminalScan>>,
    evaluate: impl FnOnce(&OpenHandsTerminalScan) -> bool,
) -> bool {
    let Ok(mut state) = state.lock() else {
        return false;
    };
    let Some(scan) = state.as_mut() else {
        return false;
    };
    let revalidated = match scan.revalidated {
        Some(revalidated) => revalidated,
        None => {
            let revalidated = scan
                .adapter
                .revalidate_inventory(scan.inventory.as_ref())
                .is_ok();
            scan.revalidated = Some(revalidated);
            revalidated
        }
    };
    revalidated && evaluate(scan)
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

fn stage_openhands_exact_replay(
    sink: &mut SourceBackedGenerationSink<'_>,
    base: &CertifiedSource,
) -> SourceBackedRouteResult<CertifiedSource> {
    let source = base.observation().source().clone();
    let writer_base = sink
        .begin_source_append(source)
        .map_err(route_coordinator_error)?;
    if writer_base != base {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::SourceChanged,
            "OpenHands exact-replay base changed inside the shared writer",
        ));
    }
    let frontier = base.frontier().ok_or_else(|| {
        openhands_internal("OpenHands exact-replay base has no shared replay frontier")
    })?;
    let append = CertifiedSourceAppend::certify(
        base,
        base.clone(),
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .map_err(openhands_internal)?;
    sink.certify_source_append(append)
        .map_err(route_coordinator_error)?;
    Ok(base.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        provider_sources::count_event_file_io, ProviderCatalogSupport, ProviderImportSupport,
        ProviderSourceKind, ProviderSourceStatus, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
    };
    use std::{fs, path::Path};

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
        assert_eq!(cold_io.body_reads, 1);

        let (replay, replay_io) = count_event_file_io(|| {
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap()
        });
        assert_eq!(replay.commit.indexed_documents, 1);
        assert_eq!(replay_io.inventory_opens, 1);
        assert_eq!(replay_io.body_reads, 0);
    }

    #[test]
    fn openhands_terminal_revalidation_is_cached_and_fail_closed_per_scan() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let selected = temp.path().join("openhands");
        let event = write_message(&selected, "conversation-terminal", "event-1", "before");
        let state = terminal_state(&selected);

        assert!(with_revalidated_openhands_scan(&state, |_| true));
        fs::write(
            &event,
            serde_json::to_vec(&message("event-1", "after-cache")).unwrap(),
        )
        .unwrap();
        assert!(with_revalidated_openhands_scan(&state, |_| true));

        let failed_state = terminal_state(&selected);
        fs::write(
            &event,
            serde_json::to_vec(&message("event-1", "changed-before-terminal")).unwrap(),
        )
        .unwrap();
        assert!(!with_revalidated_openhands_scan(&failed_state, |_| true));
        assert!(!with_revalidated_openhands_scan(&failed_state, |_| true));
    }

    fn terminal_state(selected: &Path) -> Mutex<Option<OpenHandsTerminalScan>> {
        let adapter = OpenHandsEventFileAdapterV2::new(selected);
        let inventory = Arc::new(adapter.open_inventory().unwrap());
        let complete_inventory = adapter
            .certify_complete_inventory(inventory.as_ref())
            .unwrap();
        Mutex::new(Some(OpenHandsTerminalScan {
            adapter,
            inventory,
            certificates: Vec::new(),
            complete_inventory,
            revalidated: None,
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
