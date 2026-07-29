use super::*;

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

pub(super) fn register_rovodev_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let context = ProviderAdapterContext {
        machine_id: "source-backed-rovodev".to_owned(),
        source_path: Some(root.clone()),
        source_root: Some(root.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let capture_root = root.clone();
    let capture_context = context.clone();
    let hydration_root = root;
    let hydration_context = context;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let inventory = discover_rovodev_source_backed(&capture_root, capture_context.clone())
                .map_err(route_error)?;
            for leaf in inventory.leaves() {
                let mut reader =
                    RovoDevSourceBackedReader::new(leaf, capture_context.clone(), None)
                        .map_err(route_error)?;
                sink.begin(leaf.source_key().clone())?;
                while let Some(page) = reader.next_page().map_err(route_error)? {
                    for document in page.documents {
                        sink.document(document)?;
                    }
                }
                let scan = reader.finish().map_err(route_error)?;
                if scan.disposition == RovoDevSourceBackedDisposition::Unchanged {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "cold Rovo Dev coordinator scan reported unchanged",
                    ));
                }
                sink.certify(scan.source)?;
            }
            let inventory = inventory.certify().map_err(route_error)?;
            sink.certify_complete_inventory(inventory)
        },
        provider_format_scope(CaptureProvider::RovoDev, "rovodev_session_json_tree"),
        move |request| {
            let inventory =
                discover_rovodev_source_backed(&hydration_root, hydration_context.clone())
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                    })?;
            let hydrated =
                hydrate_rovodev_source_record(&inventory, request.event_id(), request.locator())
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                    })?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
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

pub(super) fn register_openclaw_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let adapter = openclaw_source_backed_adapter_v0();
            for selected in adapter
                .discover_selected(&capture_root)
                .map_err(route_error)?
            {
                let base = sink.base_source(selected.source_key());
                sink.begin(selected.source_key().clone())?;
                let mut reader = adapter
                    .open_source(&selected, DateTime::<Utc>::UNIX_EPOCH, base.as_ref())
                    .map_err(route_error)?;
                while let Some(page) = reader.next_page().map_err(route_error)? {
                    for document in page.documents {
                        sink.document(document)?;
                    }
                }
                let scan = reader.finish().map_err(route_error)?;
                if scan.disposition
                    == crate::provider::providers::openclaw::OpenClawSourceBackedDispositionV0::Append
                {
                    let base = base.as_ref().ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            "OpenClaw append scan did not receive a base certificate",
                        )
                    })?;
                    let prefix = scan.verified_base_prefix.ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            "OpenClaw append scan did not return verified prefix evidence",
                        )
                    })?;
                    let append = CertifiedSourceAppend::certify(
                        base,
                        scan.certified_source,
                        prefix.bytes,
                        prefix.digest,
                    )
                    .map_err(route_error)?;
                    sink.certify_append(append)?;
                } else {
                    sink.certify(scan.certified_source)?;
                }
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::OpenClaw, "openclaw_session_jsonl_tree"),
        move |request| {
            let adapter = openclaw_source_backed_adapter_v0();
            for selected in adapter
                .discover_selected(&hydration_root)
                .map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?
            {
                if selected
                    .source_key()
                    .exact_descriptor_eq(request.locator().source())
                {
                    let hydrated =
                        adapter
                            .hydrate(&selected, request.locator())
                            .map_err(|error| {
                                hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
                            })?;
                    return Ok(HydratedProviderRecord {
                        event_id: request.event_id(),
                        provider_bytes: hydrated.provider_bytes,
                    });
                }
            }
            Err(hydration_failure(
                HydrationFailureKind::ConfirmedDeleted,
                "the exact OpenClaw source is absent from the selected inventory",
            ))
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

pub(super) fn register_continue_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let discovery = discover_continue_root(&capture_root).map_err(route_error)?;
            let mut reader = ContinueSourceBackedReader::new(&discovery).map_err(route_error)?;
            let mut begun = HashSet::new();
            while let Some(outcome) = reader.next_outcome().map_err(route_error)? {
                match outcome {
                    ContinueSourceBackedOutcome::Page(page) => {
                        if let Some(document) = page.documents.first() {
                            if begun.insert(document.source.identity().digest()) {
                                sink.begin(document.source.clone())?;
                            }
                        }
                        for document in page.documents {
                            sink.document(document)?;
                        }
                        if let Some(terminal) = page.terminal {
                            if begun.insert(terminal.source.identity().digest()) {
                                sink.begin(terminal.source)?;
                            }
                            sink.certify(terminal.certificate)?;
                        }
                    }
                    ContinueSourceBackedOutcome::Incomplete(_) => {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Unavailable,
                            "Continue selected source was incomplete",
                        ));
                    }
                    ContinueSourceBackedOutcome::Failed(_) => {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Unavailable,
                            "Continue selected source failed during bounded discovery",
                        ));
                    }
                }
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Continue, "continue_cli_sessions_json"),
        move |request| {
            let hydrated =
                hydrate_continue_source_backed_record(&hydration_root, request.locator()).map_err(
                    |error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error),
                )?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: hydrated.provider_bytes,
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

pub(super) fn register_mux_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root.clone();
    let batch_hydration_root = root;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            for candidate in
                discover_mux_source_backed_sources(&capture_root, DateTime::<Utc>::UNIX_EPOCH)
                    .map_err(route_error)?
            {
                let base = sink.base_source(candidate.source_key());
                sink.begin(candidate.source_key().clone())?;
                let receipt = scan_mux_source_backed(&candidate, base.as_ref(), |page| {
                    for record in page.records {
                        sink.document(record.document).map_err(|error| {
                            crate::provider::providers::mux::native_path::MuxSourceBackedError::Capture(
                                CaptureError::InvalidPayload(error.to_string()),
                            )
                        })?;
                    }
                    Ok(())
                })
                .map_err(route_error)?;
                match receipt.disposition {
                    MuxSourceBackedDisposition::Append { proof } => {
                        sink.certify_append(proof)?;
                    }
                    MuxSourceBackedDisposition::Cold
                    | MuxSourceBackedDisposition::Unchanged
                    | MuxSourceBackedDisposition::Replacement { .. } => {
                        sink.certify(receipt.certificate)?;
                    }
                }
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Mux, "mux_session_jsonl"),
        move |request| {
            let resolver = MuxSourceBackedResolverV0::discover_for_hydration(
                &hydration_root,
                DateTime::<Utc>::UNIX_EPOCH,
            )?;
            resolver.hydrate_event(request)
        },
    )
    .with_batch_hydration(move |request| {
        let resolver = MuxSourceBackedResolverV0::discover_for_hydration(
            &batch_hydration_root,
            DateTime::<Utc>::UNIX_EPOCH,
        )?;
        resolver.hydrate_batch(request)
    });
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
