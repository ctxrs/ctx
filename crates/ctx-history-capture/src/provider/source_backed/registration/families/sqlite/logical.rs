use super::*;

pub(super) fn register_forgecode_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    make_selection: impl Fn(std::path::PathBuf) -> ForgeCodeSourceSelectionV0
        + Send
        + Sync
        + Clone
        + 'static,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let capture_selection = make_selection.clone();
    let hydration_selection = make_selection;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let ForgeCodeSourceBackedDiscoveryV0::Live(mut scan) =
                open_forgecode_source_backed_v0(capture_selection(capture_path.clone()))
                    .map_err(route_error)?
            else {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    "selected ForgeCode database is missing",
                ));
            };
            let route = scan.source().clone();
            sink.begin(route.source().clone())?;
            while let Some(page) = scan.next_page().map_err(route_error)? {
                for document in page.documents {
                    sink.document(document)?;
                }
            }
            sink.certify(scan.finish().map_err(route_error)?)
        },
        provider_format_scope(CaptureProvider::ForgeCode, "forgecode_sqlite"),
        move |request| {
            let ForgeCodeSourceBackedDiscoveryV0::Live(scan) =
                open_forgecode_source_backed_v0(hydration_selection(hydration_path.clone()))
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                    })?
            else {
                return Err(hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "selected ForgeCode database is missing",
                ));
            };
            ForgeCodeSourceBackedResolverV0::new([scan.source().clone()])
                .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        if selection == SourceBackedRouteSelection::Automatic {
            SourceBackedSelectorAuthority::SelectedWithRetainedExplicit
        } else {
            SourceBackedSelectorAuthority::ExplicitPath
        },
        driver,
    )?);
    Ok(())
}
pub(super) fn register_firebender_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let FirebenderSourceBackedPlan::Replacement(mut scanner) =
                prepare_firebender_source_backed(&capture_path, None).map_err(route_error)?
            else {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "cold Firebender scan unexpectedly returned an unchanged certificate",
                ));
            };
            sink.begin(scanner.source().clone())?;
            while let Some(page) = scanner.next_page().map_err(route_error)? {
                for document in page.into_documents() {
                    sink.document(document)?;
                }
            }
            sink.certify(scanner.finish().map_err(route_error)?)
        },
        provider_format_scope(
            CaptureProvider::Firebender,
            "firebender_chat_history_sqlite",
        ),
        move |request| {
            let hydrated = hydrate_firebender_source_backed_row(&hydration_path, request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: firebender_display_bytes(
                    hydrated.messages_json(),
                    hydrated.message_index(),
                )?,
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_deepagents_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let database = source.path.clone();
    let capture_database = database.clone();
    let hydration_database = database;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let mut scanner = DeepAgentsSourceBackedScannerV0::open(
                DeepAgentsDatabaseSelectionV0::explicit(capture_database.clone()),
                DateTime::<Utc>::UNIX_EPOCH,
            )
            .map_err(route_error)?;
            sink.begin(scanner.source().clone())?;
            while let Some(page) = scanner.next_page().map_err(route_error)? {
                for document in page {
                    sink.document(document)?;
                }
            }
            sink.certify(scanner.finish().map_err(route_error)?.certificate)
        },
        provider_format_scope(CaptureProvider::DeepAgents, "deepagents_sessions_sqlite"),
        move |request| {
            let hydrated = DeepAgentsLocatorResolverV0::explicit(hydration_database.clone())
                .hydrate(request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.text.into_bytes(),
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
pub(super) fn register_opencode_family_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    crate::provider::providers::opencode::native_path::source_backed::register_source_backed_route(
        registry, source, selection,
    )
}

pub(super) fn register_hermes_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    if selection != SourceBackedRouteSelection::Automatic {
        return Err(invalid_route(
            source.provider,
            "manual Hermes registration requires a persistent explicit SourceAnchor",
        ));
    }
    let candidate = HermesSourceCandidate::automatic(source.clone())
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    register_hermes_candidate(
        registry,
        source,
        selection,
        candidate,
        SourceBackedSelectorAuthority::DiscoveredWinner,
    )
}

pub(super) fn register_hermes_candidate(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    candidate: HermesSourceCandidate,
    authority: SourceBackedSelectorAuthority,
) -> SourceBackedCoordinatorResult<()> {
    let capture_candidate = candidate.clone();
    let hydration_path = candidate.path().to_path_buf();
    let driver = captured_route_driver(
        &source,
        move |sink| {
            sink.begin(capture_candidate.source().clone())?;
            let mut sink_failure = None;
            let certificate = scan_hermes_source_backed(&capture_candidate, |page| {
                for record in page.records {
                    if let HermesSourceBackedRecord::Event(document) = record {
                        if let Err(error) = sink.document(document) {
                            let detail = error.to_string();
                            sink_failure = Some(error);
                            return Err(HermesSourceBackedError::Capture(
                                CaptureError::InvalidPayload(detail),
                            ));
                        }
                    }
                }
                Ok(())
            })
            .map_err(route_error)?;
            if let Some(error) = sink_failure {
                return Err(error);
            }
            sink.certify(certificate)
        },
        provider_format_scope(CaptureProvider::Hermes, "hermes_state_sqlite"),
        move |request| {
            let hydrated = hydrate_hermes_source_backed_message(&hydration_path, request.locator())
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
        },
    );
    registry.register(executable_route(source, selection, authority, driver)?);
    Ok(())
}
pub(super) fn register_trae_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let mut began = false;
            let mut sink_failure = None;
            let scan = scan_trae_source_backed_explicit_v0(&capture_path, &mut |page| {
                for document in page.documents {
                    if !began {
                        if let Err(error) = sink.begin(document.source.clone()) {
                            let detail = error.to_string();
                            sink_failure = Some(error);
                            return Err(TraeSourceBackedErrorV0::Capture(
                                CaptureError::InvalidPayload(detail),
                            ));
                        }
                        began = true;
                    }
                    if let Err(error) = sink.document(document) {
                        let detail = error.to_string();
                        sink_failure = Some(error);
                        return Err(TraeSourceBackedErrorV0::Capture(
                            CaptureError::InvalidPayload(detail),
                        ));
                    }
                }
                Ok(())
            })
            .map_err(route_error)?;
            if let Some(error) = sink_failure {
                return Err(error);
            }
            if !began {
                sink.begin(scan.source.observation().source().clone())?;
            }
            sink.certify(scan.source)
        },
        provider_format_scope(CaptureProvider::Trae, "trae_state_vscdb"),
        move |request| {
            let hydrated =
                hydrate_trae_source_backed_locator_v0(&hydration_path, request.locator()).map_err(
                    |error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error),
                )?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.exact_text.into_bytes(),
            })
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::ExplicitPath,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_zed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let revalidation_path = path.clone();
    let hydration_path = path;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let source_key = zed_source_key().map_err(route_error)?;
            let mut snapshot = acquire_zed_snapshot(&capture_path).map_err(route_error)?;
            let snapshot_revision = snapshot.snapshot_revision.clone();
            let physical_locator = snapshot.physical_locator.clone();
            let revision_digest = zed_snapshot_revision_digest(&snapshot_revision);
            sink.begin_source(source_key.clone())
                .map_err(route_coordinator_error)?;
            let connection = snapshot.connection().map_err(route_error)?;
            let mut zed_sink = ZedSourceBackedSinkV0::new(
                sink.writer,
                connection,
                source_key.clone(),
                revision_digest,
                capture_path.to_string_lossy().into_owned(),
            )
            .map_err(route_error)?;
            let scan = scan_zed_native_snapshot(
                connection,
                &physical_locator,
                &snapshot_revision,
                &mut zed_sink,
            )
            .map_err(route_error)?;
            if let Some(error) = zed_sink.take_failure() {
                return Err(route_error(error));
            }
            let staged_documents = zed_sink.staged_documents();
            drop(zed_sink);
            snapshot.finish().map_err(route_error)?;
            if staged_documents != scan.counters.retained_events {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "Zed source-backed counts do not reconcile",
                ));
            }
            let complete_records = scan
                .counters
                .retained_events
                .checked_add(scan.counters.rejected_threads)
                .ok_or_else(|| {
                    SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "Zed source-backed counts overflowed",
                    )
                })?;
            let counts = ScannedSourceCounts {
                complete_records,
                retained_records: scan.counters.retained_events,
                rejected_records: scan.counters.rejected_threads,
                ignored_records: 0,
                indexed_documents: staged_documents,
                certified_bytes: scan.counters.certified_logical_bytes,
            };
            let observation =
                zed_source_observation(&source_key, &snapshot_revision).map_err(route_error)?;
            let certificate = CertifiedSource::certify(
                observation.clone(),
                observation,
                "zed-nativepath-source-backed-v0",
                decode_zed_digest(&scan.source_integrity_digest).map_err(route_error)?,
                counts,
            )
            .map_err(route_error)?;
            sink.certify_source(certificate)
                .map_err(route_coordinator_error)
        },
        provider_format_scope(CaptureProvider::Zed, "zed_threads_sqlite"),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                let Ok(source_key) = zed_source_key() else {
                    return false;
                };
                let Ok(mut snapshot) = acquire_zed_snapshot(&revalidation_path) else {
                    return false;
                };
                let observation = zed_source_observation(&source_key, &snapshot.snapshot_revision);
                observation.is_ok_and(|observation| observation == *expected.observation())
                    && snapshot.finish().is_ok()
            }
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
        move |request| {
            ZedLocatorResolverV0::new(&hydration_path)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
                .hydrate_event(request)
        },
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
pub(super) fn register_forgecode_selected_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    if selection != SourceBackedRouteSelection::Automatic {
        return Err(invalid_route(
            source.provider,
            "manual ForgeCode registration requires explicit catalog lineage",
        ));
    }
    register_forgecode_route(
        registry,
        source,
        selection,
        ForgeCodeSourceSelectionV0::selected,
    )
}

pub fn register_forgecode_explicit_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    register_forgecode_route(
        registry,
        source,
        SourceBackedRouteSelection::ExplicitManual,
        move |path| ForgeCodeSourceSelectionV0::explicit(path, catalog_lineage),
    )
}
