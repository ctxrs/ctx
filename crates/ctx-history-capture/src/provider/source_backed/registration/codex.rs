use super::*;
use std::sync::Mutex;

pub(super) fn register_codex_session_tree_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let revalidation_root = root.clone();
    let complete_inventory_revalidation_root = root.clone();
    let hydration_root = root;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let opening = discover_codex_root_inventory_v0(&capture_root).map_err(route_error)?;
            sink.certify_complete_inventory(opening.certificate.clone())
                .map_err(route_coordinator_error)?;
            let base_sources = codex_writer_base_sources(sink.writer);
            for (_, source_key, _) in &opening.sources {
                sink.claim(source_key).map_err(route_coordinator_error)?;
            }
            let mut revalidation = HashMap::new();
            let mut timings = CodexSourceBackedPhaseTimingsV0::default();
            let mut counters = CodexSourceBackedCountersV0::default();
            ingest_codex_sources_serial_v0(
                opening.sources.clone(),
                &base_sources,
                sink.writer,
                &mut revalidation,
                &mut timings,
                &mut counters,
            )
            .map_err(route_error)?;
            for base in base_sources.values() {
                let base_source = base.observation().source();
                if managed_codex_session_source(base_source)
                    && !opening.certificate.contains(base_source)
                {
                    sink.delete_source(
                        CertifiedSourceDeletion::from_inventory(
                            base_source.clone(),
                            &opening.certificate,
                        )
                        .map_err(route_error)?,
                        opening.certificate.clone(),
                    )
                    .map_err(route_coordinator_error)?;
                }
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::Codex, "codex_session_jsonl"),
        move |target| {
            let Ok(inventory) = discover_codex_root_inventory_v0(&revalidation_root) else {
                return false;
            };
            match target {
                SourceBackedRevalidationTarget::Source(expected) => inventory
                    .sources
                    .iter()
                    .find(|(_, source_key, _)| {
                        source_key.exact_descriptor_eq(expected.observation().source())
                    })
                    .and_then(|(source, source_key, _)| {
                        codex_source_observation(source_key, &source.catalog_observation).ok()
                    })
                    .is_some_and(|observation| observation == *expected.observation()),
                SourceBackedRevalidationTarget::Deletion(deletion) => {
                    deletion.verifies(&inventory.certificate)
                }
            }
        },
        move |request| {
            let hydrated = CodexLocatorResolverV0::discover([&hydration_root])
                .and_then(|resolver| resolver.hydrate(request.locator()))
                .map_err(codex_locator_hydration_failure)?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: codex_display_bytes(hydrated)?,
            })
        },
    )
    .with_complete_inventory_revalidation(move |expected| {
        discover_codex_root_inventory_v0(&complete_inventory_revalidation_root)
            .is_ok_and(|current| current.certificate == *expected)
    });
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

