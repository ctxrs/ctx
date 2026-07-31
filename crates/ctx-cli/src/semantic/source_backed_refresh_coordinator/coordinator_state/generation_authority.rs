use super::*;

pub(super) struct RetainedGenerationResolver {
    pub(super) resolver: Arc<GenerationBoundSourceBackedResolver>,
    pub(super) retired_at: Option<StdInstant>,
}

/// One leaseable resolver whose identity is inseparable from the verified
/// lexical generation that installed it.
#[allow(dead_code)] // Query IPC consumes this seam in the batch-hydration lane.
pub(crate) struct GenerationBoundSourceBackedResolver {
    pub(super) generation_id: String,
    pub(super) published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(super) resolver: Arc<SourceBackedResolverRegistry>,
    pub(super) verified_index: Mutex<Option<Arc<VerifiedIndex>>>,
}

impl fmt::Debug for GenerationBoundSourceBackedResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verified_index_bound = self
            .verified_index
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some();
        formatter
            .debug_struct("GenerationBoundSourceBackedResolver")
            .field("generation_id", &self.generation_id)
            .field(
                "published_explicit_source_catalog",
                &self.published_explicit_source_catalog,
            )
            .field("verified_index_bound", &verified_index_bound)
            .finish_non_exhaustive()
    }
}

#[allow(dead_code)] // Query IPC consumes this seam in the batch-hydration lane.
impl GenerationBoundSourceBackedResolver {
    pub(crate) fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub(crate) fn resolver(&self) -> &SourceBackedResolverRegistry {
        self.resolver.as_ref()
    }

    pub(crate) fn published_explicit_source_catalog(
        &self,
    ) -> Option<&ExplicitSourceCatalogAuthority> {
        self.published_explicit_source_catalog.as_ref()
    }

    pub(crate) fn verified_index(&self) -> Option<Arc<VerifiedIndex>> {
        self.verified_index
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(Arc::clone)
    }
}

#[derive(Debug, Clone, Eq, Error, PartialEq)]
#[allow(dead_code)] // Query IPC exposes this typed failure in the hydration lane.
pub(crate) enum SourceBackedResolverAccessError {
    #[error("daemon has no resolver retained for source-backed generation {requested_generation}")]
    Missing { requested_generation: String },
    #[error(
        "daemon resolver generation mismatch: requested {requested_generation}, retained {retained_generation}"
    )]
    GenerationMismatch {
        requested_generation: String,
        retained_generation: String,
    },
}

impl SourceBackedRefreshCoordinatorState {
    pub(super) fn install_resolver(&mut self, resolver: Arc<GenerationBoundSourceBackedResolver>) {
        let generation_id = resolver.generation_id.clone();
        let now = StdInstant::now();
        if let Some(previous) = self.current_published_generation.as_deref() {
            if previous != generation_id {
                if let Some(retained) = self.published_resolvers.get_mut(previous) {
                    retained
                        .resolver
                        .verified_index
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take();
                    retained.retired_at.get_or_insert(now);
                }
            }
        }
        self.published_resolvers.insert(
            generation_id.clone(),
            RetainedGenerationResolver {
                resolver,
                retired_at: None,
            },
        );
        self.current_published_generation = Some(generation_id);
        self.prune_retired_resolvers(now, SOURCE_RESOLVER_RETIREMENT_GRACE);
    }

    pub(super) fn prune_retired_resolvers(&mut self, now: StdInstant, grace: StdDuration) {
        self.published_resolvers.retain(|_, retained| {
            retained.retired_at.is_none_or(|retired_at| {
                now.saturating_duration_since(retired_at) < grace
                    || Arc::strong_count(&retained.resolver) > 1
            })
        });
    }
}

impl SourceBackedRefreshCoordinator {
    pub(in crate::semantic) fn retained_published_generation(
        &self,
    ) -> Option<Arc<GenerationBoundSourceBackedResolver>> {
        let state = self.lock_state();
        state
            .current_published_generation
            .as_deref()
            .and_then(|generation_id| state.published_resolvers.get(generation_id))
            .map(|retained| Arc::clone(&retained.resolver))
    }

    pub(super) fn bind_verified_index(
        &self,
        data_root: &Path,
        generation_id: &str,
        verified_index: Arc<VerifiedIndex>,
    ) -> Result<()> {
        if verified_index.generation_id() != generation_id {
            bail!(
                "cannot bind verified generation {} to source authority {generation_id}",
                verified_index.generation_id()
            );
        }
        retain_daemon_cycle_verified_index(&source_backed_index_root(data_root), &verified_index);
        let state = self.lock_state();
        let Some(retained) = state.published_resolvers.get(generation_id) else {
            if cfg!(test) {
                return Ok(());
            }
            bail!("verified source generation {generation_id} has no retained resolver authority");
        };
        let mut verified_slot = retained
            .resolver
            .verified_index
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = verified_slot.as_ref() {
            if existing.generation_id() == generation_id {
                return Ok(());
            }
            bail!(
                "retained source authority for {generation_id} carries verified generation {}",
                existing.generation_id()
            );
        }
        *verified_slot = Some(verified_index);
        Ok(())
    }

    /// Returns the resolver only when it is bound to the caller's exact
    /// lexical generation. Missing or stale daemon state queues the same
    /// provider-wide refresh path and remains a typed failure.
    #[allow(dead_code)] // Query IPC consumes this seam in the batch-hydration lane.
    pub(crate) fn resolver_for_generation(
        &self,
        data_root: &Path,
        generation_id: &str,
    ) -> std::result::Result<
        Arc<GenerationBoundSourceBackedResolver>,
        SourceBackedResolverAccessError,
    > {
        let result = {
            let mut state = self.lock_state();
            state.prune_retired_resolvers(StdInstant::now(), SOURCE_RESOLVER_RETIREMENT_GRACE);
            if let Some(retained) = state.published_resolvers.get(generation_id) {
                return Ok(Arc::clone(&retained.resolver));
            }
            match state.current_published_generation.as_ref() {
                Some(retained_generation) => SourceBackedResolverAccessError::GenerationMismatch {
                    requested_generation: generation_id.to_owned(),
                    retained_generation: retained_generation.clone(),
                },
                None => SourceBackedResolverAccessError::Missing {
                    requested_generation: generation_id.to_owned(),
                },
            }
        };
        self.enqueue_with_metadata(
            Some(generation_id.to_owned()),
            source_refresh_runtime_metadata(data_root),
        );
        Err(result)
    }

    #[cfg(test)]
    pub(in crate::semantic::source_backed_refresh_coordinator) fn prune_retired_resolvers_for_test(
        &self,
    ) {
        self.lock_state()
            .prune_retired_resolvers(StdInstant::now(), StdDuration::ZERO);
    }

    #[cfg(test)]
    pub(in crate::semantic::source_backed_refresh_coordinator) fn has_retained_resolver_for_test(
        &self,
        generation_id: &str,
    ) -> bool {
        self.lock_state()
            .published_resolvers
            .contains_key(generation_id)
    }
}
