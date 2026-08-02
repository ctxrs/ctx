use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedRefreshProgress {
    pub phase: &'static str,
    pub completed_sources: usize,
    pub total_sources: usize,
    pub current_source: Option<String>,
    /// Time spent in the current phase when this event was emitted.
    pub stage_duration: Duration,
    /// Total measured discovery plus refresh time at this event.
    pub elapsed: Duration,
    /// Commit-derived source evidence, available only after publication.
    pub certified_source_count: Option<usize>,
    /// Commit-derived byte evidence, available only after publication.
    pub certified_source_bytes: Option<u64>,
}

/// Capture-owned executor that can be installed behind the daemon's
/// provider-neutral `SourceBackedRefreshExecutor` callback seam.
#[derive(Debug, Clone)]
pub struct SourceBackedRefreshExecutor {
    registry: SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    discovery_duration: Duration,
    work_budget: usize,
}

impl SourceBackedRefreshExecutor {
    pub fn new(registry: SourceBackedProviderRegistry, writer_options: WriterOptions) -> Self {
        Self::with_discovery_duration(registry, writer_options, Duration::ZERO)
    }

    pub fn with_discovery_duration(
        registry: SourceBackedProviderRegistry,
        writer_options: WriterOptions,
        discovery_duration: Duration,
    ) -> Self {
        let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
        Self {
            registry,
            writer_options,
            discovery_duration,
            work_budget,
        }
    }

    pub fn registry(&self) -> &SourceBackedProviderRegistry {
        &self.registry
    }

    pub fn refresh(
        &self,
        index_root: impl AsRef<Path>,
        report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
        refresh_source_backed_generation_with_progress_and_discovery_timing(
            index_root,
            &self.registry,
            self.writer_options.clone(),
            self.discovery_duration,
            self.work_budget,
            report_progress,
        )
    }
}

#[derive(Debug)]
pub struct SourceBackedRefreshReceipt {
    pub commit: CommitReceipt,
    /// The exact retained source set committed by `commit`, copied from its
    /// immutable manifest rather than from a later [`VerifiedIndex`] reopen.
    pub sources: Vec<CertifiedSource>,
    /// Transition-local certified leaf removals applied by this refresh.
    /// Prior-generation removals are never copied forward.
    pub removals: Vec<SourceBackedCertifiedRemoval>,
    pub scanned_routes: usize,
    pub unsupported_routes: Vec<SourceBackedRouteMetadata>,
    pub discovery_duration: Duration,
    pub scan_stage_duration: Duration,
    pub commit_duration: Duration,
    pub certified_source_count: usize,
    pub certified_source_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedCertifiedRemoval {
    pub deletion: CertifiedSourceDeletion,
    pub inventory: CertifiedSourceInventory,
}

/// Runs every executable route against one writer and publishes one atomic
/// generation. This is the capture-owned executor seam for the daemon.
pub fn refresh_source_backed_generation(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    refresh_source_backed_generation_with_progress(index_root, registry, writer_options, |_| Ok(()))
}

pub fn refresh_source_backed_generation_with_progress(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let work_budget = source_backed_refresh_work_budget(writer_options.indexer_threads);
    refresh_source_backed_generation_with_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        Duration::ZERO,
        work_budget,
        report_progress,
    )
}

