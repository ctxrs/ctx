use super::*;

/// Registers Cursor's sink-based adapter. Documents and its
/// `CertifiedSource` terminal are staged directly in the shared generation.
pub fn register_cursor_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let scan_root = root.clone();
    let revalidation_root = root.clone();
    let hydration_root = root;
    let driver = SourceBackedRouteDriver::new(
        move |sink| scan_cursor_route(&scan_root, sink),
        |source| {
            source.provider() == CaptureProvider::Cursor.as_str()
                && source.source_format() == CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
        },
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                revalidate_cursor_source(&revalidation_root, expected)
            }
            SourceBackedRevalidationTarget::Deletion(_) => false,
        },
        move |request| hydrate_cursor_route(&hydration_root, request),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
struct CursorGenerationBridge<'sink, 'writer> {
    sink: &'sink mut SourceBackedGenerationSink<'writer>,
    active: Option<SourceKey>,
}

impl CursorSourceBackedSink for CursorGenerationBridge<'_, '_> {
    fn begin_cursor_source(&mut self, plan: &CursorSourceBackedSourcePlan) -> CaptureResult<()> {
        self.sink
            .begin_source(plan.source.clone())
            .map_err(capture_coordinator_error)?;
        self.active = Some(plan.source.clone());
        Ok(())
    }

    fn stage_cursor_source_page(&mut self, page: CursorSourceBackedPage) -> CaptureResult<()> {
        for record in page.records {
            if let Some(document) = record.lexical_document() {
                self.sink
                    .add_document(document)
                    .map_err(capture_coordinator_error)?;
            }
        }
        Ok(())
    }

    fn finish_cursor_source(&mut self, terminal: CursorSourceBackedTerminal) -> CaptureResult<()> {
        let active = self.active.take().ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Cursor source-backed terminal arrived without an active source".to_owned(),
            )
        })?;
        if !active.exact_descriptor_eq(terminal.certified_source.observation().source()) {
            return Err(CaptureError::InvalidPayload(
                "Cursor source-backed terminal changed its active source".to_owned(),
            ));
        }
        self.sink
            .certify_source(terminal.certified_source)
            .map_err(capture_coordinator_error)
    }

    fn abort_cursor_source(&mut self) {
        self.active = None;
    }
}

fn scan_cursor_route(
    root: &Path,
    sink: &mut SourceBackedGenerationSink<'_>,
) -> SourceBackedRouteResult<()> {
    let mut bridge = CursorGenerationBridge { sink, active: None };
    extract_cursor_source_backed_cold(root, &mut bridge).map_err(route_capture_error)?;
    if bridge.active.is_some() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "Cursor extraction ended with an uncertified active source",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct CursorEvidenceSink {
    certificates: Vec<CertifiedSource>,
}

impl CursorSourceBackedSink for CursorEvidenceSink {
    fn begin_cursor_source(&mut self, _plan: &CursorSourceBackedSourcePlan) -> CaptureResult<()> {
        Ok(())
    }

    fn stage_cursor_source_page(&mut self, _page: CursorSourceBackedPage) -> CaptureResult<()> {
        Ok(())
    }

    fn finish_cursor_source(&mut self, terminal: CursorSourceBackedTerminal) -> CaptureResult<()> {
        self.certificates.push(terminal.certified_source);
        Ok(())
    }

    fn abort_cursor_source(&mut self) {}
}

fn revalidate_cursor_source(root: &Path, expected: &CertifiedSource) -> bool {
    let mut sink = CursorEvidenceSink::default();
    if extract_cursor_source_backed_cold(root, &mut sink).is_err() {
        return false;
    }
    sink.certificates.into_iter().any(|certificate| {
        certificate
            .observation()
            .source()
            .exact_descriptor_eq(expected.observation().source())
            && certificate == *expected
    })
}

struct CursorHydrationSink<'request> {
    request: &'request EventHydrationRequest,
    record: Option<CursorSourceBackedRecord>,
}

