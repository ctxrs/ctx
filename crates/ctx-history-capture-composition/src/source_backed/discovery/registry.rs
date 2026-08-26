use super::*;

mod platform_roots;
pub(super) use platform_roots::goose_platform_root;

pub(super) fn build_automatic_source_backed_registry_from_parts_with_probes(
    probes: &StaticProviderProbeCatalog,
    discovery: &DiscoveryContext,
    data_root: &Path,
    sources: Vec<ProviderSource>,
    discovery_issues: Vec<DiscoveryIssue>,
    retained_provider_roots: &BTreeMap<String, RetainedProviderRootAuthority>,
) -> SourceBackedAutomaticRegistryBuild {
    let canonical_automatic =
        ctx_history_source_discovery::discover_canonical_automatic_provider_sources_with_context(
            probes, discovery,
        );
    let canonical_automatic_sources = canonical_automatic.sources;
    let provider_root_registrations = normalized_provider_root_registrations(
        discovery,
        &sources,
        &canonical_automatic_sources,
        data_root,
        retained_provider_roots,
    );
    let released_compound_sources =
        released_compound_root_sources(discovery, &sources, &provider_root_registrations);
    let mut registry = SourceBackedProviderRegistry::new();
    let mut issues = discovery_issues
        .into_iter()
        .map(SourceBackedAutomaticRegistryIssue::Discovery)
        .collect::<Vec<_>>();
    let mut compound_provider_registered = HashSet::new();
    let mut released_provider_root_routes =
        BTreeMap::<String, Vec<ReleasedProviderRootRoute>>::new();
    let mut codex_session_tree_sources = Vec::new();
    let mut released_configured_codex_session_tree_sources =
        BTreeMap::<String, Vec<ProviderSource>>::new();

    // A configured home is explicit desired state. Register those routes
    // before inferred routes so a retained released identity cannot make an
    // old automatic location win merely because discovery returned it first.
    let (configured_sources, automatic_sources): (Vec<_>, Vec<_>) = sources
        .into_iter()
        .partition(|source| configured_provider_root_for_source(discovery, source).is_some());
    for mut source in configured_sources.iter().cloned().chain(automatic_sources) {
        let configured_root = configured_provider_root_for_source(discovery, &source);
        let configured_source_identity = configured_root.map(|root| {
            provider_root_registrations
                .get(&root.id)
                .map(|registration| registration.source_identity)
                .unwrap_or_else(|| default_provider_root_source_identity(discovery, root))
        });
        if let Some(configured_root) = configured_root {
            if configured_source_identity == Some(ProviderRootSourceIdentity::Released) {
                if let Err(error) = restore_released_automatic_route_role(
                    &mut source,
                    configured_root,
                    &provider_root_registrations,
                ) {
                    let reason = automatic_registration_rejected(error);
                    registry.register(SourceBackedRoute::unsupported(
                        source.clone(),
                        automatic_unavailable_detail(&reason),
                    ));
                    issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
                    continue;
                }
            }
        }
        let configured_route_role = configured_root
            .and_then(|_| source.route_provenance.route_role())
            .cloned();
        if let Err(error) =
            validate_provider_source_roots_outside_data_root(data_root, std::iter::once(&source))
        {
            let reason = SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap {
                detail: error.to_string(),
            };
            registry.register(SourceBackedRoute::unsupported(
                source.clone(),
                automatic_unavailable_detail(&reason),
            ));
            issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
            continue;
        }
        if source.import_support == ProviderImportSupport::Explicit {
            continue;
        }
        if source.import_support == ProviderImportSupport::Unsupported
            || source.source_kind == ProviderSourceKind::DetectionOnly
            || source.status == ProviderSourceStatus::Unsupported
            || (source.unsupported_reason.is_some()
                && source.status != ProviderSourceStatus::Empty
                && !(configured_root.is_some() && source.status == ProviderSourceStatus::Unknown))
        {
            let detail = source
                .unsupported_reason
                .unwrap_or("the detected provider format is not supported for automatic refresh");
            retain_unsupported_automatic_format(&mut registry, &mut issues, source, detail);
            continue;
        }
        if source.status == ProviderSourceStatus::Unknown {
            let reason = SourceBackedAutomaticUnavailableReason::SourceStatus(source.status);
            let mut route = if configured_root.is_some() {
                SourceBackedRoute::unavailable_explicit(
                    source.clone(),
                    automatic_unavailable_detail(&reason),
                )
                .unwrap_or_else(|_| {
                    SourceBackedRoute::unsupported(
                        source.clone(),
                        automatic_unavailable_detail(&reason),
                    )
                })
            } else {
                SourceBackedRoute::unsupported(
                    source.clone(),
                    automatic_unavailable_detail(&reason),
                )
            };
            if let (Some(configured_root), Some(route_role)) =
                (configured_root, configured_route_role.as_ref())
            {
                let source_root_lineage = configured_source_identity
                    .and_then(|identity| identity.lineage(configured_root));
                if let Err(error) =
                    route.apply_provider_root_route_identity(source_root_lineage, route_role)
                {
                    let reason = automatic_registration_rejected(error);
                    registry.register(SourceBackedRoute::unsupported(
                        source.clone(),
                        automatic_unavailable_detail(&reason),
                    ));
                    issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { reason, source });
                    continue;
                }
            }
            registry.register(route);
            issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { reason, source });
            continue;
        }

        let Some(format_route) = landed_format_route(source.provider, source.source_format) else {
            retain_unsupported_automatic_format(
                &mut registry,
                &mut issues,
                source,
                "the discovered provider format has no landed source-backed route",
            );
            continue;
        };
        if !format_route.automatic {
            retain_unsupported_automatic_format(
                &mut registry,
                &mut issues,
                source,
                "the discovered provider format is not registered for automatic refresh",
            );
            continue;
        }
        if let Some(reason) = format_route.unsupported_reason {
            retain_unsupported_automatic_format(&mut registry, &mut issues, source, reason);
            continue;
        }

        if source.status == ProviderSourceStatus::Missing {
            if configured_source_identity == Some(ProviderRootSourceIdentity::Released)
                && released_root_uses_automatic_registration(source.provider)
            {
                // A missing moved Released root cannot reconstruct current
                // connector routes. Leave its applied membership empty so the
                // refresh merge restores the exact prior route set instead of
                // minting a configured-path identity for the missing scan path.
                let reason = SourceBackedAutomaticUnavailableReason::SourceStatus(source.status);
                issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
                continue;
            }
            // Suppress a stale missing automatic path; reappearance registers coexistence.
            if configured_root.is_none()
                && released_root_automatic_coexistence_lineage(
                    &registry,
                    discovery,
                    &provider_root_registrations,
                    &configured_sources,
                    &source,
                )
                .is_some()
            {
                continue;
            }
            let route = if configured_root.is_some() {
                let reason = SourceBackedAutomaticUnavailableReason::SourceStatus(source.status);
                SourceBackedRoute::unavailable_explicit(
                    source.clone(),
                    automatic_unavailable_detail(&reason),
                )
            } else {
                SourceBackedRoute::certified_missing(
                    source.clone(),
                    format_route.selector_authority,
                )
            };
            let route = route.and_then(|mut route| {
                if let (Some(configured_root), Some(route_role)) =
                    (configured_root, configured_route_role.as_ref())
                {
                    let source_root_lineage = configured_source_identity
                        .and_then(|identity| identity.lineage(configured_root));
                    route.apply_provider_root_route_identity(source_root_lineage, route_role)?;
                }
                Ok(route)
            });
            match route {
                Ok(route) => {
                    registry.register(route);
                    if configured_root.is_some() {
                        let reason = SourceBackedAutomaticUnavailableReason::SourceStatus(
                            ProviderSourceStatus::Missing,
                        );
                        issues.push(SourceBackedAutomaticRegistryIssue::Unavailable {
                            source,
                            reason,
                        });
                    }
                }
                Err(error) => {
                    let reason = automatic_registration_rejected(error);
                    registry.register(SourceBackedRoute::unsupported(
                        source.clone(),
                        automatic_unavailable_detail(&reason),
                    ));
                    issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
                }
            }
            continue;
        }

        if !matches!(
            source.status,
            ProviderSourceStatus::Available | ProviderSourceStatus::Empty
        ) {
            let reason = SourceBackedAutomaticUnavailableReason::SourceStatus(source.status);
            registry.register(SourceBackedRoute::unsupported(
                source.clone(),
                automatic_unavailable_detail(&reason),
            ));
            issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
            continue;
        }

        if source.status == ProviderSourceStatus::Empty {
            // Resolver diagnostics explain why a present root is empty; they do
            // not make its landed adapter unsupported.
            source.unsupported_reason = None;
        }
        if let Some(configured_root) = configured_root {
            let Some(route_role) = configured_route_role.as_ref() else {
                retain_unsupported_automatic_format(
                    &mut registry,
                    &mut issues,
                    source,
                    "the configured provider source has no explicit route role",
                );
                continue;
            };
            if configured_source_identity == Some(ProviderRootSourceIdentity::Released)
                && released_root_uses_automatic_registration(source.provider)
            {
                let identity_root = provider_root_registrations
                    .get(&configured_root.id)
                    .and_then(|registration| registration.released_identity_root.as_deref());
                let compound_provider = matches!(
                    format_route.constructor,
                    SourceBackedRouteConstructor::FiniteInventory
                        | SourceBackedRouteConstructor::DiscoveryContext
                );
                let registration = identity_root.map_or_else(
                    || {
                        Err(SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                            detail: "released provider root has no immutable automatic identity root",
                        })
                    },
                    |identity_root| {
                        register_released_provider_root_route(
                            &mut registry,
                            probes,
                            discovery,
                            data_root,
                            (configured_root, source.clone(), identity_root),
                            &released_compound_sources,
                            &provider_root_registrations,
                        )
                        .map_err(automatic_registration_rejected)
                    },
                );
                match registration {
                    Ok(route) => {
                        released_provider_root_routes
                            .entry(configured_root.id.clone())
                            .or_default()
                            .push(route);
                        if compound_provider {
                            compound_provider_registered.insert(source.provider);
                        }
                    }
                    Err(reason) => {
                        if compound_provider {
                            compound_provider_registered.insert(source.provider);
                        }
                        registry.register(SourceBackedRoute::unsupported(
                            source.clone(),
                            automatic_unavailable_detail(&reason),
                        ));
                        issues.push(SourceBackedAutomaticRegistryIssue::Unavailable {
                            source,
                            reason,
                        });
                    }
                }
                continue;
            }
            if source.provider == CaptureProvider::Codex
                && source.source_format == "codex_session_jsonl_tree"
                && configured_source_identity == Some(ProviderRootSourceIdentity::Released)
            {
                released_configured_codex_session_tree_sources
                    .entry(configured_root.id.clone())
                    .or_default()
                    .push(source);
                continue;
            }
            let source_root_lineage =
                configured_source_identity.and_then(|identity| identity.lineage(configured_root));
            let registration = match (source.provider, source.source_format) {
                (CaptureProvider::Claude, "claude_projects_jsonl_tree") => {
                    register_configured_claude_source_backed_route(
                        &mut registry,
                        source.clone(),
                        SourceBackedRouteSelection::ExplicitManual,
                        source_root_lineage,
                        route_role,
                    )
                }
                (CaptureProvider::Codex, "codex_session_jsonl_tree") => {
                    register_configured_codex_session_tree_route(
                        &mut registry,
                        source.clone(),
                        SourceBackedRouteSelection::ExplicitManual,
                        source_root_lineage,
                        route_role,
                    )
                }
                (CaptureProvider::Codex, "codex_history_jsonl") => {
                    register_configured_codex_prompt_history_source_backed_route(
                        &mut registry,
                        source.clone(),
                        SourceBackedRouteSelection::ExplicitManual,
                        source_root_lineage,
                        route_role,
                    )
                }
                (CaptureProvider::Crush, "crush_sqlite")
                | (CaptureProvider::Goose, "goose_sessions_sqlite")
                | (CaptureProvider::AstrBot, "astrbot_data_v4_sqlite")
                | (CaptureProvider::Lingma, "lingma_sqlite")
                | (CaptureProvider::Warp, "warp_sqlite") => register_configured_compound_route(
                    &mut registry,
                    discovery,
                    configured_root,
                    source.clone(),
                    data_root,
                    source_root_lineage,
                    route_role,
                ),
                _ => register_configured_landed_source_backed_route(
                    &mut registry,
                    source.clone(),
                    data_root,
                    source_root_lineage,
                    route_role,
                ),
            };
            match registration {
                Ok(()) => {}
                Err(error) => {
                    let reason = automatic_registration_rejected(error);
                    registry.register(SourceBackedRoute::unsupported(
                        source.clone(),
                        automatic_unavailable_detail(&reason),
                    ));
                    issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
                }
            }
            continue;
        }
        if source.provider == CaptureProvider::Codex
            && source.source_format == "codex_session_jsonl_tree"
        {
            codex_session_tree_sources.push(source);
            continue;
        }

        let compound_provider = matches!(
            format_route.constructor,
            SourceBackedRouteConstructor::FiniteInventory
                | SourceBackedRouteConstructor::DiscoveryContext
        );
        let coexistence_lineage = released_root_automatic_coexistence_lineage(
            &registry,
            discovery,
            &provider_root_registrations,
            &configured_sources,
            &source,
        );
        if compound_provider
            && compound_provider_registered.contains(&source.provider)
            && (coexistence_lineage.is_none()
                || matches!(
                    source.provider,
                    CaptureProvider::Crush | CaptureProvider::Lingma
                ))
        {
            continue;
        }
        match register_discovered_automatic_route(
            &mut registry,
            probes,
            discovery,
            data_root,
            format_route,
            source.clone(),
            (
                coexistence_lineage,
                released_compound_inventory_coverage(
                    source.provider,
                    discovery,
                    &released_compound_sources,
                    &provider_root_registrations,
                ),
            ),
        ) {
            Ok(()) => {
                if compound_provider {
                    compound_provider_registered.insert(source.provider);
                }
            }
            Err(reason) => {
                if compound_provider {
                    compound_provider_registered.insert(source.provider);
                }
                registry.register(SourceBackedRoute::unsupported(
                    source.clone(),
                    automatic_unavailable_detail(&reason),
                ));
                issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
            }
        }
    }

    for sources in released_configured_codex_session_tree_sources.into_values() {
        let source = sources.first().cloned();
        let route_role = source
            .as_ref()
            .and_then(|source| source.route_provenance.route_role())
            .cloned();
        let registration = route_role.as_ref().map_or_else(
            || {
                Err(invalid_route(
                    CaptureProvider::Codex,
                    "configured Codex session routes have no explicit route role",
                ))
            },
            |route_role| {
                register_configured_codex_session_tree_routes(
                    &mut registry,
                    sources,
                    SourceBackedRouteSelection::ExplicitManual,
                    None,
                    route_role,
                )
            },
        );
        if let (Some(source), Err(error)) = (source, registration) {
            let reason = automatic_registration_rejected(error);
            registry.register(SourceBackedRoute::unsupported(
                source.clone(),
                automatic_unavailable_detail(&reason),
            ));
            issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
        }
    }

    if !codex_session_tree_sources.is_empty() {
        codex_session_tree_sources.sort_by(|left, right| {
            codex_automatic_session_root_rank(&left.path)
                .cmp(&codex_automatic_session_root_rank(&right.path))
                .then_with(|| left.path.cmp(&right.path))
        });
        codex_session_tree_sources.dedup_by(|left, right| left.path == right.path);
        let source = codex_session_tree_sources.first().cloned();
        let registration = register_codex_session_tree_routes(
            &mut registry,
            codex_session_tree_sources,
            SourceBackedRouteSelection::Automatic,
        );
        if let (Some(source), Err(error)) = (source, registration) {
            let reason = automatic_registration_rejected(error);
            registry.register(SourceBackedRoute::unsupported(
                source.clone(),
                automatic_unavailable_detail(&reason),
            ));
            issues.push(SourceBackedAutomaticRegistryIssue::Unavailable { source, reason });
        }
    }

    let definitions = discovery.configured_provider_roots().to_vec();
    let applied_roots = applied_provider_roots(
        discovery,
        &registry,
        &provider_root_registrations,
        &released_provider_root_routes,
    );
    match applied_roots {
        Ok(applied_roots) => {
            if let Err(error) = registry.set_applied_provider_roots(
                discovery.automatic_provider_discovery_enabled(),
                provider_source_config_digest(
                    discovery.automatic_provider_discovery_enabled(),
                    &definitions,
                ),
                applied_roots,
            ) {
                if let Some(source) = registry
                    .routes
                    .first()
                    .map(|route| route.metadata.source.clone())
                {
                    issues.push(SourceBackedAutomaticRegistryIssue::Unavailable {
                        source,
                        reason: SourceBackedAutomaticUnavailableReason::RegistrationRejected {
                            kind: SourceBackedRouteErrorKind::Internal,
                            detail: error.to_string(),
                        },
                    });
                }
            }
        }
        Err(error) => {
            if let Some(source) = registry
                .routes
                .first()
                .map(|route| route.metadata.source.clone())
            {
                issues.push(SourceBackedAutomaticRegistryIssue::Unavailable {
                    source,
                    reason: SourceBackedAutomaticUnavailableReason::RegistrationRejected {
                        kind: SourceBackedRouteErrorKind::Internal,
                        detail: error.to_string(),
                    },
                });
            }
        }
    }

    SourceBackedAutomaticRegistryBuild {
        registry,
        issues,
        discovery_duration: Duration::ZERO,
    }
}