fn refresh_source_backed_generation_with_progress_and_discovery_timing(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    discovery_duration: Duration,
    work_budget: usize,
    mut report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    if let Some(unavailable) = registry.routes.iter().find(|route| {
        route.driver.is_none()
            && route.certified_missing_paths.is_empty()
            && route.metadata.source.status == ProviderSourceStatus::Unknown
    }) {
        return Err(SourceBackedCoordinatorError::UnavailableRoute {
            provider: unavailable.metadata.source.provider,
            detail: unavailable
                .metadata
                .unsupported_reason
                .clone()
                .unwrap_or_else(|| "route state is unavailable".to_owned()),
        });
    }
    let scanned_routes = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_some())
        .count();
    let refresh_started = Instant::now();
    report_progress(SourceBackedRefreshProgress {
        phase: "discovering",
        completed_sources: 0,
        total_sources: scanned_routes,
        current_source: None,
        stage_duration: discovery_duration,
        elapsed: discovery_duration,
        certified_source_count: None,
        certified_source_bytes: None,
    })
    .map_err(SourceBackedCoordinatorError::Progress)?;
    let unsupported_routes = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_none())
        .map(|route| route.metadata.clone())
        .collect();

    let scan_started = Instant::now();
    let mut writer = GenerationWriter::open(index_root.as_ref(), writer_options)?;
    let automatic_missing_observed_at_unix_ms = source_missing_observation_time();
    let mut owners = HashMap::new();
    let mut complete_inventory_owners = Vec::new();
    let mut applied_removals = Vec::new();
    let mut completed_routes = 0;
    for (route_index, route) in registry.routes.iter().enumerate() {
        let Some(driver) = &route.driver else {
            continue;
        };
        report_progress(SourceBackedRefreshProgress {
            phase: "refreshing",
            completed_sources: completed_routes,
            total_sources: scanned_routes,
            current_source: Some(route.metadata.source.path.display().to_string()),
            stage_duration: scan_started.elapsed(),
            elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
            certified_source_count: None,
            certified_source_bytes: None,
        })
        .map_err(SourceBackedCoordinatorError::Progress)?;
        let mut sink = SourceBackedGenerationSink {
            writer: &mut writer,
            owners: &mut owners,
            complete_inventories: &mut complete_inventory_owners,
            applied_removals: &mut applied_removals,
            route_index,
            leaf_worker_budget: work_budget,
        };
        (driver.scan)(&mut sink).map_err(|source| SourceBackedCoordinatorError::RouteScan {
            provider: route.metadata.source.provider,
            source,
        })?;
        completed_routes += 1;
    }

    let mut present_routes = Vec::new();
    for (route_index, route) in registry.routes.iter().enumerate() {
        if route.driver.is_none() {
            continue;
        }
        let route_identity =
            route
                .metadata
                .route_identity
                .clone()
                .ok_or(IndexError::WriterInvariant(
                    "executable source route has no route identity",
                ))?;
        let members = owners
            .values()
            .filter(|owner| owner.route_index == route_index && owner.present)
            .map(|owner| owner.source.clone())
            .collect();
        present_routes.push(SourceRouteSnapshot::present(route_identity, members)?);
    }
    writer.set_present_source_routes(present_routes)?;

    for route in registry
        .routes
        .iter()
        .filter(|route| !route.certified_missing_paths.is_empty())
    {
        let route_identity =
            route
                .metadata
                .route_identity
                .clone()
                .ok_or(IndexError::WriterInvariant(
                    "certified-missing source route has no route identity",
                ))?;
        let paths = route.certified_missing_paths.clone();
        writer.observe_certified_missing_route(
            route_identity,
            automatic_missing_observed_at_unix_ms,
            AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS,
            move || {
                paths
                    .iter()
                    .all(|path| path_presence(path) == PathPresence::Missing)
            },
        )?;
    }

    report_progress(SourceBackedRefreshProgress {
        phase: "verifying",
        completed_sources: completed_routes,
        total_sources: scanned_routes,
        current_source: None,
        stage_duration: scan_started.elapsed(),
        elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
        certified_source_count: None,
        certified_source_bytes: None,
    })
    .map_err(SourceBackedCoordinatorError::Progress)?;
    require_complete_base_source_ownership(&writer, registry, &owners, &complete_inventory_owners)?;
    let scan_stage_duration = scan_started.elapsed();
    let commit_started = Instant::now();
    let commit = writer.commit_with_complete_inventory_revalidation(
        |target| {
            let source = match target {
                RevalidationTarget::Source(source) => source.observation().source(),
                RevalidationTarget::Deletion(deletion) => deletion.source(),
            };
            let Some(owner) = owners.get(&source.identity().digest()) else {
                return false;
            };
            if !owner.source.exact_descriptor_eq(source) {
                return false;
            }
            let Some(driver) = registry.routes[owner.route_index].driver.as_ref() else {
                return false;
            };
            if !(driver.owns_source)(source) {
                return false;
            }
            match target {
                RevalidationTarget::Source(source) => {
                    (driver.revalidate)(SourceBackedRevalidationTarget::Source(source))
                }
                RevalidationTarget::Deletion(deletion) => {
                    (driver.revalidate)(SourceBackedRevalidationTarget::Deletion(deletion))
                }
            }
        },
        |inventory| {
            let Some(owner) = complete_inventory_owners
                .iter()
                .find(|owner| owner.inventory == *inventory)
            else {
                return false;
            };
            registry.routes[owner.route_index]
                .driver
                .as_ref()
                .and_then(|driver| driver.revalidate_complete_inventory.as_ref())
                .is_some_and(|revalidate| revalidate(inventory))
        },
    )?;
    for route in &registry.routes {
        if let Some(after_publication) = route
            .driver
            .as_ref()
            .and_then(|driver| driver.after_successful_publication.as_ref())
        {
            after_publication();
        }
    }
    let commit_duration = commit_started.elapsed();
    let _ = report_progress(SourceBackedRefreshProgress {
        phase: "committed",
        completed_sources: completed_routes,
        total_sources: scanned_routes,
        current_source: None,
        stage_duration: commit_duration,
        elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
        certified_source_count: Some(commit.certified_sources),
        certified_source_bytes: Some(commit.certified_source_bytes),
    });
    let certified_source_count = commit.certified_sources;
    let certified_source_bytes = commit.certified_source_bytes;
    let sources = commit.manifest().sources.clone();
    Ok(SourceBackedRefreshReceipt {
        commit,
        sources,
        removals: applied_removals,
        scanned_routes,
        unsupported_routes,
        discovery_duration,
        scan_stage_duration,
        commit_duration,
        certified_source_count,
        certified_source_bytes,
    })
}

