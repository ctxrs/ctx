use super::*;

mod provider_roots;
mod registry;
use provider_roots::{
    applied_provider_roots, released_compound_inventory_coverage, released_compound_root_sources,
    restore_released_automatic_route_role, ReleasedCompoundRootSource, ReleasedProviderRootRoute,
};
#[cfg(test)]
pub(in crate::source_backed) use registry::build_automatic_source_backed_registry_from_parts;
use registry::{
    build_automatic_source_backed_registry_from_parts_with_probes, goose_platform_root,
};

/// Derives the stable catalog lineage for an exact provider source request.
///
/// This is the released explicit-source v1 identity contract. Automatic
/// routes that represent the same certified format and physical path must use
/// this lineage too, so route selection does not fork source, session, event,
/// cursor, or replay-checkpoint identity.
pub fn explicit_source_catalog_lineage(
    provider: CaptureProvider,
    certified_source_format: &str,
    path: &Path,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.explicit-source-request-lineage.v1\0");
    digest.update(provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(certified_source_format.as_bytes());
    digest.update([0]);
    digest.update(path.as_os_str().as_encoded_bytes());
    digest.finalize().into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceBackedAutomaticUnavailableReason {
    SourceStatus(ProviderSourceStatus),
    UnsafeRootOverlap {
        detail: String,
    },
    UnsupportedFormat {
        detail: &'static str,
    },
    SelectorAuthorityUnavailable {
        detail: &'static str,
    },
    RegistrationRejected {
        kind: SourceBackedRouteErrorKind,
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceBackedAutomaticRegistryIssue {
    Discovery(DiscoveryIssue),
    Unavailable {
        source: ProviderSource,
        reason: SourceBackedAutomaticUnavailableReason,
    },
}

#[derive(Debug, Clone)]
pub struct SourceBackedAutomaticRegistryBuild {
    pub registry: SourceBackedProviderRegistry,
    pub issues: Vec<SourceBackedAutomaticRegistryIssue>,
    pub discovery_duration: Duration,
}

impl SourceBackedAutomaticRegistryBuild {
    pub fn executable_route_count(&self) -> usize {
        self.registry.executable_route_count()
    }

    pub fn unsupported_route_count(&self) -> usize {
        self.registry.unsupported_route_count()
    }

    pub fn into_parts(
        self,
    ) -> (
        SourceBackedProviderRegistry,
        Vec<SourceBackedAutomaticRegistryIssue>,
    ) {
        (self.registry, self.issues)
    }

    pub fn into_refresh_executor(
        self,
        writer_options: WriterOptions,
    ) -> (
        SourceBackedRefreshExecutor,
        Vec<SourceBackedAutomaticRegistryIssue>,
    ) {
        (
            SourceBackedRefreshExecutor::with_discovery_duration(
                self.registry,
                writer_options,
                self.discovery_duration,
            ),
            self.issues,
        )
    }
}

/// Discovers and registers every automatic source-backed route capture can
/// construct without daemon-side provider branching.
///
/// Normal provider absence and selector/discovery limitations are returned as
/// typed issues. A detected format whose adapter seam is unavailable is also
/// retained as a typed unsupported route, so refresh and hydration cannot
/// silently claim it.
pub fn build_automatic_source_backed_registry_with_probes(
    probes: &StaticProviderProbeCatalog,
    discovery: &DiscoveryContext,
    data_root: &Path,
) -> SourceBackedAutomaticRegistryBuild {
    let discovery_started = Instant::now();
    let discovery = discovery.clone().with_data_root(data_root);
    let report =
        ctx_history_source_discovery::discover_provider_sources_with_context(probes, &discovery);
    let mut build = build_automatic_source_backed_registry_from_report_with_probes(
        probes, &discovery, data_root, report,
    );
    build.discovery_duration = discovery_started.elapsed();
    build
}

/// Registers automatic routes from one already-completed discovery report.
///
/// Callers that must validate source roots before their first persistent write
/// can pass the same report through registration instead of traversing every
/// provider tree a second time.
pub fn build_automatic_source_backed_registry_from_report_with_probes(
    probes: &StaticProviderProbeCatalog,
    discovery: &DiscoveryContext,
    data_root: &Path,
    report: DiscoveryReport,
) -> SourceBackedAutomaticRegistryBuild {
    build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
        probes,
        discovery,
        data_root,
        report,
        &BTreeMap::new(),
    )
}

#[doc(hidden)]
pub fn build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
    probes: &StaticProviderProbeCatalog,
    discovery: &DiscoveryContext,
    data_root: &Path,
    report: DiscoveryReport,
    retained_provider_roots: &BTreeMap<String, RetainedProviderRootAuthority>,
) -> SourceBackedAutomaticRegistryBuild {
    build_automatic_source_backed_registry_from_parts_with_probes(
        probes,
        discovery,
        data_root,
        report.sources,
        report.issues,
        retained_provider_roots,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderRootRegistration {
    source_identity: ProviderRootSourceIdentity,
    released_identity_root: Option<PathBuf>,
    retained_authority: Option<RetainedProviderRootAuthority>,
}

fn normalized_provider_root_registrations(
    discovery: &DiscoveryContext,
    configured_sources: &[ProviderSource],
    canonical_automatic_sources: &[ProviderSource],
    data_root: &Path,
    retained: &BTreeMap<String, RetainedProviderRootAuthority>,
) -> BTreeMap<String, ProviderRootRegistration> {
    // Composition may run after the discovery report crossed an I/O boundary,
    // so the canonical automatic view is deliberately revalidated before it
    // can grant a *new* Released owner. Any TOCTOU change (including a root
    // becoming unavailable) fails this gate and remains NamedV1. A previously
    // published Released owner is kept ahead of this gate; refresh retention
    // then protects its exact prior route membership while the source is
    // unreadable.
    let mut released_claims = BTreeSet::new();
    let mut identities = BTreeMap::new();
    for root in discovery.configured_provider_roots() {
        let retained_root = retained.get(&root.id);
        let shared_released = matches!(
            root.provider,
            CaptureProvider::Crush | CaptureProvider::Lingma
        );
        let route_claims = released_automatic_route_claims(
            root,
            configured_sources,
            canonical_automatic_sources,
            retained_root,
        );
        let retained_released = retained_root.is_some_and(|authority| {
            authority.source_identity() == ProviderRootSourceIdentity::Released
        });
        let released_available = shared_released
            || (retained_released && route_claims.is_empty())
            || route_claims.is_disjoint(&released_claims);
        let identity = match retained_root.map(RetainedProviderRootAuthority::source_identity) {
            Some(ProviderRootSourceIdentity::Released) if released_available => {
                ProviderRootSourceIdentity::Released
            }
            Some(_) => ProviderRootSourceIdentity::NamedV1,
            None if released_available
                && configured_root_matches_canonical_automatic_routes(
                    root,
                    configured_sources,
                    canonical_automatic_sources,
                    data_root,
                ) =>
            {
                ProviderRootSourceIdentity::Released
            }
            None => ProviderRootSourceIdentity::NamedV1,
        };
        if identity == ProviderRootSourceIdentity::Released && !shared_released {
            released_claims.extend(route_claims);
        }
        let released_identity_root = match identity {
            ProviderRootSourceIdentity::Released => retained_root
                .and_then(RetainedProviderRootAuthority::connector_binding)
                .and_then(|binding| binding.identity_root().map(Path::to_path_buf))
                .or_else(|| {
                    (retained_root.is_none()
                        && configured_root_matches_canonical_automatic_routes(
                            root,
                            configured_sources,
                            canonical_automatic_sources,
                            data_root,
                        ))
                    .then(|| root.path.clone())
                }),
            ProviderRootSourceIdentity::NamedV1 => None,
        };
        identities.insert(
            root.id.clone(),
            ProviderRootRegistration {
                source_identity: identity,
                released_identity_root,
                retained_authority: retained_root
                    .filter(|authority| authority.source_identity() == identity)
                    .cloned(),
            },
        );
    }
    identities
}

fn released_automatic_route_claims(
    root: &ProviderRootDefinition,
    configured: &[ProviderSource],
    automatic: &[ProviderSource],
    retained: Option<&RetainedProviderRootAuthority>,
) -> BTreeSet<SourceRouteIdentity> {
    let binding = retained.and_then(RetainedProviderRootAuthority::connector_binding);
    let identity_root = binding.and_then(|binding| binding.identity_root());
    let path_independent = binding.is_some() && identity_root.is_none();
    configured
        .iter()
        .filter(|source| provider_source_belongs_to_configured_root(root, source))
        .flat_map(|source| {
            let identity = identity_root
                .and_then(|identity_root| {
                    released_identity_source(root, source, identity_root).ok()
                })
                .unwrap_or_else(|| source.clone());
            let matches = automatic.iter().filter(|candidate| {
                candidate.provider == identity.provider
                    && candidate.source_format == identity.source_format
                    && (path_independent
                        || provider_paths_equivalent(&candidate.path, &identity.path))
            });
            let mut claims = matches
                .filter_map(|candidate| automatic_source_backed_route_identity(candidate).ok())
                .collect::<Vec<_>>();
            if claims.is_empty() {
                claims.extend(automatic_source_backed_route_identity(&identity));
            }
            claims
        })
        .collect()
}

fn default_provider_root_source_identity(
    _discovery: &DiscoveryContext,
    _root: &ProviderRootDefinition,
) -> ProviderRootSourceIdentity {
    ProviderRootSourceIdentity::NamedV1
}

fn configured_root_matches_canonical_automatic_routes(
    root: &ProviderRootDefinition,
    configured_sources: &[ProviderSource],
    canonical_automatic_sources: &[ProviderSource],
    data_root: &Path,
) -> bool {
    let routes = configured_sources
        .iter()
        .filter(|source| provider_source_belongs_to_configured_root(root, source))
        .collect::<Vec<_>>();
    // Compound homes commonly materialize routes lazily. A missing sibling is
    // equivalent only when the independent automatic replay reports that exact
    // route missing too; at least one readable/empty sibling must still prove
    // the live root, and Unknown/Unsupported states never grant Released.
    !routes.is_empty()
        && routes.iter().any(|source| {
            matches!(
                source.status,
                ProviderSourceStatus::Available | ProviderSourceStatus::Empty
            )
        })
        && routes.iter().all(|source| {
            validate_provider_source_roots_outside_data_root(data_root, [*source]).is_ok()
                && matched_canonical_automatic_source(source, canonical_automatic_sources)
                    .is_some_and(|automatic| {
                        matches!(
                            (source.status, automatic.status),
                            (
                                ProviderSourceStatus::Available | ProviderSourceStatus::Empty,
                                ProviderSourceStatus::Available | ProviderSourceStatus::Empty
                            ) | (ProviderSourceStatus::Missing, ProviderSourceStatus::Missing)
                        )
                    })
        })
}

fn matched_canonical_automatic_source<'a>(
    configured: &ProviderSource,
    canonical_automatic_sources: &'a [ProviderSource],
) -> Option<&'a ProviderSource> {
    canonical_automatic_sources.iter().find(|automatic| {
        automatic.provider == configured.provider
            && automatic.source_format == configured.source_format
            && provider_paths_equivalent(&automatic.path, &configured.path)
    })
}

fn register_released_provider_root_route(
    registry: &mut SourceBackedProviderRegistry,
    probes: &StaticProviderProbeCatalog,
    discovery: &DiscoveryContext,
    data_root: &Path,
    configured: (&ProviderRootDefinition, ProviderSource, &Path),
    released_compound_sources: &[ReleasedCompoundRootSource],
    provider_root_registrations: &BTreeMap<String, ProviderRootRegistration>,
) -> SourceBackedCoordinatorResult<ReleasedProviderRootRoute> {
    let (configured_root, configured_source, identity_root) = configured;
    let mut identity_source =
        released_identity_source(configured_root, &configured_source, identity_root)?;
    let mut scan_source = configured_source.clone();
    scan_source.route_provenance = identity_source.route_provenance.clone();
    let mut scoped = SourceBackedProviderRegistry::new();
    let mut exact_source_token = None;
    let inventory_coverage = released_compound_inventory_coverage(
        configured_source.provider,
        discovery,
        released_compound_sources,
        provider_root_registrations,
    );
    match configured_source.provider {
        CaptureProvider::OpenClaw => {
            register_landed_source_backed_route_with_data_root_and_lineage(
                &mut scoped,
                scan_source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                None,
            )?;
        }
        CaptureProvider::OpenHands => {
            let current_root =
                resolve_openhands_conversations_root(discovery).ok_or_else(|| {
                    invalid_route(
                        configured_source.provider,
                        "released OpenHands identity has no exact automatic current root",
                    )
                })?;
            register_openhands_automatic_route(&mut scoped, scan_source, &current_root, None)?;
        }
        CaptureProvider::Hermes => register_hermes_released_source_backed_route(
            &mut scoped,
            scan_source,
            data_root,
            &identity_source.path,
        )?,
        CaptureProvider::Warp => {
            let selected =
                resolve_warp_released_identity_authority(probes, discovery, &identity_source.path)
                    .map_err(|error| {
                        invalid_route(configured_source.provider, error.to_string())
                    })?;
            identity_source.route_provenance = selected.source().route_provenance.clone();
            scan_source.route_provenance = identity_source.route_provenance.clone();
            register_warp_source_backed_route(
                &mut scoped,
                scan_source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                selected.surface_key().as_str(),
                None,
            )?;
        }
        CaptureProvider::Goose => {
            let identity_platform_root = goose_platform_root(discovery, &identity_source.path)
                .ok_or_else(|| {
                    invalid_route(
                        configured_source.provider,
                        "released Goose identity has no exact automatic platform root",
                    )
                })?;
            let scan_platform_root = rebase_goose_platform_root(
                &identity_source.path,
                &identity_platform_root,
                &scan_source.path,
            )
            .or_else(|| scan_source.path.parent().map(Path::to_path_buf))
            .ok_or_else(|| {
                invalid_route(
                    configured_source.provider,
                    "configured Goose database has no attachment-context parent",
                )
            })?;
            register_goose_source_backed_route(
                &mut scoped,
                scan_source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                scan_platform_root,
                Vec::new(),
                None,
            )?;
        }
        CaptureProvider::Crush => {
            let roots = released_compound_sources
                .iter()
                .filter(|root| root.source.provider == CaptureProvider::Crush)
                .collect::<Vec<_>>();
            let current = roots
                .iter()
                .position(|root| root.definition.id == configured_root.id)
                .ok_or_else(|| {
                    invalid_route(
                        configured_source.provider,
                        "released Crush root is absent from consolidated inventory authority",
                    )
                })?;
            let rebindings = roots
                .iter()
                .map(|root| {
                    released_identity_source(&root.definition, &root.source, &root.identity_root)
                        .map(|identity| (identity.path, root.source.path.clone()))
                })
                .collect::<SourceBackedCoordinatorResult<Vec<_>>>()?;
            let released = resolve_crush_released_project_inventories(
                probes,
                discovery,
                &rebindings,
                discovery.automatic_provider_discovery_enabled(),
            )
            .map_err(|error| invalid_route(configured_source.provider, error.to_string()))?;
            exact_source_token = Some(source_token(
                &crush_source_key(released.released_project_keys()[current].clone()).map_err(
                    |error| invalid_route(configured_source.provider, error.to_string()),
                )?,
            ));
            let databases = released
                .databases()
                .iter()
                .map(|(key, path)| {
                    CrushProjectDatabaseV0::new(key.clone(), path.clone()).map_err(|error| {
                        invalid_route(configured_source.provider, error.to_string())
                    })
                })
                .collect::<SourceBackedCoordinatorResult<Vec<_>>>()?;
            let inventory = Arc::new(ReleasedCrushInventorySource {
                authority_key: released.authority_key().clone(),
                revision: released.revision().to_vec(),
                databases,
            });
            register_crush_source_backed_route(
                &mut scoped,
                scan_source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                inventory,
                None,
                inventory_coverage,
            )?;
        }
        CaptureProvider::Lingma => {
            let roots = released_compound_sources
                .iter()
                .filter(|root| root.source.provider == CaptureProvider::Lingma)
                .map(|root| {
                    let identity_source = released_identity_source(
                        &root.definition,
                        &root.source,
                        &root.identity_root,
                    )?;
                    let lineage = resolve_lingma_released_identity_authority(
                        probes,
                        discovery,
                        &identity_source.path,
                    )
                    .map_err(|error| invalid_route(configured_source.provider, error.to_string()))?
                    .typed_key()
                    .map_err(|error| {
                        invalid_route(configured_source.provider, error.to_string())
                    })?;
                    Ok((root, identity_source.path, lineage))
                })
                .collect::<SourceBackedCoordinatorResult<Vec<_>>>()?;
            let released_lineage = roots
                .iter()
                .find(|(root, _, _)| root.definition.id == configured_root.id)
                .map(|(_, _, lineage)| lineage.clone())
                .ok_or_else(|| {
                    invalid_route(
                        configured_source.provider,
                        "released Lingma root is absent from consolidated inventory authority",
                    )
                })?;
            let inventory = LingmaInventorySelector::new(discovery.clone(), *probes)
                .observe()
                .map_err(|error| invalid_route(configured_source.provider, error.to_string()))?;
            let authority_key = inventory
                .authority_key()
                .map_err(|error| invalid_route(configured_source.provider, error.to_string()))?;
            exact_source_token = Some(source_token(
                &lingma_source_key(released_lineage.clone()).map_err(|error| {
                    invalid_route(configured_source.provider, error.to_string())
                })?,
            ));
            let mut databases = if discovery.automatic_provider_discovery_enabled() {
                inventory
                    .databases()
                    .iter()
                    .filter(|database| {
                        !roots.iter().any(|(root, identity_path, _)| {
                            database.path() == identity_path || database.path() == root.source.path
                        })
                    })
                    .map(|database| {
                        database
                            .catalog_lineage()
                            .typed_key()
                            .map(|lineage| (database.path().to_path_buf(), lineage))
                            .map_err(|error| {
                                invalid_route(configured_source.provider, error.to_string())
                            })
                    })
                    .collect::<SourceBackedCoordinatorResult<Vec<_>>>()?
            } else {
                Vec::new()
            };
            databases.extend(
                roots
                    .iter()
                    .map(|(root, _, lineage)| (root.source.path.clone(), lineage.clone())),
            );
            register_lingma_source_backed_route(
                &mut scoped,
                scan_source,
                SourceBackedRouteSelection::Automatic,
                data_root,
                authority_key,
                databases,
                (None, inventory_coverage),
            )?;
        }
        CaptureProvider::AstrBot => register_astrbot_released_source_backed_route(
            &mut scoped,
            scan_source,
            identity_source.clone(),
            discovery.home(),
            data_root,
        )?,
        provider => {
            return Err(invalid_route(
                provider,
                "provider has no released automatic connector reconstruction",
            ));
        }
    }
    if scoped.routes.len() != 1 {
        return Err(invalid_route(
            configured_source.provider,
            format!(
                "released automatic registration produced {} routes instead of one",
                scoped.routes.len()
            ),
        ));
    }
    let mut route = scoped
        .routes
        .pop()
        .expect("one released route was validated");
    let mut configured_source = configured_source;
    if let ProviderSourceRouteProvenance::ConfiguredRoot {
        automatic_route_role,
        ..
    } = &mut configured_source.route_provenance
    {
        *automatic_route_role = identity_source
            .route_provenance
            .automatic_route_role()
            .cloned();
    }
    route.apply_released_automatic_identity(&identity_source, configured_source)?;
    let route_identity = route.metadata.route_identity.clone().ok_or_else(|| {
        invalid_route(
            identity_source.provider,
            "released automatic route has no stable route identity",
        )
    })?;
    registry.register(route);
    Ok(ReleasedProviderRootRoute {
        route_identity,
        exact_source_token,
    })
}

fn released_identity_source(
    configured_root: &ProviderRootDefinition,
    configured_source: &ProviderSource,
    identity_root: &Path,
) -> SourceBackedCoordinatorResult<ProviderSource> {
    let relative = configured_source
        .path
        .strip_prefix(&configured_root.path)
        .map_err(|_| {
            invalid_route(
                configured_source.provider,
                "configured source is outside its provider root",
            )
        })?;
    let mut identity_source = configured_source.clone();
    // Joining an empty suffix appends a separator and rotates path-sensitive identity bytes.
    identity_source.path = if relative.as_os_str().is_empty() {
        identity_root.to_path_buf()
    } else {
        identity_root.join(relative)
    };
    identity_source.route_provenance = ProviderSourceRouteProvenance::Unroled;
    if configured_source.provider == CaptureProvider::OpenClaw {
        let mut components = relative.components();
        let agents = components.next();
        let agent_id = components.next().map(|component| component.as_os_str());
        if agents.map(|component| component.as_os_str()) != Some(std::ffi::OsStr::new("agents"))
            || agent_id.is_none()
        {
            return Err(invalid_route(
                configured_source.provider,
                "released OpenClaw source has no bounded automatic agent identity",
            ));
        }
        let route_role = ProviderRouteRole::from_dynamic([
            b"agent".as_slice(),
            agent_id.expect("agent id was validated").as_encoded_bytes(),
        ])
        .map_err(|error| invalid_route(configured_source.provider, error.to_string()))?;
        identity_source.route_provenance = ProviderSourceRouteProvenance::Automatic { route_role };
    }
    Ok(identity_source)
}

fn rebase_goose_platform_root(
    identity_database: &Path,
    identity_platform_root: &Path,
    scan_database: &Path,
) -> Option<PathBuf> {
    let suffix = identity_database
        .strip_prefix(identity_platform_root)
        .ok()?;
    if suffix.as_os_str().is_empty() || !scan_database.ends_with(suffix) {
        return None;
    }
    scan_database
        .ancestors()
        .nth(suffix.components().count())
        .map(Path::to_path_buf)
}

fn codex_automatic_session_root_rank(root: &Path) -> u8 {
    match root.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("sessions") => 0,
        Some("archived_sessions") => 1,
        _ => 2,
    }
}

fn released_root_automatic_coexistence_lineage(
    registry: &SourceBackedProviderRegistry,
    discovery: &DiscoveryContext,
    provider_root_registrations: &BTreeMap<String, ProviderRootRegistration>,
    configured_sources: &[ProviderSource],
    automatic: &ProviderSource,
) -> Option<[u8; 32]> {
    if matches!(
        automatic.provider,
        CaptureProvider::Crush | CaptureProvider::Lingma
    ) {
        // These compound adapters share one route across independently keyed
        // databases. Exact source-token membership distinguishes each root;
        // route-wide coexistence lineage would instead scope unrelated
        // automatic peers to one unavailable root and hide their base members
        // from partial-inventory carry.
        return None;
    }
    let ordinary_route = automatic_source_backed_route_identity(automatic).ok()?;
    let adopted = registry.routes.iter().find(|route| {
        route.metadata.route_identity.as_ref() == Some(&ordinary_route)
            && !provider_paths_equivalent(&route.metadata.source.path, &automatic.path)
    });
    if let Some(adopted) = adopted {
        let (root_id, _) = adopted.metadata.source.route_provenance.configured_root()?;
        if provider_root_registrations
            .get(root_id)
            .map(|registration| registration.source_identity)
            == Some(ProviderRootSourceIdentity::Released)
        {
            let root = discovery
                .configured_provider_roots()
                .iter()
                .find(|root| root.id == root_id && root.provider == automatic.provider)?;
            return Some(automatic_provider_root_coexistence_source_lineage(root));
        }
    }
    // A moved Released root can be unavailable before it reconstructs an
    // executable route. Its retained connector binding still establishes the
    // same old automatic lineage, so suppress a stale Missing observation
    // without treating an Available automatic source as stale.
    let root = discovery.configured_provider_roots().iter().find(|root| {
        let Some(registration) = provider_root_registrations.get(&root.id) else {
            return false;
        };
        registration.source_identity == ProviderRootSourceIdentity::Released
            && registration.retained_authority.is_some()
            && released_automatic_route_claims(
                root,
                configured_sources,
                std::slice::from_ref(automatic),
                registration.retained_authority.as_ref(),
            )
            .contains(&ordinary_route)
    })?;
    Some(automatic_provider_root_coexistence_source_lineage(root))
}

const fn released_root_uses_automatic_registration(provider: CaptureProvider) -> bool {
    matches!(
        provider,
        CaptureProvider::OpenClaw
            | CaptureProvider::OpenHands
            | CaptureProvider::Hermes
            | CaptureProvider::Crush
            | CaptureProvider::Goose
            | CaptureProvider::AstrBot
            | CaptureProvider::Lingma
            | CaptureProvider::Warp
    )
}

fn configured_provider_root_for_source<'a>(
    discovery: &'a DiscoveryContext,
    source: &ProviderSource,
) -> Option<&'a ctx_history_capture_model::ProviderRootDefinition> {
    discovery
        .configured_provider_roots()
        .iter()
        .find(|root| provider_source_belongs_to_configured_root(root, source))
}