#[cfg(test)]
pub(in crate::source_backed) fn build_automatic_source_backed_registry_from_parts(
    discovery: &DiscoveryContext,
    data_root: &Path,
    sources: Vec<ProviderSource>,
    discovery_issues: Vec<DiscoveryIssue>,
) -> SourceBackedAutomaticRegistryBuild {
    build_automatic_source_backed_registry_from_parts_with_probes(
        &crate::test_provider_probes(),
        discovery,
        data_root,
        sources,
        discovery_issues,
        &BTreeMap::new(),
    )
}

fn retain_unsupported_automatic_format(
    registry: &mut SourceBackedProviderRegistry,
    issues: &mut Vec<SourceBackedAutomaticRegistryIssue>,
    source: ProviderSource,
    detail: &'static str,
) {
    registry.register(SourceBackedRoute::unsupported(source.clone(), detail));
    issues.push(SourceBackedAutomaticRegistryIssue::Unavailable {
        source,
        reason: SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail },
    });
}

fn automatic_unavailable_detail(reason: &SourceBackedAutomaticUnavailableReason) -> String {
    match reason {
        SourceBackedAutomaticUnavailableReason::SourceStatus(status) => {
            format!("provider source status is {}", status.as_str())
        }
        SourceBackedAutomaticUnavailableReason::UnsafeRootOverlap { detail }
        | SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail, .. } => {
            detail.clone()
        }
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail } => {
            (*detail).to_owned()
        }
    }
}

