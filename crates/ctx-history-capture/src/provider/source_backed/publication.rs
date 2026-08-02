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
    /// Certified removals applied by this commit. These are projection
    /// handoff evidence, not provider content.
    pub removals: Vec<SourceBackedCertifiedRemoval>,
    pub scanned_routes: usize,
    pub unsupported_routes: Vec<SourceBackedRouteMetadata>,
    pub discovery_duration: Duration,
    pub scan_stage_duration: Duration,
    pub commit_duration: Duration,
    pub certified_source_count: usize,
    pub certified_source_bytes: u64,
    pub outcome: SourceBackedRefreshOutcome,
    pub successful_routes: usize,
    pub source_failures: SourceBackedSourceFailures,
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
    let mut isolated_failures = HashMap::<usize, SourceBackedSourceFailure>::new();
    let mut locked_base_generation = None::<Option<String>>;
    let mut total_commit_duration = Duration::ZERO;
    let index_root = index_root.as_ref();

    let (commit, successful_route_indices) = loop {
        let mut writer = GenerationWriter::open(index_root, writer_options.clone())?;
        let current_base_generation = writer
            .base_manifest()
            .map(|manifest| manifest.generation_id())
            .transpose()?;
        match &locked_base_generation {
            Some(expected) if expected != &current_base_generation => {
                return Err(IndexError::ConcurrentGenerationChange.into());
            }
            None => locked_base_generation = Some(current_base_generation),
            Some(_) => {}
        }
        for route_index in isolated_failures.keys().copied().collect::<Vec<_>>() {
            let driver =
                registry.routes[route_index]
                    .driver
                    .as_ref()
                    .ok_or(IndexError::WriterInvariant(
                        "isolated source route lost its driver",
                    ))?;
            let carried_forward = carry_failed_route_from_base(&mut writer, driver)?;
            if carried_forward != isolated_failures[&route_index].carried_forward {
                return Err(IndexError::WriterInvariant(
                    "isolated source route changed its locked-base contribution",
                )
                .into());
            }
        }

        let automatic_missing_observed_at_unix_ms = source_missing_observation_time();
        let mut owners = HashMap::new();
        let mut complete_inventory_owners = Vec::new();
        let mut successful_this_attempt = HashSet::new();
        let mut completed_routes = isolated_failures.len();
        for (route_index, route) in registry.routes.iter().enumerate() {
            let Some(driver) = &route.driver else {
                continue;
            };
            if isolated_failures.contains_key(&route_index) {
                continue;
            }
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
            writer.begin_source_stage()?;
            let owners_checkpoint = owners.clone();
            let complete_inventory_owners_checkpoint = complete_inventory_owners.clone();
            let scan_result = {
                let mut sink = SourceBackedGenerationSink {
                    writer: &mut writer,
                    owners: &mut owners,
                    complete_inventories: &mut complete_inventory_owners,
                    route_index,
                    leaf_worker_budget: work_budget,
                    automatic_missing_observed_at_unix_ms: (route.metadata.selection
                        == Some(SourceBackedRouteSelection::Automatic))
                    .then_some(automatic_missing_observed_at_unix_ms),
                };
                (driver.scan)(&mut sink)
            };

            let source_failure_class = match scan_result {
                Ok(()) => {
                    recertify_retained_deletions_for_route(
                        &mut writer,
                        driver,
                        route_index,
                        &mut owners,
                        &complete_inventory_owners,
                    )?;
                    validate_route_ownership(
                        route,
                        driver,
                        route_index,
                        &owners,
                        &complete_inventory_owners[complete_inventory_owners_checkpoint.len()..],
                    )?;
                    // This commit is only an opaque Tantivy rollback boundary.
                    // Reading provider authority here would duplicate snapshots
                    // and still would not fence publication; the true terminal
                    // source and inventory revalidation below owns that job.
                    writer.finish_source_stage(|_| true, |_| true)?;
                    successful_this_attempt.insert(route_index);
                    None
                }
                Err(source) => match source.kind.source_failure_class() {
                    Some(class) => Some((class, source.detail.clone())),
                    None => {
                        return Err(SourceBackedCoordinatorError::RouteScan {
                            provider: route.metadata.source.provider,
                            source,
                        });
                    }
                },
            };
            if let Some((class, detail)) = source_failure_class {
                writer.rollback_source_stage()?;
                owners = owners_checkpoint;
                complete_inventory_owners = complete_inventory_owners_checkpoint;
                let carried_forward = carry_failed_route_from_base(&mut writer, driver)?;
                isolated_failures.insert(
                    route_index,
                    SourceBackedSourceFailure::from_route(route, class, carried_forward, detail),
                );
            }
            completed_routes += 1;
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
        require_complete_base_source_ownership(&writer, &owners, &complete_inventory_owners)?;
        if !isolated_failures.is_empty() && !writer.has_result_certified_sources() {
            return Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
                failures: bounded_source_failures(&isolated_failures),
            });
        }

        let base_contributions = registry
            .routes
            .iter()
            .enumerate()
            .filter_map(|(route_index, route)| {
                route.driver.as_ref().map(|driver| {
                    (
                        route_index,
                        failed_route_has_base_contribution(&writer, driver),
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        let terminal_failed_route = RefCell::new(None::<usize>);
        let commit_started = Instant::now();
        let commit_result = writer.commit_with_complete_inventory_revalidation(
            |target| {
                let source = match target {
                    RevalidationTarget::Source(source) => source.observation().source(),
                    RevalidationTarget::Deletion(deletion) => deletion.source(),
                };
                let Some(owner) = owners.get(&source.identity().digest()) else {
                    return false;
                };
                if !owner.source.exact_descriptor_eq(source)
                    || !successful_this_attempt.contains(&owner.route_index)
                {
                    return false;
                }
                let Some(driver) = registry.routes[owner.route_index].driver.as_ref() else {
                    return false;
                };
                let valid = (driver.owns_source)(source)
                    && match target {
                        RevalidationTarget::Source(source) => {
                            (driver.revalidate)(SourceBackedRevalidationTarget::Source(source))
                        }
                        RevalidationTarget::Deletion(deletion) => {
                            (driver.revalidate)(SourceBackedRevalidationTarget::Deletion(deletion))
                        }
                    };
                if !valid {
                    *terminal_failed_route.borrow_mut() = Some(owner.route_index);
                }
                valid
            },
            |inventory| {
                let Some(owner) = complete_inventory_owners
                    .iter()
                    .find(|owner| owner.inventory == *inventory)
                else {
                    return false;
                };
                if !successful_this_attempt.contains(&owner.route_index) {
                    return false;
                }
                let valid = registry.routes[owner.route_index]
                    .driver
                    .as_ref()
                    .and_then(|driver| driver.revalidate_complete_inventory.as_ref())
                    .is_some_and(|revalidate| revalidate(inventory));
                if !valid {
                    *terminal_failed_route.borrow_mut() = Some(owner.route_index);
                }
                valid
            },
        );
        total_commit_duration = total_commit_duration.saturating_add(commit_started.elapsed());
        match commit_result {
            Ok(commit) => break (commit, successful_this_attempt),
            Err(
                error @ (IndexError::SourceInvalidated(_)
                | IndexError::CompleteInventoryInvalidated { .. }),
            ) => {
                let Some(route_index) = terminal_failed_route.into_inner() else {
                    return Err(error.into());
                };
                let route = &registry.routes[route_index];
                isolated_failures.insert(
                    route_index,
                    SourceBackedSourceFailure::from_route(
                        route,
                        SourceBackedSourceFailureClass::SourceChanged,
                        base_contributions
                            .get(&route_index)
                            .copied()
                            .unwrap_or(false),
                        error.to_string(),
                    ),
                );
            }
            Err(error) => return Err(error.into()),
        }
    };
    let source_failures = bounded_source_failures(&isolated_failures);
    let scan_stage_duration = scan_started.elapsed();
    for (route_index, route) in registry.routes.iter().enumerate() {
        if !successful_route_indices.contains(&route_index) {
            continue;
        }
        if let Some(after_publication) = route
            .driver
            .as_ref()
            .and_then(|driver| driver.after_successful_publication.as_ref())
        {
            after_publication();
        }
    }
    let commit_duration = total_commit_duration;
    let _ = report_progress(SourceBackedRefreshProgress {
        phase: "committed",
        completed_sources: scanned_routes,
        total_sources: scanned_routes,
        current_source: None,
        stage_duration: commit_duration,
        elapsed: discovery_duration.saturating_add(refresh_started.elapsed()),
        certified_source_count: Some(commit.certified_sources),
        certified_source_bytes: Some(commit.certified_source_bytes),
    });
    let certified_source_count = commit.certified_sources;
    let certified_source_bytes = commit.certified_source_bytes;
    let successful_routes = successful_route_indices.len();
    let outcome = if source_failures.is_empty() {
        SourceBackedRefreshOutcome::Completed
    } else {
        SourceBackedRefreshOutcome::CompletedWithSourceFailures
    };
    let sources = commit.manifest().sources.clone();
    let removals = commit
        .manifest()
        .removals
        .iter()
        .map(|removal| SourceBackedCertifiedRemoval {
            deletion: removal.deletion().clone(),
            inventory: removal.inventory().clone(),
        })
        .collect();
    Ok(SourceBackedRefreshReceipt {
        commit,
        sources,
        removals,
        scanned_routes,
        unsupported_routes,
        discovery_duration,
        scan_stage_duration,
        commit_duration,
        certified_source_count,
        certified_source_bytes,
        outcome,
        successful_routes,
        source_failures,
    })
}

fn require_complete_base_source_ownership(
    writer: &GenerationWriter,
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
        .chain(base.removals.iter().map(GenerationRemoval::source))
    {
        let claimed = owners
            .get(&source.identity().digest())
            .is_some_and(|owner| {
                source_owner_covers_base_source(source, owner, complete_inventory_owners)
            });
        if !claimed && !writer.is_base_source_carried(source) {
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

fn bounded_source_failures(
    isolated: &HashMap<usize, SourceBackedSourceFailure>,
) -> SourceBackedSourceFailures {
    let mut route_indices = isolated.keys().copied().collect::<Vec<_>>();
    route_indices.sort_unstable();
    let mut failures = SourceBackedSourceFailures::default();
    for route_index in route_indices {
        if let Some(failure) = isolated.get(&route_index) {
            failures.record(failure.clone());
        }
    }
    failures
}

fn failed_route_base_sources(
    writer: &GenerationWriter,
    driver: &SourceBackedRouteDriver,
) -> HashSet<SourceKey> {
    writer
        .base_manifest()
        .map(|manifest| {
            manifest
                .sources
                .iter()
                .map(|source| source.observation().source())
                .chain(manifest.removals.iter().map(GenerationRemoval::source))
                .chain(
                    manifest
                        .source_catalog()
                        .missing_sources()
                        .iter()
                        .map(|missing| missing.source()),
                )
                .filter(|source| (driver.owns_source)(source))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn failed_route_has_base_contribution(
    writer: &GenerationWriter,
    driver: &SourceBackedRouteDriver,
) -> bool {
    !failed_route_base_sources(writer, driver).is_empty()
}

fn carry_failed_route_from_base(
    writer: &mut GenerationWriter,
    driver: &SourceBackedRouteDriver,
) -> SourceBackedCoordinatorResult<bool> {
    let carried = failed_route_base_sources(writer, driver);
    let carried_forward = !carried.is_empty();
    for source in carried {
        writer.carry_base_source(source)?;
    }
    Ok(carried_forward)
}

fn validate_route_ownership(
    route: &SourceBackedRoute,
    driver: &SourceBackedRouteDriver,
    route_index: usize,
    owners: &HashMap<[u8; 32], SourceOwner>,
    new_inventories: &[CompleteInventoryOwner],
) -> SourceBackedCoordinatorResult<()> {
    if owners
        .values()
        .filter(|owner| owner.route_index == route_index)
        .any(|owner| !(driver.owns_source)(&owner.source))
    {
        return Err(SourceBackedCoordinatorError::InvalidRoute {
            provider: route.metadata.source.provider,
            detail: "route staged a source outside its ownership predicate".to_owned(),
        });
    }
    if !new_inventories.is_empty() && driver.revalidate_complete_inventory.is_none() {
        return Err(SourceBackedCoordinatorError::InvalidRoute {
            provider: route.metadata.source.provider,
            detail: "route staged a complete inventory without terminal revalidation".to_owned(),
        });
    }
    Ok(())
}

fn recertify_retained_deletions_for_route(
    writer: &mut GenerationWriter,
    driver: &SourceBackedRouteDriver,
    route_index: usize,
    owners: &mut HashMap<[u8; 32], SourceOwner>,
    complete_inventory_owners: &[CompleteInventoryOwner],
) -> SourceBackedCoordinatorResult<()> {
    let retained = writer
        .base_manifest()
        .map(|manifest| manifest.removals.clone())
        .unwrap_or_default();
    for removal in retained {
        let source = removal.source();
        if !(driver.owns_source)(source) {
            continue;
        }
        let prior_authority = removal.deletion().inventory();
        let current = complete_inventory_owners
            .iter()
            .find(|owner| {
                let current = owner.inventory.observation();
                owner.route_index == route_index
                    && current.provider() == prior_authority.provider()
                    && current.authority_namespace() == prior_authority.authority_namespace()
                    && current.authority_key() == prior_authority.authority_key()
            })
            .cloned()
            .ok_or_else(|| {
                retained_deletion_error(
                    source,
                    "the current refresh supplied no complete inventory for its authority",
                )
            })?;
        let digest = source.identity().digest();
        if current.inventory.contains(source) {
            let staged = owners.get(&digest).is_some_and(|owner| {
                owner.route_index == route_index && owner.source.exact_descriptor_eq(source)
            });
            if !staged {
                return Err(retained_deletion_error(
                    source,
                    "the current inventory rediscovered the source without staging it",
                ));
            }
            continue;
        }

        claim_retained_deletion(owners, route_index, source)?;
        if !removal.deletion().verifies(&current.inventory) {
            let deletion =
                CertifiedSourceDeletion::from_inventory(source.clone(), &current.inventory)
                    .map_err(|error| {
                        retained_deletion_error(
                            source,
                            format!("the current inventory could not certify absence: {error}"),
                        )
                    })?;
            writer.delete_source(deletion, current.inventory)?;
        }
    }
    Ok(())
}

fn claim_retained_deletion(
    owners: &mut HashMap<[u8; 32], SourceOwner>,
    route_index: usize,
    source: &SourceKey,
) -> SourceBackedCoordinatorResult<()> {
    let digest = source.identity().digest();
    match owners.get(&digest) {
        Some(owner)
            if owner.route_index != route_index || !owner.source.exact_descriptor_eq(source) =>
        {
            Err(SourceBackedCoordinatorError::DuplicateSourceOwner {
                source_id: source.identity().to_string(),
            })
        }
        Some(_) => Ok(()),
        None => {
            owners.insert(
                digest,
                SourceOwner {
                    route_index,
                    source: source.clone(),
                },
            );
            Ok(())
        }
    }
}

fn retained_deletion_error(
    source: &SourceKey,
    detail: impl Into<String>,
) -> SourceBackedCoordinatorError {
    SourceBackedCoordinatorError::RetainedDeletionRecertification {
        source_id: source.identity().to_string(),
        detail: detail.into(),
    }
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
        };
        assert!(source_owner_covers_base_source(
            &descriptor_a,
            &exact_owner,
            &[]
        ));

        let replacement_owner = SourceOwner {
            route_index: 3,
            source: descriptor_b.clone(),
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
