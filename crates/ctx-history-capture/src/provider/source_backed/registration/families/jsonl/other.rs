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
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        junie_jsonl_adapter(),
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
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        mux_jsonl_adapter(),
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
    let driver = crate::provider::source_backed::family::jsonl::jsonl_family_driver(
        pi_source_backed_adapter(),
        root.path().to_path_buf(),
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