pub(super) fn register_codex_explicit_session_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let input = CodexExplicitSessionSourceBackedInputV0::discover(&source.path)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let owned_source = input.source().clone();
    let scan_input = input.clone();
    let revalidation_input = input.clone();
    let complete_inventory_revalidation_input = input.clone();
    let hydration_input = input.clone();
    let batch_hydration_input = input;
    let claimed_source = owned_source.clone();
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            let opening = observe_codex_explicit_session_source_backed_v0(&scan_input)
                .map_err(route_error)?;
            let base = sink.base_source(&claimed_source).cloned();
            if opening.is_missing() {
                let closing = observe_codex_explicit_session_source_backed_v0(&scan_input)
                    .map_err(route_error)?;
                let inventory = opening.certify_against(&closing).map_err(route_error)?;
                sink.certify_complete_inventory(inventory.clone())
                    .map_err(route_coordinator_error)?;
                if let Some(base) = base {
                    let deletion = CertifiedSourceDeletion::from_inventory(
                        base.observation().source().clone(),
                        &inventory,
                    )
                    .map_err(route_error)?;
                    sink.delete_source(deletion, inventory)
                        .map_err(route_coordinator_error)?;
                }
                return Ok(());
            }

            let plan = opening.source_plan().ok_or_else(|| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "explicit Codex session disappeared after its opening observation",
                )
            })?;
            if !plan.1.exact_descriptor_eq(&claimed_source) {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "explicit Codex session changed its exact source identity",
                ));
            }
            sink.claim(&claimed_source)
                .map_err(route_coordinator_error)?;
            let base_sources = base
                .into_iter()
                .map(|base| (base.observation().source().clone(), base))
                .collect::<HashMap<_, _>>();
            let mut revalidation = HashMap::new();
            let mut timings = CodexSourceBackedPhaseTimingsV0::default();
            let mut counters = CodexSourceBackedCountersV0::default();
            ingest_codex_sources_serial_v0(
                vec![plan],
                &base_sources,
                sink.writer,
                &mut revalidation,
                &mut timings,
                &mut counters,
            )
            .map_err(route_error)?;
            let closing = observe_codex_explicit_session_source_backed_v0(&scan_input)
                .map_err(route_error)?;
            let inventory = opening.certify_against(&closing).map_err(route_error)?;
            sink.certify_complete_inventory(inventory)
                .map_err(route_coordinator_error)
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => {
                let Ok(inventory) =
                    observe_codex_explicit_session_source_backed_v0(&revalidation_input)
                else {
                    return false;
                };
                inventory
                    .source_plan()
                    .filter(|(_, source_key, _)| {
                        source_key.exact_descriptor_eq(expected.observation().source())
                    })
                    .and_then(|(catalog_source, source_key, _)| {
                        codex_source_observation(&source_key, &catalog_source.catalog_observation)
                            .ok()
                    })
                    .is_some_and(|observation| observation == *expected.observation())
            }
            SourceBackedRevalidationTarget::Deletion(deletion) => {
                let Ok(opening) =
                    observe_codex_explicit_session_source_backed_v0(&revalidation_input)
                else {
                    return false;
                };
                let Ok(closing) =
                    observe_codex_explicit_session_source_backed_v0(&revalidation_input)
                else {
                    return false;
                };
                opening
                    .certify_against(&closing)
                    .is_ok_and(|inventory| deletion.verifies(&inventory))
            }
        },
        move |request| hydrate_codex_explicit_event(&hydration_input, request),
    )
    .with_complete_inventory_revalidation(move |expected| {
        let Ok(opening) =
            observe_codex_explicit_session_source_backed_v0(&complete_inventory_revalidation_input)
        else {
            return false;
        };
        let Ok(closing) =
            observe_codex_explicit_session_source_backed_v0(&complete_inventory_revalidation_input)
        else {
            return false;
        };
        opening
            .certify_against(&closing)
            .is_ok_and(|current| current == *expected)
    })
    .with_batch_hydration(move |request| {
        hydrate_codex_explicit_batch(&batch_hydration_input, request)
    });
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::ExplicitPath,
        driver,
    )?);
    Ok(())
}

fn hydrate_codex_explicit_event(
    input: &CodexExplicitSessionSourceBackedInputV0,
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    codex_explicit_resolver(input)?
        .hydrate_event_request(request)
        .map_err(codex_locator_hydration_failure)
}

fn hydrate_codex_explicit_batch(
    input: &CodexExplicitSessionSourceBackedInputV0,
    request: &BatchHydrationRequest,
) -> Result<BatchHydrationResult, HydrationFailure> {
    codex_explicit_resolver(input)?
        .hydrate_batch_request(request)
        .map_err(codex_locator_hydration_failure)
}

fn codex_explicit_resolver(
    input: &CodexExplicitSessionSourceBackedInputV0,
) -> Result<CodexLocatorResolverV0, HydrationFailure> {
    let inventory = observe_codex_explicit_session_source_backed_v0(input)
        .map_err(codex_explicit_observation_hydration_failure)?;
    inventory
        .resolver()
        .map_err(codex_locator_hydration_failure)?
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::ConfirmedDeleted,
                "the explicit Codex session source is absent",
            )
        })
}