fn register_discovered_automatic_route(
    registry: &mut SourceBackedProviderRegistry,
    probes: &StaticProviderProbeCatalog,
    discovery: &DiscoveryContext,
    data_root: &Path,
    format_route: &'static SourceBackedProviderRouteMetadata,
    source: ProviderSource,
    route_authority: SqliteInventoryRouteAuthority,
) -> Result<(), SourceBackedAutomaticUnavailableReason> {
    let (source_root_lineage, inventory_coverage) = route_authority;
    let Some(source_root_lineage) = source_root_lineage else {
        return register_discovered_automatic_route_scoped(
            registry,
            probes,
            discovery,
            data_root,
            format_route,
            source,
            (None, inventory_coverage),
        );
    };
    let provider = source.provider;
    let mut scoped = SourceBackedProviderRegistry::new();
    register_discovered_automatic_route_scoped(
        &mut scoped,
        probes,
        discovery,
        data_root,
        format_route,
        source,
        (Some(source_root_lineage), inventory_coverage),
    )?;
    if scoped.routes.len() != 1 {
        return Err(
            SourceBackedAutomaticUnavailableReason::RegistrationRejected {
                kind: SourceBackedRouteErrorKind::Internal,
                detail: format!(
                    "{} automatic coexistence registration produced {} routes instead of one",
                    provider.as_str(),
                    scoped.routes.len()
                ),
            },
        );
    }
    let mut route = scoped.routes.pop().expect("one scoped route was validated");
    route
        .apply_automatic_coexistence_identity(source_root_lineage)
        .map_err(automatic_registration_rejected)?;
    registry.register(route);
    Ok(())
}