fn register_configured_landed_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
    route_role: &ProviderRouteRole,
) -> SourceBackedCoordinatorResult<()> {
    register_landed_source_backed_route_with_data_root_and_lineage(
        registry,
        source.clone(),
        SourceBackedRouteSelection::ExplicitManual,
        data_root,
        source_root_lineage,
    )?;
    apply_configured_route_identity(registry, &source, source_root_lineage, route_role)
}

fn apply_configured_route_identity(
    registry: &mut SourceBackedProviderRegistry,
    source: &ProviderSource,
    source_root_lineage: Option<[u8; 32]>,
    route_role: &ProviderRouteRole,
) -> SourceBackedCoordinatorResult<()> {
    let route = registry.routes.last_mut().ok_or_else(|| {
        invalid_route(
            source.provider,
            "landed configured registration produced no executable route",
        )
    })?;
    route.apply_provider_root_route_identity(source_root_lineage, route_role)
}

fn register_configured_compound_route(
    registry: &mut SourceBackedProviderRegistry,
    discovery: &DiscoveryContext,
    configured_root: &ProviderRootDefinition,
    source: ProviderSource,
    data_root: &Path,
    source_root_lineage: Option<[u8; 32]>,
    route_role: &ProviderRouteRole,
) -> SourceBackedCoordinatorResult<()> {
    // Configured roots are direct authority.  The selector keys below are
    // derived from the stable root lineage and static route role, never the
    // filesystem location, so a valid named root remains executable when its
    // installed-client automatic selector is presently unavailable.
    let configured_key = configured_compound_selector_key(source_root_lineage, route_role)?;
    match source.provider {
        CaptureProvider::Warp => {
            register_warp_source_backed_route(
                registry,
                source.clone(),
                SourceBackedRouteSelection::ExplicitManual,
                data_root,
                configured_surface_key(source_root_lineage, route_role),
                source_root_lineage,
            )?;
        }
        CaptureProvider::Goose => {
            let platform_root = source.path.parent().ok_or_else(|| {
                invalid_route(
                    source.provider,
                    "configured Goose database has no attachment-context parent",
                )
            })?;
            register_goose_source_backed_route(
                registry,
                source.clone(),
                SourceBackedRouteSelection::ExplicitManual,
                data_root,
                platform_root,
                Vec::new(),
                source_root_lineage,
            )?;
        }
        CaptureProvider::Crush => {
            let inventory = Arc::new(ConfiguredCrushInventorySource {
                database: CrushProjectDatabaseV0::new(configured_key.clone(), source.path.clone())
                    .map_err(|error| invalid_route(source.provider, error.to_string()))?,
                authority_key: configured_key.clone(),
                revision: route_role.as_bytes().to_vec(),
            });
            register_crush_source_backed_route(
                registry,
                source.clone(),
                SourceBackedRouteSelection::ExplicitManual,
                data_root,
                inventory,
                source_root_lineage,
                SqliteInventoryCoverage::Complete,
            )?;
        }
        CaptureProvider::AstrBot => {
            let root_local_discovery = discovery
                .clone()
                .with_automatic_provider_discovery(false)
                .with_configured_provider_roots(vec![configured_root.clone()]);
            register_astrbot_source_backed_route(
                registry,
                source.clone(),
                SourceBackedRouteSelection::ExplicitManual,
                data_root,
                root_local_discovery,
                source_root_lineage,
            )?;
        }
        CaptureProvider::Lingma => {
            register_lingma_source_backed_route(
                registry,
                source.clone(),
                SourceBackedRouteSelection::ExplicitManual,
                data_root,
                configured_key.clone(),
                vec![(source.path.clone(), configured_key)],
                (source_root_lineage, SqliteInventoryCoverage::Complete),
            )?;
        }
        _ => unreachable!("configured compound route is filtered by its caller"),
    }
    apply_configured_route_identity(registry, &source, source_root_lineage, route_role)
}