impl CursorSourceBackedSink for CursorHydrationSink<'_> {
    fn begin_cursor_source(&mut self, _plan: &CursorSourceBackedSourcePlan) -> CaptureResult<()> {
        Ok(())
    }

    fn stage_cursor_source_page(&mut self, page: CursorSourceBackedPage) -> CaptureResult<()> {
        for record in page.records {
            if record.event_id == self.request.event_id()
                && record.locator == *self.request.locator()
                && self.record.replace(record).is_some()
            {
                return Err(CaptureError::InvalidPayload(
                    "Cursor exact locator resolved more than once".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn finish_cursor_source(&mut self, _terminal: CursorSourceBackedTerminal) -> CaptureResult<()> {
        Ok(())
    }

    fn abort_cursor_source(&mut self) {}
}

fn hydrate_cursor_route(
    root: &Path,
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    let mut sink = CursorHydrationSink {
        request,
        record: None,
    };
    extract_cursor_source_backed_cold(root, &mut sink)
        .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))?;
    let record = sink.record.ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Cursor exact locator is absent from the selected transcript tree",
        )
    })?;
    let text = hydrate_cursor_source_backed_message(root, &record)
        .map_err(|error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error))?;
    Ok(HydratedProviderRecord {
        event_id: request.event_id(),
        provider_bytes: text.into_bytes(),
    })
}

