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
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            let catalog = KimiSourceBackedCatalog::discover(&capture_root).map_err(route_error)?;
            let sources = catalog.source_keys().cloned().collect::<Vec<_>>();
            for source in sources {
                sink.begin(source.clone())?;
                let certificate = catalog
                    .scan_source(&source, |document| {
                        sink.document(document).map_err(|error| {
                            crate::provider::providers::kimi::native_path::source_backed::KimiSourceBackedError::Capture(
                                CaptureError::InvalidPayload(error.to_string()),
                            )
                        })
                    })
                    .map_err(route_error)?;
                sink.certify(certificate)?;
            }
            if !catalog.revalidate_inventory().map_err(route_error)? {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::SourceChanged,
                    "Kimi catalog changed before shared publication",
                ));
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::KimiCodeCli, "kimi_code_cli_wire_jsonl"),
        move |request| {
            let catalog = KimiSourceBackedCatalog::discover(&hydration_root).map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            KimiSourceBackedResolver::new(catalog).hydrate_event(request)
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

pub(super) fn register_firebender_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let driver = captured_route_driver(
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

pub(super) fn register_mistral_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            let scan = scan_mistral_vibe_source_backed(&capture_root, DateTime::<Utc>::UNIX_EPOCH)
                .map_err(route_error)?;
            for leaf in scan.leaves {
                sink.begin(leaf.source.observation().source().clone())?;
                for document in leaf.documents {
                    sink.document(document)?;
                }
                sink.certify(leaf.source)?;
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::MistralVibe, "mistral_vibe_session_jsonl"),
        move |request| {
            let scan =
                scan_mistral_vibe_source_backed(&hydration_root, DateTime::<Utc>::UNIX_EPOCH)
                    .map_err(|error| {
                        hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                    })?;
            scan.resolver.hydrate_event(request)
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
    let registration = match source.provider {
        CaptureProvider::OpenCode => opencode_source_backed_registration(),
        CaptureProvider::Kilo => kilo_source_backed_registration(),
        CaptureProvider::MiMoCode => mimocode_source_backed_registration(),
        _ => unreachable!("caller restricts the OpenCode family"),
    };
    let path = source.path.clone();
    let capture_path = path.clone();
    let hydration_path = path;
    let provider = source.provider;
    let source_format = source.source_format;
    let driver = captured_route_driver(
        move |sink| {
            let mut began = false;
            let mut sink_failure = None;
            let scan = registration
                .scan(&capture_path, &mut |page| {
                    for document in page {
                        if !began {
                            if let Err(error) = sink.begin(document.source.clone()) {
                                let detail = error.to_string();
                                sink_failure = Some(error);
                                return Err(OpenCodeSourceBackedError::Capture(
                                    CaptureError::InvalidPayload(detail),
                                ));
                            }
                            began = true;
                        }
                        if let Err(error) = sink.document(document) {
                            let detail = error.to_string();
                            sink_failure = Some(error);
                            return Err(OpenCodeSourceBackedError::Capture(
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
                sink.begin(scan.source)?;
            }
            sink.certify(scan.certificate)
        },
        provider_format_scope(provider, source_format),
        move |request| {
            registration
                .exact_resolver(hydration_path.clone())
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

pub(super) fn register_openhands_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let root = source.path.clone();
    let capture_root = root.clone();
    let hydration_root = root;
    let driver = captured_route_driver(
        move |sink| {
            let adapter =
                OpenHandsSourceBackedAdapterV1::discover(&capture_root).map_err(route_error)?;
            let projection = adapter.project().map_err(route_error)?;
            for certificate in projection.sources() {
                sink.begin(certificate.observation().source().clone())?;
                for document in projection.documents().iter().filter(|document| {
                    document
                        .source
                        .exact_descriptor_eq(certificate.observation().source())
                }) {
                    sink.document(document.clone())?;
                }
                sink.certify(certificate.clone())?;
            }
            Ok(())
        },
        provider_format_scope(CaptureProvider::OpenHands, "openhands_file_events"),
        move |request| {
            let resolver =
                OpenHandsLocatorResolverV1::discover(&hydration_root).map_err(|error| {
                    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
                })?;
            resolver.hydrate_event(request)
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

pub(super) fn register_task_json_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let selected = vec![source.clone()];
    let capture_selected = selected.clone();
    let hydration_selected = selected;
    let provider = source.provider;
    let source_format = source.source_format;
    let driver = captured_route_driver(
        move |sink| {
            let mut adapter = match provider {
                CaptureProvider::Cline => cline_task_json_source_backed_adapter(&capture_selected),
                CaptureProvider::RooCode => roo_task_json_source_backed_adapter(&capture_selected),
                _ => unreachable!("caller restricts task JSON providers"),
            };
            if !adapter.detected_but_unsupported().is_empty() {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unsupported,
                    "the selected task directory is a detected but unsupported format",
                ));
            }
            if !adapter.unavailable().is_empty() {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    "the selected task directory is unavailable",
                ));
            }
            let mut begun = HashSet::new();
            while let Some(page) = adapter.next_page().map_err(route_error)? {
                let digest = page.source.identity().digest();
                if begun.insert(digest) {
                    sink.begin(page.source)?;
                }
                for document in page.documents {
                    sink.document(document)?;
                }
            }
            let completion = adapter.finish().map_err(route_error)?;
            if !completion.detected_but_unsupported.is_empty() {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unsupported,
                    "task discovery completed with an unsupported detected format",
                ));
            }
            if !completion.unavailable.is_empty() {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Unavailable,
                    "task discovery completed with an unavailable selected route",
                ));
            }
            for task in completion.tasks {
                if begun.insert(task.source.identity().digest()) {
                    sink.begin(task.source)?;
                }
                sink.certify(task.certified_source)?;
            }
            let _certified_inventories = completion.inventories.len();
            Ok(())
        },
        provider_format_scope(provider, source_format),
        move |request| {
            let resolver = match provider {
                CaptureProvider::Cline => {
                    cline_task_json_source_backed_resolver(&hydration_selected)
                }
                CaptureProvider::RooCode => {
                    roo_task_json_source_backed_resolver(&hydration_selected)
                }
                _ => unreachable!("caller restricts task JSON providers"),
            }
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
            })?;
            resolver.hydrate_event(request)
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