fn codex_explicit_observation_hydration_failure(
    error: CodexSourceBackedErrorV0,
) -> HydrationFailure {
    let kind = match &error {
        CodexSourceBackedErrorV0::ExplicitSourceIdentityChanged
        | CodexSourceBackedErrorV0::Capture(
            CaptureError::InvalidPayload(_) | CaptureError::SourceChangedDuringCapture,
        ) => HydrationFailureKind::StaleSourceEvidence,
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    hydration_failure(kind, error)
}

pub(in crate::provider::source_backed) fn codex_locator_hydration_failure(
    error: CodexSourceBackedErrorV0,
) -> HydrationFailure {
    let kind = match &error {
        CodexSourceBackedErrorV0::LocatorDigestMismatch
        | CodexSourceBackedErrorV0::LocatorRecordBoundaryMismatch => {
            HydrationFailureKind::StaleRecordEvidence
        }
        CodexSourceBackedErrorV0::LocatorRangeMissing => HydrationFailureKind::MissingRecord,
        CodexSourceBackedErrorV0::ExplicitSourceIdentityChanged
        | CodexSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture
            | CaptureError::InvalidProviderTranscriptPath { .. },
        ) => HydrationFailureKind::StaleSourceEvidence,
        // A generation-bound locator can outlive a temporarily missing source
        // between refreshes. Only an authoritative refresh may certify
        // deletion, so absence from the current provider catalog is
        // unavailable rather than an invalid locator.
        CodexSourceBackedErrorV0::LocatorSourceNotFound(_) => {
            HydrationFailureKind::TemporarilyUnavailable
        }
        CodexSourceBackedErrorV0::InvalidCodexLocator
        | CodexSourceBackedErrorV0::LocatorRangeTooLarge
        | CodexSourceBackedErrorV0::LocatorEventMismatch => HydrationFailureKind::InvalidLocator,
        CodexSourceBackedErrorV0::LocatorRecordNotDisplayable => {
            HydrationFailureKind::UnsupportedParserRevision
        }
        CodexSourceBackedErrorV0::Json(_) => HydrationFailureKind::StaleRecordEvidence,
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    hydration_failure(kind, error)
}

// SHA-256("ctx.codex.prompt-history.default-catalog-lineage.v0"). This is
// catalog-route identity, not a digest of the user-specific source path.
pub(in crate::provider::source_backed) const CODEX_PROMPT_HISTORY_DEFAULT_CATALOG_LINEAGE_V0: [u8;
    32] = [
    0x2d, 0x2e, 0xb3, 0x41, 0xde, 0xe9, 0x7a, 0xd3, 0x15, 0xec, 0xfa, 0xb3, 0x33, 0x20, 0x7c, 0x44,
    0x53, 0x18, 0xb9, 0x32, 0x1c, 0xc1, 0x6b, 0xf2, 0x2c, 0xdb, 0x09, 0x68, 0xe0, 0xf1, 0xf5, 0x0a,
];

/// Registers Codex's one default prompt-history catalog route while retaining
/// the opened ordinary-file authority for scanning, revalidation, and exact
/// hydration. The selected path never participates in public source identity.
pub fn register_codex_prompt_history_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let input = CodexPromptHistorySourceBackedInputV0::explicit(
        source.path.clone(),
        CODEX_PROMPT_HISTORY_DEFAULT_CATALOG_LINEAGE_V0,
    );
    let retained = observe_codex_prompt_history_source_backed_explicit_v0(&input)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let owned_source = retained.source().clone();
    let capture_source = retained.clone();
    let terminal_source = retained.clone();
    let hydration_resolver = Arc::new(
        CodexPromptHistorySourceBackedResolverV0::new([retained])
            .map_err(|error| invalid_route(source.provider, error.to_string()))?,
    );
    let claimed_source = owned_source.clone();
    let capture_route = source.clone();
    let terminal_route = source.clone();
    let terminal_evidence = Arc::new(Mutex::new(
        None::<Result<(CertifiedSource, CertifiedSourceInventory), SourceBackedRouteError>>,
    ));
    let scan_terminal_evidence = Arc::clone(&terminal_evidence);
    let source_terminal_evidence = Arc::clone(&terminal_evidence);
    let inventory_terminal_evidence = terminal_evidence;
    let terminal_capture: Arc<CodexPromptTerminalCapture> = Arc::new(move || {
        let scan =
            scan_codex_prompt_history_source_backed_v0(terminal_source.clone(), None, |_| Ok(()))
                .map_err(route_error)?;
        let inventory =
            certify_captured_route_inventory(&terminal_route, &[scan.certificate.clone()])?;
        Ok((scan.certificate, inventory))
    });
    let source_terminal_capture = Arc::clone(&terminal_capture);
    let inventory_terminal_capture = terminal_capture;
    let driver = SourceBackedRouteDriver::new(
        move |sink| {
            *scan_terminal_evidence.lock().map_err(|_| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "Codex prompt-history terminal evidence lock was poisoned",
                )
            })? = None;
            let base = sink.base_source(&claimed_source).cloned();
            let Some(base) = base else {
                sink.begin_source(claimed_source.clone())
                    .map_err(route_coordinator_error)?;
                let scan = scan_codex_prompt_history_source_backed_v0(
                    capture_source.clone(),
                    None,
                    |page| {
                        if !page.source.exact_descriptor_eq(&claimed_source) {
                            return Err(CaptureError::InvalidPayload(
                                "Codex prompt-history page changed its source descriptor"
                                    .to_owned(),
                            )
                            .into());
                        }
                        let _retained_page_bytes = page.retained_bytes;
                        for document in page.documents {
                            sink.add_document(document)
                                .map_err(capture_coordinator_error)?;
                        }
                        Ok(())
                    },
                )
                .map_err(route_error)?;
                if !matches!(
                    scan.disposition,
                    CodexPromptHistorySourceBackedDispositionV0::Cold
                ) || !scan.source.source().exact_descriptor_eq(&claimed_source)
                    || scan.certificate.counts().indexed_documents != scan.emitted_documents
                {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "Codex prompt-history cold scan did not reconcile",
                    ));
                }
                sink.certify_source(scan.certificate.clone())
                    .map_err(route_coordinator_error)?;
                let inventory =
                    certify_captured_route_inventory(&capture_route, &[scan.certificate])?;
                sink.certify_complete_inventory(inventory)
                    .map_err(route_coordinator_error)?;
                return Ok(());
            };

            let planned = scan_codex_prompt_history_source_backed_v0(
                capture_source.clone(),
                Some(&base),
                |_| Ok(()),
            )
            .map_err(route_error)?;
            if !planned.source.source().exact_descriptor_eq(&claimed_source) {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Codex prompt-history replay changed its source descriptor",
                ));
            }
            let certificate = match planned.disposition {
                CodexPromptHistorySourceBackedDispositionV0::Unchanged => {
                    if planned.certificate != base || planned.emitted_documents != 0 {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Codex prompt-history unchanged evidence did not match its base",
                        ));
                    }
                    let frontier = base.frontier().ok_or_else(|| {
                        SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::Internal,
                            "Codex prompt-history replay base has no append frontier",
                        )
                    })?;
                    sink.begin_source_append(claimed_source.clone())
                        .map_err(route_coordinator_error)?;
                    let append = CertifiedSourceAppend::certify(
                        &base,
                        planned.certificate.clone(),
                        frontier.certified_prefix_bytes(),
                        *frontier.certified_prefix_digest(),
                    )
                    .map_err(route_error)?;
                    sink.certify_source_append(append)
                        .map_err(route_coordinator_error)?;
                    planned.certificate
                }
                CodexPromptHistorySourceBackedDispositionV0::Append
                | CodexPromptHistorySourceBackedDispositionV0::Replacement => {
                    let is_append = matches!(
                        planned.disposition,
                        CodexPromptHistorySourceBackedDispositionV0::Append
                    );
                    if is_append {
                        sink.begin_source_append(claimed_source.clone())
                            .map_err(route_coordinator_error)?;
                    } else {
                        sink.begin_source(claimed_source.clone())
                            .map_err(route_coordinator_error)?;
                    }
                    let scan = scan_codex_prompt_history_source_backed_v0(
                        capture_source.clone(),
                        Some(&base),
                        |page| {
                            if !page.source.exact_descriptor_eq(&claimed_source) {
                                return Err(CaptureError::InvalidPayload(
                                    "Codex prompt-history page changed its source descriptor"
                                        .to_owned(),
                                )
                                .into());
                            }
                            let _retained_page_bytes = page.retained_bytes;
                            for document in page.documents {
                                sink.add_document(document)
                                    .map_err(capture_coordinator_error)?;
                            }
                            Ok(())
                        },
                    )
                    .map_err(route_error)?;
                    if scan.certificate != planned.certificate
                        || std::mem::discriminant(&scan.disposition)
                            != std::mem::discriminant(&planned.disposition)
                    {
                        return Err(SourceBackedRouteError::new(
                            SourceBackedRouteErrorKind::SourceChanged,
                            "Codex prompt-history changed between planning and staging",
                        ));
                    }
                    if is_append {
                        let frontier = base.frontier().ok_or_else(|| {
                            SourceBackedRouteError::new(
                                SourceBackedRouteErrorKind::Internal,
                                "Codex prompt-history append base has no frontier",
                            )
                        })?;
                        let append = CertifiedSourceAppend::certify(
                            &base,
                            scan.certificate.clone(),
                            frontier.certified_prefix_bytes(),
                            *frontier.certified_prefix_digest(),
                        )
                        .map_err(route_error)?;
                        sink.certify_source_append(append)
                            .map_err(route_coordinator_error)?;
                    } else {
                        sink.certify_source(scan.certificate.clone())
                            .map_err(route_coordinator_error)?;
                    }
                    scan.certificate
                }
                CodexPromptHistorySourceBackedDispositionV0::Cold => {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Internal,
                        "Codex prompt-history replay unexpectedly returned a cold disposition",
                    ));
                }
            };
            let inventory = certify_captured_route_inventory(&capture_route, &[certificate])?;
            sink.certify_complete_inventory(inventory)
                .map_err(route_coordinator_error)
        },
        move |candidate| candidate.exact_descriptor_eq(&owned_source),
        move |target| match target {
            SourceBackedRevalidationTarget::Source(expected) => cached_codex_prompt_evidence(
                &source_terminal_evidence,
                source_terminal_capture.as_ref(),
            )
            .is_some_and(|(current, _)| current == *expected),
            SourceBackedRevalidationTarget::Deletion(deletion) => cached_codex_prompt_evidence(
                &source_terminal_evidence,
                source_terminal_capture.as_ref(),
            )
            .is_some_and(|(_, inventory)| deletion.verifies(&inventory)),
        },
        move |request| hydration_resolver.hydrate_event(request),
    )
    .with_complete_inventory_revalidation(move |expected| {
        cached_codex_prompt_evidence(
            &inventory_terminal_evidence,
            inventory_terminal_capture.as_ref(),
        )
        .is_some_and(|(_, current)| current == *expected)
    });
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

type CodexPromptTerminalCapture = dyn Fn() -> Result<(CertifiedSource, CertifiedSourceInventory), SourceBackedRouteError>
    + Send
    + Sync;

fn cached_codex_prompt_evidence(
    cached: &Mutex<
        Option<Result<(CertifiedSource, CertifiedSourceInventory), SourceBackedRouteError>>,
    >,
    capture: &CodexPromptTerminalCapture,
) -> Option<(CertifiedSource, CertifiedSourceInventory)> {
    let mut cached = cached.lock().ok()?;
    if cached.is_none() {
        *cached = Some(capture());
    }
    cached.as_ref()?.as_ref().ok().cloned()
}