pub(super) fn register_junie_route(
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
            let mut scanner =
                JunieSourceBackedScannerV0::discover(&capture_root, DateTime::<Utc>::UNIX_EPOCH)
                    .map_err(route_error)?;
            while let Some(emission) = scanner.next_page().map_err(route_error)? {
                match emission {
                    JunieSourceBackedEmissionV0::BeginSource(source) => sink.begin(source)?,
                    JunieSourceBackedEmissionV0::Documents(documents) => {
                        for document in documents {
                            sink.document(document)?;
                        }
                    }
                    JunieSourceBackedEmissionV0::CertifiedSource(certificate) => {
                        sink.certify(certificate)?;
                    }
                }
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Junie, "junie_session_events_jsonl_tree"),
        move |request| {
            let resolver = JunieLocatorResolverV0::discover_for_hydration(&hydration_root)?;
            resolver.hydrate_event(request)
        },
    )
    .with_batch_hydration(move |request| {
        let resolver = JunieLocatorResolverV0::discover_for_hydration(&batch_hydration_root)?;
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

pub(super) fn register_kimi_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let adapter: KimiSourceBackedResolver = KimiSourceBackedCatalog::shared();
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        adapter.into_shared(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}
pub(super) fn register_mistral_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        scan_mistral_vibe_source_backed(),
        source.path.clone(),
    );
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_openclaw_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        openclaw_source_backed_adapter_v0(),
        source.path.clone(),
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

pub(super) fn register_pi_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = match selection {
        SourceBackedRouteSelection::Automatic => {
            PiSourceBackedRoot::winning(source.path.clone())
                .map_err(|error| invalid_route(source.provider, error.to_string()))?
        }
        SourceBackedRouteSelection::ExplicitManual => {
            PiSourceBackedRoot::explicit(source.path.clone())
        }
    };
    let context = ProviderAdapterContext {
        machine_id: "source-backed-pi".to_owned(),
        source_path: Some(source.path.clone()),
        source_root: Some(source.path.clone()),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let capture_root = root.clone();
    let capture_context = context.clone();
    let hydration_root = root;
    let hydration_context = context;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            let mut begun = HashSet::new();
            let mut sink_failure = None;
            let projection = project_pi_source_backed_root_cold(
                &capture_root,
                capture_context.clone(),
                |page| {
                    if sink_failure.is_some() {
                        return;
                    }
                    if begun.insert(page.source.identity().digest()) {
                        if let Err(error) = sink.begin(page.source) {
                            sink_failure = Some(error);
                            return;
                        }
                    }
                    for document in page.documents {
                        if let Err(error) = sink.document(document) {
                            sink_failure = Some(error);
                            return;
                        }
                    }
                },
            )
            .map_err(route_error)?;
            if let Some(error) = sink_failure {
                return Err(error);
            }
            for source in projection.sources {
                if begun.insert(source.route.source.identity().digest()) {
                    sink.begin(source.route.source)?;
                }
                sink.certify(source.certificate)?;
            }
            let _inventory = projection.inventory;
            Ok(())
        },
        provider_format_scope(CaptureProvider::Pi, "pi_session_jsonl"),
        move |request| {
            let projection = project_pi_source_backed_root_cold(
                &hydration_root,
                hydration_context.clone(),
                |_| {},
            )
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            PiSourceBackedResolver::new(projection.sources.into_iter().map(|source| source.route))
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
/// Registers one caller-owned Custom History JSONL route. The path is only a
/// resolver location; `catalog_lineage` remains the durable source identity.
pub fn register_custom_history_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    let input = CustomHistorySourceBackedInput::explicit(source.path.clone(), catalog_lineage);
    let owned_source = input
        .source_key()
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let scan_input = input.clone();
    let revalidation_input = input.clone();
    let hydration_input = input;
    let claimed_source = owned_source.clone();
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let opening =
                observe_custom_history_source_backed_explicit(&scan_input).map_err(route_error)?;
            let base = sink.base_source(&claimed_source).cloned();
            if opening.is_missing() {
                let outcome =
                    scan_custom_history_source_backed_explicit(&scan_input, base.as_ref(), |_| {
                        Ok(())
                    })
                    .map_err(route_error)?;
                let CustomHistorySourceBackedOutcome::Missing {
                    inventory,
                    deletion,
                } = outcome
                else {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::SourceChanged,
                        "Custom History source appeared after its opening observation",
                    ));
                };
                if let Some(deletion) = deletion {
                    sink.delete_source(deletion, inventory)
                        .map_err(route_coordinator_error)?;
                }
                return Ok(());
            }

            sink.begin_source(claimed_source.clone())
                .map_err(route_coordinator_error)?;
            let outcome = scan_custom_history_source_backed_explicit(&scan_input, None, |page| {
                for document in page.documents {
                    sink.add_document(document)
                        .map_err(capture_coordinator_error)?;
                }
                Ok(())
            })
            .map_err(route_error)?;
            let CustomHistorySourceBackedOutcome::Present(receipt) = outcome else {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Custom History source disappeared during its replacement scan",
                ));
            };
            if !matches!(
                receipt.disposition,
                CustomHistorySourceBackedDisposition::Cold
            ) {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "cold Custom History scan returned a non-cold disposition",
                ));
            }
            sink.certify_source(receipt.certificate)
                .map_err(route_coordinator_error)
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(certificate) => {
                revalidate_custom_history_source_backed(&revalidation_input, certificate)
                    .unwrap_or(false)
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                let Ok(opening) =
                    observe_custom_history_source_backed_explicit(&revalidation_input)
                else {
                    return false;
                };
                let Ok(closing) =
                    observe_custom_history_source_backed_explicit(&revalidation_input)
                else {
                    return false;
                };
                opening
                    .certify_against(&closing)
                    .is_ok_and(|inventory| deletion.verifies(&inventory))
            }
        },
        move |request| {
            let outcome =
                scan_custom_history_source_backed_explicit(&hydration_input, None, |_| Ok(()))
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                    })?;
            let CustomHistorySourceBackedOutcome::Present(receipt) = outcome else {
                return Err(hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "the explicit Custom History source is absent",
                ));
            };
            CustomHistorySourceBackedResolver::new([receipt.route])
                .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?
                .hydrate_event(request)
        },
    );
    registry.register(SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::CatalogLineage,
        driver,
    )?);
    Ok(())
}