const CONFIGURED_COMPOUND_SELECTOR_DOMAIN: &str = "ctx.configured-root-compound-selector.v1";

fn configured_compound_selector_key(
    source_root_lineage: Option<[u8; 32]>,
    route_role: &ProviderRouteRole,
) -> SourceBackedCoordinatorResult<TypedKey> {
    let mut components = vec![
        TypedKey::utf8(CONFIGURED_COMPOUND_SELECTOR_DOMAIN)
            .map_err(|error| invalid_route(CaptureProvider::Unknown, error.to_string()))?,
        TypedKey::bytes(route_role.as_bytes().to_vec())
            .map_err(|error| invalid_route(CaptureProvider::Unknown, error.to_string()))?,
    ];
    if let Some(lineage) = source_root_lineage {
        components.push(
            TypedKey::bytes(lineage.to_vec())
                .map_err(|error| invalid_route(CaptureProvider::Unknown, error.to_string()))?,
        );
    }
    TypedKey::composite(components)
        .map_err(|error| invalid_route(CaptureProvider::Unknown, error.to_string()))
}

fn configured_surface_key(
    source_root_lineage: Option<[u8; 32]>,
    route_role: &ProviderRouteRole,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx.configured-root-warp-surface.v1\0");
    digest.update(route_role.as_bytes());
    if let Some(lineage) = source_root_lineage {
        digest.update(lineage);
    }
    format!("ctx-configured-root:{:x}", digest.finalize())
}

#[derive(Debug, Clone)]
struct ConfiguredCrushInventorySource {
    authority_key: TypedKey,
    revision: Vec<u8>,
    database: CrushProjectDatabaseV0,
}

impl CrushProjectInventorySourceV0 for ConfiguredCrushInventorySource {
    fn observe(
        &self,
    ) -> ctx_history_providers_sqlite_inventory::CrushSourceBackedResultV0<
        CrushProjectInventoryObservationV0,
    > {
        CrushProjectInventoryObservationV0::new(
            self.authority_key.clone(),
            self.revision.clone(),
            vec![self.database.clone()],
        )
    }
}

#[derive(Debug, Clone)]
struct ReleasedCrushInventorySource {
    authority_key: TypedKey,
    revision: Vec<u8>,
    databases: Vec<CrushProjectDatabaseV0>,
}

impl CrushProjectInventorySourceV0 for ReleasedCrushInventorySource {
    fn observe(
        &self,
    ) -> ctx_history_providers_sqlite_inventory::CrushSourceBackedResultV0<
        CrushProjectInventoryObservationV0,
    > {
        CrushProjectInventoryObservationV0::new(
            self.authority_key.clone(),
            self.revision.clone(),
            self.databases.clone(),
        )
    }
}