fn require_complete_base_source_ownership(
    writer: &GenerationWriter,
    registry: &SourceBackedProviderRegistry,
    owners: &HashMap<[u8; 32], SourceOwner>,
    complete_inventory_owners: &[CompleteInventoryOwner],
) -> SourceBackedCoordinatorResult<()> {
    let Some(base) = writer.base_manifest() else {
        return Ok(());
    };
    for source in base
        .sources
        .iter()
        .map(|source| source.observation().source())
    {
        let claimed = owners
            .get(&source.identity().digest())
            .is_some_and(|owner| {
                source_owner_covers_base_source(source, owner, complete_inventory_owners)
            });
        let covered_by_missing_route = base.source_routes().iter().any(|snapshot| {
            snapshot
                .sources()
                .iter()
                .any(|member| member.exact_descriptor_eq(source))
                && registry.routes.iter().any(|route| {
                    !route.certified_missing_paths.is_empty()
                        && route.metadata.route_identity.as_ref() == Some(snapshot.route_identity())
                })
        });
        if !claimed && !covered_by_missing_route {
            return Err(SourceBackedCoordinatorError::UnclaimedBaseSource {
                source_id: source.identity().to_string(),
            });
        }
    }
    Ok(())
}

fn source_owner_covers_base_source(
    base: &SourceKey,
    owner: &SourceOwner,
    complete_inventory_owners: &[CompleteInventoryOwner],
) -> bool {
    if owner.source.exact_descriptor_eq(base) {
        return true;
    }
    if !base.is_same_lineage_descriptor_replacement(&owner.source) {
        return false;
    }

    let mut matching_inventories = complete_inventory_owners.iter().filter(|candidate| {
        candidate.route_index == owner.route_index
            && candidate.inventory.observation().provider() == owner.source.provider()
            && candidate.inventory.validate_contract().is_ok()
            && candidate.inventory.contains(&owner.source)
    });
    matching_inventories.next().is_some() && matching_inventories.next().is_none()
}