fn register_discovered_automatic_route_scoped(
    registry: &mut SourceBackedProviderRegistry,
    probes: &StaticProviderProbeCatalog,
    discovery: &DiscoveryContext,
    data_root: &Path,
    format_route: &'static SourceBackedProviderRouteMetadata,
    source: ProviderSource,
    route_authority: SqliteInventoryRouteAuthority,
) -> Result<(), SourceBackedAutomaticUnavailableReason> {
    let (source_root_lineage, inventory_coverage) = route_authority;
    let result = match (format_route.constructor, source.provider) {
        (SourceBackedRouteConstructor::NamedSurface, CaptureProvider::Warp) => {
            let selected =
                resolve_warp_discovery_authority(probes, discovery, &source).map_err(|error| {
                    SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                        detail: warp_discovery_unavailable_detail(error),
                    }
                })?;
            register_warp_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                selected.surface_key().as_str(),
                source_root_lineage,
            )
        }
        (SourceBackedRouteConstructor::SelectedWithRetainedRoutes, CaptureProvider::Goose) => {
            let platform_root = goose_platform_root(discovery, &source.path).ok_or(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: "Goose discovery selected a database without its exact platform root",
                },
            )?;
            register_goose_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                platform_root,
                Vec::new(),
                source_root_lineage,
            )
        }
        (SourceBackedRouteConstructor::FiniteInventory, CaptureProvider::Crush) => {
            let inventory_source = discovered_crush_inventory_source(probes, discovery, &source)?;
            register_crush_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                inventory_source,
                source_root_lineage,
                inventory_coverage,
            )
        }
        (SourceBackedRouteConstructor::FiniteInventory, CaptureProvider::Lingma) => {
            let selector = LingmaInventorySelector::new(discovery.clone(), *probes);
            let registration =
                ctx_history_providers_sqlite_inventory::registration::discovered_lingma_registration_scoped::<
                    crate::provider::source_backed::family::document::CaptureDocumentLifecycle,
                    crate::provider::source_backed::family::document::CaptureDocumentSpool,
                    _,
                >(
                    source,
                    SourceBackedRouteSelection::Automatic,
                    data_root,
                    move || selector.observe(),
                    source_root_lineage.map_or(
                        ctx_history_core::SourceAnchorScope::Unqualified,
                        ctx_history_core::SourceAnchorScope::Lineage,
                    ),
                    inventory_coverage,
                )
                .map_err(|error| match error {
                    ctx_history_providers_sqlite_inventory::registration::LingmaRegistrationError::SelectorAuthorityUnavailable(detail) => {
                        SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail }
                    }
                    ctx_history_providers_sqlite_inventory::registration::LingmaRegistrationError::RegistrationRejected(detail) => {
                        SourceBackedAutomaticUnavailableReason::RegistrationRejected {
                            kind: SourceBackedRouteErrorKind::Unsupported,
                            detail,
                        }
                    }
                })?;
            crate::provider::source_backed::family::document::install_sqlite_inventory_registration(
                registry,
                registration,
            )
        }
        (SourceBackedRouteConstructor::DiscoveryContext, CaptureProvider::AstrBot) => {
            register_astrbot_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                discovery.clone().with_configured_provider_roots(Vec::new()),
                source_root_lineage,
            )
        }
        (SourceBackedRouteConstructor::CatalogLineage, CaptureProvider::NanoClaw) => {
            if source_root_lineage.is_some() {
                return Err(
                    SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                        detail:
                            "NanoClaw automatic coexistence requires a scoped catalog connector",
                    },
                );
            }
            let lineage = explicit_source_catalog_lineage(
                source.provider,
                format_route.certified_source_format,
                &source.path,
            );
            register_nanoclaw_source_backed_route_with_selection(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                lineage,
                &[],
            )
        }
        (SourceBackedRouteConstructor::ExactCwd, CaptureProvider::Shelley) => {
            if source_root_lineage.is_some() {
                return Err(
                    SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                        detail:
                            "Shelley automatic coexistence requires a scoped exact-CWD connector",
                    },
                );
            }
            let exact_cwd = discovery.cwd().ok_or(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: "Shelley automatic registration requires the exact discovery CWD",
                },
            )?;
            register_shelley_source_backed_route(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                exact_cwd,
            )
        }
        (SourceBackedRouteConstructor::ProviderSource, CaptureProvider::OpenHands) => {
            let current_root = resolve_openhands_conversations_root(discovery).ok_or(
                SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                    detail: "OpenHands automatic registration requires its exact current conversation root",
                },
            )?;
            register_openhands_automatic_route(registry, source, &current_root, source_root_lineage)
        }
        (SourceBackedRouteConstructor::ProviderSource, _) => {
            register_landed_source_backed_route_with_data_root_and_lineage(
                registry,
                source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                source_root_lineage,
            )
        }
        _ => Err(invalid_route(
            source.provider,
            "the landed route constructor does not match its provider registration callback",
        )),
    };
    result.map_err(automatic_registration_rejected)
}

