use super::*;

const DIRECT_ROUTES: &[RouteEntry] = &[
    RouteEntry::new(
        CaptureProvider::Auggie,
        crate::provider::providers::auggie::native_path::register_source_backed_route,
    ),
    RouteEntry::new(
        CaptureProvider::CodeBuddy,
        crate::provider::providers::codebuddy::native_path::register_source_backed_route,
    ),
];

pub(super) fn register_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    if let Some(register) = direct_route_registration(DIRECT_ROUTES, source.provider) {
        return register(registry, source, selection);
    }
    match source.provider {
        CaptureProvider::Cline | CaptureProvider::RooCode => {
            register_task_json_route(registry, source, selection)
        }
        CaptureProvider::RovoDev => register_rovodev_route(registry, source, selection),
        CaptureProvider::Continue => register_continue_route(registry, source, selection),
        provider => Err(invalid_route(
            provider,
            "this provider is not registered by the document route family",
        )),
    }
}

pub(super) fn register_task_json_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let selected = vec![source.clone()];
    let provider = source.provider;
    let resolver = match provider {
        CaptureProvider::Cline => cline_task_json_source_backed_resolver(&selected),
        CaptureProvider::RooCode => roo_task_json_source_backed_resolver(&selected),
        _ => unreachable!("caller restricts task JSON providers"),
    }
    .map_err(|error| invalid_route(provider, error.to_string()))?;
    let adapter = match provider {
        CaptureProvider::Cline => cline_task_json_source_backed_adapter(&selected),
        CaptureProvider::RooCode => roo_task_json_source_backed_adapter(&selected),
        _ => unreachable!("caller restricts task JSON providers"),
    }
    .with_resolver(resolver);
    crate::provider::source_backed::family::document::register_replacement_document_tree_route(
        registry, source, selection, adapter,
    )
}

/// Registers one explicit NanoClaw compound project with caller-owned catalog
/// lineage.
pub fn register_nanoclaw_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    catalog_lineage: [u8; 32],
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let owned_source = nanoclaw_source_key(catalog_lineage)
        .map_err(|error| invalid_route(source.provider, error.to_string()))?;
    let driver = captured_route_driver(
        &source,
        move |sink| {
            sink.begin(owned_source.clone())?;
            let receipt =
                scan_nanoclaw_source_backed(&capture_path, catalog_lineage, |page| {
                    for document in page.documents {
                        sink.document(document).map_err(|error| {
                            crate::provider::providers::nanoclaw::native_path::source_backed::NanoClawSourceBackedError::Capture(
                                CaptureError::InvalidPayload(error.to_string()),
                            )
                        })?;
                    }
                    Ok(())
                })
                .map_err(route_error)?;
            sink.certify(receipt.source)
        },
        provider_format_scope(CaptureProvider::NanoClaw, "nanoclaw_project"),
        move |request| {
            let record = hydrate_nanoclaw_source_backed_exact(
                &hydration_path,
                catalog_lineage,
                request.locator(),
            )
            .map_err(|error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error))?;
            Ok(HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: record.text.into_bytes(),
            })
        },
    );
    registry.register(SourceBackedRoute::explicit_manual(
        source,
        SourceBackedSelectorAuthority::CatalogLineage,
        driver,
    )?);
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