fn source_missing_observation_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod ownership_tests {
    use ctx_history_core::{
        ProjectionContractError, SourceAnchor, SourceInventoryObservation, TypedKey,
    };

    use super::*;

    fn descriptor(schema_variant: &str, lineage: u8) -> SourceKey {
        SourceKey::derive(
            CaptureProvider::Gemini.as_str(),
            "ownership-test",
            schema_variant,
            1,
            SourceAnchor::CatalogLineage([lineage; 32]),
        )
        .unwrap()
    }

    fn inventory_owner(
        route_index: usize,
        authority: u8,
        sources: Vec<SourceKey>,
    ) -> CompleteInventoryOwner {
        let observation = SourceInventoryObservation::new(
            CaptureProvider::Gemini.as_str(),
            "ownership-test-root",
            TypedKey::U64(u64::from(authority)),
            "ownership-test-revision",
            vec![authority],
        )
        .unwrap();
        CompleteInventoryOwner {
            route_index,
            inventory: CertifiedSourceInventory::certify(
                observation.clone(),
                observation,
                "ownership-test-discovery",
                sources,
            )
            .unwrap(),
        }
    }

    #[test]
    fn base_ownership_accepts_exact_or_one_inventory_certified_descriptor_replacement() {
        let descriptor_a = descriptor("schema-a", 1);
        let descriptor_b = descriptor("schema-b", 1);
        let exact_owner = SourceOwner {
            route_index: 3,
            source: descriptor_a.clone(),
            present: true,
        };
        assert!(source_owner_covers_base_source(
            &descriptor_a,
            &exact_owner,
            &[]
        ));

        let replacement_owner = SourceOwner {
            route_index: 3,
            source: descriptor_b.clone(),
            present: true,
        };
        let inventory = inventory_owner(3, 1, vec![descriptor_b]);
        assert!(source_owner_covers_base_source(
            &descriptor_a,
            &replacement_owner,
            &[inventory]
        ));
    }

    #[test]
    fn descriptor_replacement_ownership_rejects_absence_wrong_route_ambiguity_and_lineage() {
        let descriptor_a = descriptor("schema-a", 1);
        let descriptor_b = descriptor("schema-b", 1);
        let replacement_owner = SourceOwner {
            route_index: 3,
            source: descriptor_b.clone(),
            present: true,
        };

        assert!(!source_owner_covers_base_source(
            &descriptor_a,
            &replacement_owner,
            &[]
        ));
        assert!(!source_owner_covers_base_source(
            &descriptor_a,
            &replacement_owner,
            &[inventory_owner(4, 1, vec![descriptor_b.clone()])]
        ));
        assert!(!source_owner_covers_base_source(
            &descriptor_a,
            &replacement_owner,
            &[
                inventory_owner(3, 1, vec![descriptor_b.clone()]),
                inventory_owner(3, 2, vec![descriptor_b]),
            ]
        ));

        let unrelated_owner = SourceOwner {
            route_index: 3,
            source: descriptor("schema-b", 2),
            present: true,
        };
        assert!(!source_owner_covers_base_source(
            &descriptor_a,
            &unrelated_owner,
            &[inventory_owner(3, 3, vec![unrelated_owner.source.clone()])]
        ));
    }

    #[test]
    fn inventory_rejects_two_descriptors_for_one_canonical_lineage() {
        let descriptor_a = descriptor("schema-a", 1);
        let descriptor_b = descriptor("schema-b", 1);
        let observation = SourceInventoryObservation::new(
            CaptureProvider::Gemini.as_str(),
            "ownership-test-root",
            TypedKey::U64(1),
            "ownership-test-revision",
            vec![1],
        )
        .unwrap();
        assert_eq!(
            CertifiedSourceInventory::certify(
                observation.clone(),
                observation,
                "ownership-test-discovery",
                vec![descriptor_a, descriptor_b],
            )
            .unwrap_err(),
            ProjectionContractError::DuplicateInventorySource
        );
    }
}