fn automatic_registration_rejected(
    error: SourceBackedCoordinatorError,
) -> SourceBackedAutomaticUnavailableReason {
    let kind = match &error {
        SourceBackedCoordinatorError::RouteScan { source, .. }
        | SourceBackedCoordinatorError::RouteRegistration { source, .. }
        | SourceBackedCoordinatorError::Progress(source)
        | SourceBackedCoordinatorError::CoreEmission(source) => source.kind,
        SourceBackedCoordinatorError::UnavailableRoute { .. } => {
            SourceBackedRouteErrorKind::Unavailable
        }
        SourceBackedCoordinatorError::InvalidRoute { .. }
        | SourceBackedCoordinatorError::InvalidRefreshScope { .. } => {
            SourceBackedRouteErrorKind::Unsupported
        }
        _ => SourceBackedRouteErrorKind::Internal,
    };
    SourceBackedAutomaticUnavailableReason::RegistrationRejected {
        kind,
        detail: error.to_string(),
    }
}

const fn warp_discovery_unavailable_detail(error: WarpDiscoveryUnavailable) -> &'static str {
    match error {
        WarpDiscoveryUnavailable::UnsupportedPlatform { .. } => {
            "Warp installed-surface authority is unavailable on this platform"
        }
        WarpDiscoveryUnavailable::WindowsLocalDataRootUnavailable => {
            "Warp installed-surface authority has no Windows local-data root"
        }
        WarpDiscoveryUnavailable::ProviderSpecUnavailable => {
            "Warp provider discovery specification is unavailable"
        }
        WarpDiscoveryUnavailable::SourceCandidateRejected { .. } => {
            "Warp installed-surface discovery rejected the selected source within fixed bounds"
        }
        WarpDiscoveryUnavailable::SourceNotSelected => {
            "Warp source is absent from authoritative installed-surface discovery"
        }
    }
}

