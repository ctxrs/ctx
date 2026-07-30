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
        Self {
            registry,
            writer_options,
            discovery_duration,
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
    refresh_source_backed_generation_with_progress_and_discovery_timing(
        index_root,
        registry,
        writer_options,
        Duration::ZERO,
        report_progress,
    )
}

fn refresh_source_backed_generation_with_progress_and_discovery_timing(
    index_root: impl AsRef<Path>,
    registry: &SourceBackedProviderRegistry,
    writer_options: WriterOptions,
    discovery_duration: Duration,
    mut report_progress: impl FnMut(SourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
) -> SourceBackedCoordinatorResult<SourceBackedRefreshReceipt> {
    let scanned_routes = registry
        .routes
        .iter()
        .filter(|route| route.driver.is_some())
        .count();
    if scanned_routes == 0 {
        return Err(SourceBackedCoordinatorError::NoExecutableRoutes);
    }
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
    let mut owners = HashMap::new();
    let mut complete_inventory_owners = Vec::new();
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
            route_index,
        };
        (driver.scan)(&mut sink).map_err(|source| SourceBackedCoordinatorError::RouteScan {
            provider: route.metadata.source.provider,
            source,
        })?;
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
    recertify_retained_deletions(
        &mut writer,
        registry,
        &mut owners,
        &complete_inventory_owners,
    )?;
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
    })
}

fn recertify_retained_deletions(
    writer: &mut GenerationWriter,
    registry: &SourceBackedProviderRegistry,
    owners: &mut HashMap<[u8; 32], SourceOwner>,
    complete_inventory_owners: &[CompleteInventoryOwner],
) -> SourceBackedCoordinatorResult<()> {
    let retained = writer
        .base_manifest()
        .map(|manifest| manifest.removals.clone())
        .unwrap_or_default();
    for removal in retained {
        let source = removal.source();
        let prior_authority = removal.deletion().inventory();
        let current = complete_inventory_owners
            .iter()
            .find(|owner| {
                let current = owner.inventory.observation();
                current.provider() == prior_authority.provider()
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
        let driver = registry.routes[current.route_index]
            .driver
            .as_ref()
            .ok_or_else(|| {
                retained_deletion_error(
                    source,
                    "the current complete inventory has no executable route",
                )
            })?;
        if !(driver.owns_source)(source) {
            return Err(retained_deletion_error(
                source,
                "the current inventory route does not own the deleted source",
            ));
        }

        let digest = source.identity().digest();
        if current.inventory.contains(source) {
            let staged = owners.get(&digest).is_some_and(|owner| {
                owner.route_index == current.route_index && owner.source.exact_descriptor_eq(source)
            });
            if !staged {
                return Err(retained_deletion_error(
                    source,
                    "the current inventory rediscovered the source without staging it",
                ));
            }
            continue;
        }

        claim_retained_deletion(owners, current.route_index, source)?;
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