#[derive(Debug, Clone)]
struct DiscoveredCrushInventorySource {
    selector: CrushProjectInventorySelector,
    spec: &'static ProviderSourceSpec,
}

impl CrushProjectInventorySourceV0 for DiscoveredCrushInventorySource {
    fn observe(
        &self,
    ) -> ctx_history_providers_sqlite_inventory::CrushSourceBackedResultV0<
        CrushProjectInventoryObservationV0,
    > {
        self.selector
            .observe(self.spec)
            .map_err(crush_selector_adapter_error)
            .and_then(crush_adapter_inventory)
    }
}

fn discovered_crush_inventory_source(
    probes: &StaticProviderProbeCatalog,
    discovery: &DiscoveryContext,
    selected_source: &ProviderSource,
) -> Result<Arc<DiscoveredCrushInventorySource>, SourceBackedAutomaticUnavailableReason> {
    let spec = provider_source_spec(CaptureProvider::Crush).ok_or(
        SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
            detail: "Crush provider discovery specification is unavailable",
        },
    )?;
    let source = Arc::new(DiscoveredCrushInventorySource {
        selector: CrushProjectInventorySelector::new(discovery.clone(), *probes),
        spec,
    });
    let opening = source.selector.observe(spec).map_err(|error| {
        SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
            detail: error.detail(),
        }
    })?;
    if !opening
        .databases()
        .iter()
        .any(|database| database.database_path() == selected_source.path)
    {
        return Err(
            SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
                detail:
                    "Crush selected database is absent from its authoritative project inventory",
            },
        );
    }
    crush_adapter_inventory(opening).map_err(|error| {
        SourceBackedAutomaticUnavailableReason::RegistrationRejected {
            kind: SourceBackedRouteErrorKind::Unsupported,
            detail: error.to_string(),
        }
    })?;
    Ok(source)
}

fn crush_adapter_inventory(
    inventory: CrushDiscoveredProjectInventory,
) -> ctx_history_providers_sqlite_inventory::CrushSourceBackedResultV0<
    CrushProjectInventoryObservationV0,
> {
    let authority_key = inventory
        .authority_key()
        .map_err(crush_selector_adapter_error)?;
    let databases = inventory
        .databases()
        .iter()
        .map(|database| {
            let project_key = database
                .selector_key()
                .typed_key()
                .map_err(crush_selector_adapter_error)?;
            CrushProjectDatabaseV0::new(project_key, database.database_path())
        })
        .collect::<ctx_history_providers_sqlite_inventory::CrushSourceBackedResultV0<Vec<_>>>()?;
    CrushProjectInventoryObservationV0::new(authority_key, inventory.revision().to_vec(), databases)
}

fn crush_selector_adapter_error(
    error: CrushProjectInventorySelectorError,
) -> ctx_history_providers_sqlite_inventory::CrushSourceBackedErrorV0 {
    ctx_history_providers_sqlite_inventory::CaptureError::InvalidPayload(error.to_string()).into()
}
