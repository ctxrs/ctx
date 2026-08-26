use super::*;

#[cfg(test)]
thread_local! {
    static ROUTE_RETIREMENT_MEMBERSHIP_LOOKUPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static ROUTE_RETIREMENT_MEMBER_COMPARISONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_route_retirement_membership_probes() {
    ROUTE_RETIREMENT_MEMBERSHIP_LOOKUPS.with(|lookups| lookups.set(0));
    ROUTE_RETIREMENT_MEMBER_COMPARISONS.with(|comparisons| comparisons.set(0));
}

#[cfg(test)]
pub(crate) fn route_retirement_membership_probes() -> (u64, u64) {
    (
        ROUTE_RETIREMENT_MEMBERSHIP_LOOKUPS.with(std::cell::Cell::get),
        ROUTE_RETIREMENT_MEMBER_COMPARISONS.with(std::cell::Cell::get),
    )
}

impl GenerationWriter {
    /// Installs the provider-root aliases and route membership applied by this
    /// refresh. This snapshot is committed atomically with the route/source
    /// manifest so readers never resolve selectors against live config.
    pub fn set_applied_provider_roots(
        &mut self,
        automatic_provider_discovery: bool,
        config_digest: String,
        roots: Vec<AppliedProviderRoot>,
    ) -> Result<()> {
        if self.writer.is_some()
            || self.active_source_route_stage.is_some()
            || !self.pending.is_empty()
            || !self.deletions.is_empty()
            || !self.complete_inventories.is_empty()
            || self.applied_provider_roots.is_some()
        {
            return Err(IndexError::InvalidSourceRoutePlan(
                "applied provider roots must be installed exactly once before staging".to_owned(),
            ));
        }
        self.applied_provider_roots = Some((automatic_provider_discovery, config_digest, roots));
        Ok(())
    }

    /// Replaces the provisional provider-root selector snapshot after every
    /// selected route has reached a terminal outcome. This is limited to the
    /// no-active-stage boundary so a route can never observe selector aliases
    /// changing beneath its savepoint.
    pub fn finalize_applied_provider_roots(
        &mut self,
        automatic_provider_discovery: bool,
        config_digest: String,
        roots: Vec<AppliedProviderRoot>,
    ) -> Result<()> {
        self.validate_source_route_plan_complete()?;
        if self.active_source_route_stage.is_some()
            || self.active_source_route_cohort_stage.is_some()
            || self.applied_provider_roots.is_none()
        {
            return Err(IndexError::InvalidSourceRoutePlan(
                "provider-root finalization requires installed roots outside a route stage"
                    .to_owned(),
            ));
        }
        self.applied_provider_roots = Some((automatic_provider_discovery, config_digest, roots));
        Ok(())
    }

    /// Authorizes exact locked-base routes to disappear as part of a
    /// provider-root topology transition.
    ///
    /// The caller derives these identities from the admitted provider-root
    /// definitions and the locked generation manifest. The route plan still
    /// validates that every identity belongs to the base and is neither
    /// selected nor carried, so this cannot become a general route-deletion
    /// escape hatch.
    pub fn set_authorized_topology_route_retirements(
        &mut self,
        routes: BTreeSet<SourceRouteIdentity>,
    ) -> Result<()> {
        if self.writer.is_some()
            || self.source_route_plan.is_some()
            || self.active_source_route_stage.is_some()
            || !self.pending.is_empty()
            || !self.deletions.is_empty()
            || !self.complete_inventories.is_empty()
            || self.authorized_topology_route_retirements.is_some()
        {
            return Err(IndexError::InvalidSourceRoutePlan(
                "topology route retirements must be installed exactly once before the route plan"
                    .to_owned(),
            ));
        }
        self.authorized_topology_route_retirements = Some(routes);
        Ok(())
    }

    /// Defines every route conclusively present in the candidate snapshot.
    /// Missing routes are added separately by `observe_certified_missing_route`.
    pub fn set_present_source_routes(&mut self, routes: Vec<SourceRouteSnapshot>) -> Result<()> {
        if routes.iter().any(|route| route.missing_state().is_some()) {
            return Err(IndexError::WriterInvariant(
                "present source routes cannot carry missing state",
            ));
        }
        let mut canonical = routes;
        if let Some(plan) = &self.source_route_plan {
            if let Some(route) = canonical.iter().find(|route| {
                !plan.completed.contains(route.route_identity())
                    || plan.carried_from_base.contains(route.route_identity())
            }) {
                return Err(IndexError::InvalidSourceRoutePlan(format!(
                    "present route {} is not a completed selected route",
                    route.route_identity().as_str()
                )));
            }
            if let Some(base) = self
                .base_publication
                .as_ref()
                .map(PinnedPublication::manifest)
            {
                canonical.extend(
                    base.source_routes()
                        .iter()
                        .filter(|route| {
                            plan.carried_from_base.contains(route.route_identity())
                                || self
                                    .partial_source_route_deltas
                                    .contains_key(route.route_identity())
                        })
                        .cloned(),
                );
            }
        }
        canonical.sort_by(|left, right| left.route_identity().cmp(right.route_identity()));
        if canonical
            .windows(2)
            .any(|pair| pair[0].route_identity() == pair[1].route_identity())
        {
            return Err(IndexError::NonCanonicalSourceRoutes);
        }
        self.present_source_routes = Some(canonical);
        Ok(())
    }

    /// Registers one route-level witness that must still hold at Core's final
    /// publication fence, including exact-generation reuse.
    pub fn register_source_route_publication_revalidation<F>(
        &mut self,
        route_identity: SourceRouteIdentity,
        revalidate: F,
    ) -> Result<()>
    where
        F: Fn() -> bool + Send + 'static,
    {
        if self.source_route_plan.is_some() {
            self.require_active_source_route(&route_identity)?;
        }
        if self
            .route_publication_revalidations
            .iter()
            .any(|(candidate, _)| candidate == &route_identity)
        {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "route {} has duplicate publication revalidation",
                route_identity.as_str()
            )));
        }
        self.route_publication_revalidations
            .push((route_identity, Box::new(revalidate)));
        Ok(())
    }

    /// Binds this writer to one exact route selection against its locked base.
    ///
    /// `carried_from_base` routes are authenticated by the immutable base
    /// manifest and are copied without reconstructing their snapshots,
    /// certificates, aggregates, documents, or missing-grace state.
    pub fn set_source_route_plan(
        &mut self,
        selected: BTreeSet<SourceRouteIdentity>,
        carried_from_base: BTreeSet<SourceRouteIdentity>,
    ) -> Result<()> {
        if self.writer.is_some()
            || self.source_route_plan.is_some()
            || self.active_source_route_stage.is_some()
            || !self.pending.is_empty()
            || !self.deletions.is_empty()
            || !self.complete_inventories.is_empty()
        {
            return Err(IndexError::InvalidSourceRoutePlan(
                "route plan must be installed before staging".to_owned(),
            ));
        }
        if let Some(route) = selected.intersection(&carried_from_base).next() {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "route {} is both selected and carried",
                route.as_str()
            )));
        }
        let base_routes = self
            .base_publication
            .as_ref()
            .map(PinnedPublication::manifest)
            .map(|manifest| {
                manifest
                    .source_routes()
                    .iter()
                    .map(|route| route.route_identity().clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        if let Some(route) = carried_from_base.difference(&base_routes).next() {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "carried route {} is absent from the locked base",
                route.as_str()
            )));
        }
        let covered_base_routes = selected
            .union(&carried_from_base)
            .cloned()
            .collect::<BTreeSet<_>>();
        let uncovered_base_routes = base_routes
            .difference(&covered_base_routes)
            .cloned()
            .collect::<BTreeSet<_>>();
        let inferred_provider_root_retirements = self
            .base_publication
            .as_ref()
            .zip(self.applied_provider_roots.as_ref())
            .map(|(base, (_, _, applied_roots))| {
                let previous = base
                    .manifest()
                    .provider_roots()
                    .iter()
                    .flat_map(|root| root.routes().iter().cloned())
                    .collect::<BTreeSet<_>>();
                let current = applied_roots
                    .iter()
                    .flat_map(|root| root.routes().iter().cloned())
                    .collect::<BTreeSet<_>>();
                previous
                    .difference(&current)
                    .cloned()
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let explicit_topology_retirements = self
            .authorized_topology_route_retirements
            .as_ref()
            .cloned()
            .unwrap_or_default();
        if let Some(route) = explicit_topology_retirements
            .difference(&base_routes)
            .next()
        {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "topology retirement route {} is absent from the locked base",
                route.as_str()
            )));
        }
        if let Some(route) = explicit_topology_retirements
            .intersection(&covered_base_routes)
            .next()
        {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "topology retirement route {} is also selected or carried",
                route.as_str()
            )));
        }
        let authorized_provider_root_retirements = inferred_provider_root_retirements
            .union(&explicit_topology_retirements)
            .cloned()
            .collect::<BTreeSet<_>>();
        if let Some(route) = uncovered_base_routes
            .difference(&authorized_provider_root_retirements)
            .next()
        {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "base route {} is neither selected nor carried",
                route.as_str()
            )));
        }
        if !uncovered_base_routes.is_empty() {
            let base = self
                .base_publication
                .as_ref()
                .ok_or_else(|| {
                    IndexError::InvalidSourceRoutePlan(
                        "provider-root retirement requires a locked base".to_owned(),
                    )
                })?
                .manifest();
            let retired_sources = uncovered_base_routes
                .iter()
                .filter_map(|route| base.source_route(route))
                .flat_map(SourceRouteSnapshot::sources)
                .cloned()
                .collect::<Vec<_>>();
            for source in retired_sources {
                // Defer the physical delete until publication. A selected
                // replacement route may re-own this exact source and retain
                // its unchanged documents without restaging them.
                self.route_deletions.insert(source);
            }
        }
        self.source_route_plan = Some(SourceRoutePlan {
            selected,
            carried_from_base,
            completed: BTreeSet::new(),
        });
        Ok(())
    }

    /// Starts a candidate-only savepoint for one selected route.
    pub fn begin_source_route_stage(&mut self, route_identity: SourceRouteIdentity) -> Result<()> {
        if let Some(active) = &self.active_source_route_stage {
            return Err(IndexError::SourceRouteStagingAlreadyActive(
                active.route_identity.as_str().to_owned(),
            ));
        }
        let plan = self.source_route_plan.as_ref().ok_or_else(|| {
            IndexError::InvalidSourceRoutePlan("route staging requires a route plan".to_owned())
        })?;
        if !plan.selected.contains(&route_identity) || plan.completed.contains(&route_identity) {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "route {} is not an incomplete selected route",
                route_identity.as_str()
            )));
        }
        let checkpoint = SourceRouteStageCheckpoint {
            route_identity: route_identity.clone(),
            source_route_plan: plan.clone(),
            complete_inventories: self.complete_inventories.clone(),
            pending: self.pending.clone(),
            deletions: self.deletions.clone(),
            route_deletions: self.route_deletions.clone(),
            observed_missing_routes: self.observed_missing_routes.clone(),
            route_publication_revalidation_len: self.route_publication_revalidations.len(),
            partially_reconciled_routes: self.partially_reconciled_routes.clone(),
            partial_source_route_deltas: self.partial_source_route_deltas.clone(),
            source_identities: self.source_identities.clone(),
            changed_session_insertions: Vec::new(),
            changed_session_updates: Vec::new(),
        };
        self.active_source_route_stage = Some(checkpoint);
        Ok(())
    }

    /// Opens an outer savepoint that makes several selected route stages one
    /// publication unit. Inner route stages still produce independent failure
    /// diagnostics, but their index writes remain uncommitted until the cohort
    /// succeeds as a whole.
    pub fn begin_source_route_cohort_stage(
        &mut self,
        cohort_identity: SourceRouteIdentity,
    ) -> Result<()> {
        if self.active_source_route_stage.is_some()
            || self.active_source_route_cohort_stage.is_some()
        {
            return Err(IndexError::InvalidSourceRoutePlan(
                "source route cohort staging is already active".to_owned(),
            ));
        }
        let plan = self.source_route_plan.as_ref().ok_or_else(|| {
            IndexError::InvalidSourceRoutePlan(
                "route cohort staging requires a route plan".to_owned(),
            )
        })?;
        if !plan.selected.contains(&cohort_identity) || plan.completed.contains(&cohort_identity) {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "route {} cannot anchor an incomplete route cohort",
                cohort_identity.as_str()
            )));
        }
        self.active_source_route_cohort_stage = Some(SourceRouteStageCheckpoint {
            route_identity: cohort_identity,
            source_route_plan: plan.clone(),
            complete_inventories: self.complete_inventories.clone(),
            pending: self.pending.clone(),
            deletions: self.deletions.clone(),
            route_deletions: self.route_deletions.clone(),
            observed_missing_routes: self.observed_missing_routes.clone(),
            route_publication_revalidation_len: self.route_publication_revalidations.len(),
            partially_reconciled_routes: self.partially_reconciled_routes.clone(),
            partial_source_route_deltas: self.partial_source_route_deltas.clone(),
            source_identities: self.source_identities.clone(),
            changed_session_insertions: Vec::new(),
            changed_session_updates: Vec::new(),
        });
        Ok(())
    }

    /// Returns the complete source/deletion certificates added by the active
    /// route since its savepoint was opened.
    ///
    /// This lets a coordinator perform one route-local terminal authority
    /// check while the route can still be rolled back. The returned targets
    /// borrow the writer and cannot outlive or mutate the active stage.
    pub fn active_source_route_revalidation_targets(&self) -> Result<Vec<RevalidationTarget<'_>>> {
        let checkpoint = self.active_source_route_stage.as_ref().ok_or_else(|| {
            IndexError::InvalidSourceRoutePlan(
                "route revalidation requires an active source route stage".to_owned(),
            )
        })?;
        let mut targets = Vec::new();
        for (token, pending) in &self.pending {
            if checkpoint.pending.contains_key(token) {
                continue;
            }
            let certificate = pending.certificate.as_ref().ok_or_else(|| {
                IndexError::SourceNotCertified(pending.source.identity().to_string())
            })?;
            targets.push(RevalidationTarget::Source(certificate));
        }
        for (source, deletion) in &self.deletions {
            if !checkpoint.deletions.contains_key(source) {
                targets.push(RevalidationTarget::Deletion(&deletion.proof));
            }
        }
        Ok(targets)
    }

    /// Makes the active route's staged operations the rollback point for the
    /// next route, without publishing the candidate generation.
    pub fn finish_source_route_stage(
        &mut self,
        route_identity: &SourceRouteIdentity,
    ) -> Result<()> {
        self.require_active_source_route(route_identity)?;
        for pending in self.pending.values() {
            if pending.certificate.is_none() {
                return Err(IndexError::SourceNotCertified(
                    pending.source.identity().to_string(),
                ));
            }
        }
        if self.active_source_route_cohort_stage.is_none() {
            if let Some(writer) = self.writer.as_mut() {
                writer.commit()?;
            }
        }
        if self.partially_reconciled_routes.contains(route_identity) {
            let checkpoint = self.active_source_route_stage.as_ref().ok_or_else(|| {
                IndexError::SourceRouteStagingNotActive(route_identity.as_str().into())
            })?;
            let mut delta = PartialSourceRouteDelta::default();
            for (token, pending) in &self.pending {
                if !checkpoint.pending.contains_key(token) {
                    let source = pending.source.clone();
                    delta.upserts.insert(source.identity().digest(), source);
                }
            }
            for source in self.deletions.keys() {
                if !checkpoint.deletions.contains_key(source) {
                    delta.deletions.insert(source.identity().digest());
                }
            }
            self.partial_source_route_deltas
                .insert(route_identity.clone(), delta);
        }
        let checkpoint = self.active_source_route_stage.take().ok_or_else(|| {
            IndexError::SourceRouteStagingNotActive(route_identity.as_str().into())
        })?;
        if let Some(cohort) = self.active_source_route_cohort_stage.as_mut() {
            cohort
                .changed_session_insertions
                .extend(checkpoint.changed_session_insertions);
            cohort
                .changed_session_updates
                .extend(checkpoint.changed_session_updates);
        }
        self.source_route_plan
            .as_mut()
            .ok_or(IndexError::WriterInvariant(
                "route staging lost its route plan",
            ))?
            .completed
            .insert(route_identity.clone());
        Ok(())
    }

    /// Commits every inner route stage in the active cohort as one writer
    /// transaction after the coordinator has observed complete success.
    pub fn finish_source_route_cohort_stage(&mut self) -> Result<()> {
        if self.active_source_route_stage.is_some()
            || self.active_source_route_cohort_stage.is_none()
        {
            return Err(IndexError::InvalidSourceRoutePlan(
                "route cohort completion requires an active outer stage and no inner stage"
                    .to_owned(),
            ));
        }
        if let Some(writer) = self.writer.as_mut() {
            writer.commit()?;
        }
        self.active_source_route_cohort_stage = None;
        Ok(())
    }

    /// Discards every inner route stage in the active cohort, restoring the
    /// exact route plan and candidate state from before its first member.
    pub fn rollback_source_route_cohort_stage(&mut self) -> Result<()> {
        if self.active_source_route_stage.is_some() {
            return Err(IndexError::InvalidSourceRoutePlan(
                "cannot roll back a route cohort while an inner stage is active".to_owned(),
            ));
        }
        let checkpoint = self
            .active_source_route_cohort_stage
            .take()
            .ok_or_else(|| {
                IndexError::InvalidSourceRoutePlan(
                    "route cohort rollback requires an active outer stage".to_owned(),
                )
            })?;
        if let Some(writer) = self.writer.as_mut() {
            writer.rollback()?;
            writer.set_merge_policy(Box::new(LexicalMergePolicy::default()));
        }
        self.complete_inventories = checkpoint.complete_inventories;
        self.source_route_plan = Some(checkpoint.source_route_plan);
        self.pending = checkpoint.pending;
        self.deletions = checkpoint.deletions;
        self.route_deletions = checkpoint.route_deletions;
        self.observed_missing_routes = checkpoint.observed_missing_routes;
        self.route_publication_revalidations
            .truncate(checkpoint.route_publication_revalidation_len);
        self.partially_reconciled_routes = checkpoint.partially_reconciled_routes;
        self.partial_source_route_deltas = checkpoint.partial_source_route_deltas;
        self.source_identities = checkpoint.source_identities;
        for (session_uuid, prior) in checkpoint.changed_session_updates.into_iter().rev() {
            self.changed_sessions.insert(session_uuid, prior);
        }
        for session_uuid in checkpoint.changed_session_insertions {
            if self.changed_sessions.remove(&session_uuid).is_none() {
                return Err(IndexError::WriterInvariant(
                    "route cohort rollback lost a changed-session registry insertion",
                ));
            }
        }
        Ok(())
    }

    /// Cancels every document and manifest mutation made since the active
    /// route began. Earlier successful route checkpoints remain intact.
    pub fn rollback_source_route_stage(
        &mut self,
        route_identity: &SourceRouteIdentity,
    ) -> Result<()> {
        self.require_active_source_route(route_identity)?;
        let checkpoint = self.active_source_route_stage.take().ok_or_else(|| {
            IndexError::SourceRouteStagingNotActive(route_identity.as_str().into())
        })?;
        if let Some(writer) = self.writer.as_mut() {
            writer.rollback()?;
            writer.set_merge_policy(Box::new(LexicalMergePolicy::default()));
        }
        self.complete_inventories = checkpoint.complete_inventories;
        self.source_route_plan = Some(checkpoint.source_route_plan);
        self.pending = checkpoint.pending;
        self.deletions = checkpoint.deletions;
        self.route_deletions = checkpoint.route_deletions;
        self.observed_missing_routes = checkpoint.observed_missing_routes;
        self.route_publication_revalidations
            .truncate(checkpoint.route_publication_revalidation_len);
        self.partially_reconciled_routes = checkpoint.partially_reconciled_routes;
        self.partial_source_route_deltas = checkpoint.partial_source_route_deltas;
        self.source_identities = checkpoint.source_identities;
        for (session_uuid, prior) in checkpoint.changed_session_updates.into_iter().rev() {
            self.changed_sessions.insert(session_uuid, prior);
        }
        for session_uuid in checkpoint.changed_session_insertions {
            if self.changed_sessions.remove(&session_uuid).is_none() {
                return Err(IndexError::WriterInvariant(
                    "route rollback lost a changed-session registry insertion",
                ));
            }
        }
        Ok(())
    }

    /// Marks the active selected route as a bounded incremental update. Its
    /// unmentioned exact members remain authenticated by the locked base route
    /// snapshot while staged members replace that base atomically.
    pub fn retain_unstaged_source_route_members(
        &mut self,
        route_identity: &SourceRouteIdentity,
    ) -> Result<()> {
        self.require_active_source_route(route_identity)?;
        self.partially_reconciled_routes
            .insert(route_identity.clone());
        Ok(())
    }

    pub fn source_route_retains_unstaged_members(
        &self,
        route_identity: &SourceRouteIdentity,
    ) -> bool {
        self.partially_reconciled_routes.contains(route_identity)
    }

    /// Authorizes the active route to take ownership from one exact carried
    /// base route while it scans.
    ///
    /// The authorization remains inside the active route savepoint and does
    /// not mutate the retained route or its documents. The caller must finish
    /// it with `retire_carried_source_route` only after successful terminal
    /// revalidation; rollback restores the original route plan.
    pub fn authorize_carried_source_route_retirement(
        &mut self,
        replacement_route: &SourceRouteIdentity,
        retired_route: &SourceRouteIdentity,
    ) -> Result<()> {
        self.validate_carried_source_route_retirement(replacement_route, retired_route)?;
        let plan = self.source_route_plan.as_mut().ok_or_else(|| {
            IndexError::InvalidSourceRoutePlan(
                "route retirement authorization requires a route plan".to_owned(),
            )
        })?;
        if !plan.carried_from_base.remove(retired_route) {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "retired route {} is already authorized for replacement",
                retired_route.as_str()
            )));
        }
        Ok(())
    }

    /// Atomically retires one exact base route after the active replacement
    /// route has scanned and terminally revalidated successfully.
    ///
    /// The retired route must already be authenticated as carried from this
    /// writer's locked base. Its sources must not be shared with another base
    /// route. The mutation remains inside the active route savepoint, so a
    /// failed replacement rolls back without changing the retained route.
    pub fn retire_carried_source_route(
        &mut self,
        replacement_route: &SourceRouteIdentity,
        retired_route: &SourceRouteIdentity,
    ) -> Result<Vec<SourceKey>> {
        let retired_sources =
            self.validate_carried_source_route_retirement(replacement_route, retired_route)?;
        let source_key_field = self.fields.source_key;
        for source in &retired_sources {
            let token = source_token(source);
            let reowned_by_active_route =
                self.active_source_route_stage
                    .as_ref()
                    .is_some_and(|checkpoint| {
                        !checkpoint.pending.contains_key(&token)
                            && self.pending.contains_key(&token)
                    });
            if reowned_by_active_route {
                continue;
            }
            if self.pending.contains_key(&token) || self.deletions.contains_key(source) {
                return Err(IndexError::InvalidSourceRoutePlan(format!(
                    "retired route {} source {} is already mutated",
                    retired_route.as_str(),
                    source.identity()
                )));
            }
            self.writer_mut()?
                .delete_term(Term::from_field_text(source_key_field, &token));
            self.route_deletions.insert(source.clone());
        }
        remove_retired_route_from_plan(self.source_route_plan.as_mut(), retired_route)?;
        Ok(retired_sources)
    }

    fn validate_carried_source_route_retirement(
        &self,
        replacement_route: &SourceRouteIdentity,
        retired_route: &SourceRouteIdentity,
    ) -> Result<Vec<SourceKey>> {
        self.require_active_source_route(replacement_route)?;
        if replacement_route == retired_route {
            return Err(IndexError::InvalidSourceRoutePlan(
                "a source route cannot retire itself".to_owned(),
            ));
        }
        let plan = self.source_route_plan.as_ref().ok_or_else(|| {
            IndexError::InvalidSourceRoutePlan("route retirement requires a route plan".to_owned())
        })?;
        let carried_at_stage_start =
            self.active_source_route_stage
                .as_ref()
                .is_some_and(|checkpoint| {
                    checkpoint
                        .source_route_plan
                        .carried_from_base
                        .contains(retired_route)
                });
        if !carried_at_stage_start {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "retired route {} was not carried when the active route began",
                retired_route.as_str()
            )));
        }
        if plan.selected.contains(retired_route) {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "retired route {} is selected in the active route plan",
                retired_route.as_str()
            )));
        }
        let base = self
            .base_publication
            .as_ref()
            .map(PinnedPublication::manifest)
            .ok_or_else(|| {
                IndexError::InvalidSourceRoutePlan(
                    "route retirement requires a locked base generation".to_owned(),
                )
            })?;
        let retired = base.source_route(retired_route).ok_or_else(|| {
            IndexError::InvalidSourceRoutePlan(format!(
                "retired route {} is absent from the locked base",
                retired_route.as_str()
            ))
        })?;
        for source in retired.sources() {
            if let Some(other) = base.source_routes().iter().find(|candidate| {
                candidate.route_identity() != retired_route
                    && route_contains_exact_source(candidate, source)
            }) {
                return Err(IndexError::InvalidSourceRoutePlan(format!(
                    "retired route {} shares source {} with route {}",
                    retired_route.as_str(),
                    source.identity(),
                    other.route_identity().as_str()
                )));
            }
        }
        Ok(retired.sources().to_vec())
    }

    /// Converts a failed selected route into exact base carry-forward. Cold
    /// failed routes have no base snapshot and are omitted.
    pub fn carry_failed_source_route_from_base(
        &mut self,
        route_identity: &SourceRouteIdentity,
    ) -> Result<bool> {
        if self.active_source_route_stage.is_some() {
            return Err(IndexError::InvalidSourceRoutePlan(
                "cannot carry a route while route staging is active".to_owned(),
            ));
        }
        let retained = self
            .base_publication
            .as_ref()
            .map(PinnedPublication::manifest)
            .is_some_and(|manifest| manifest.source_route(route_identity).is_some());
        let plan = self.source_route_plan.as_mut().ok_or_else(|| {
            IndexError::InvalidSourceRoutePlan(
                "failed-route carry requires a route plan".to_owned(),
            )
        })?;
        if !plan.selected.remove(route_identity) || plan.completed.contains(route_identity) {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "route {} is not a failed selected route",
                route_identity.as_str()
            )));
        }
        if retained {
            plan.carried_from_base.insert(route_identity.clone());
        }
        Ok(retained)
    }

    pub(super) fn require_active_source_route(
        &self,
        route_identity: &SourceRouteIdentity,
    ) -> Result<()> {
        match &self.active_source_route_stage {
            Some(active) if &active.route_identity == route_identity => Ok(()),
            _ => Err(IndexError::SourceRouteStagingNotActive(
                route_identity.as_str().to_owned(),
            )),
        }
    }

    pub(crate) fn validate_source_route_plan_complete(&self) -> Result<()> {
        if self.active_source_route_stage.is_some()
            || self.active_source_route_cohort_stage.is_some()
        {
            return Err(IndexError::InvalidSourceRoutePlan(
                "cannot publish while route staging is active".to_owned(),
            ));
        }
        if let Some(plan) = &self.source_route_plan {
            if plan.selected != plan.completed {
                let incomplete = plan
                    .selected
                    .difference(&plan.completed)
                    .next()
                    .map(SourceRouteIdentity::as_str)
                    .unwrap_or("unknown");
                return Err(IndexError::InvalidSourceRoutePlan(format!(
                    "selected route {incomplete} has no terminal successful outcome"
                )));
            }
        }
        Ok(())
    }

    pub(super) fn reject_carried_source_mutation(&self, source: &SourceKey) -> Result<()> {
        let Some(plan) = &self.source_route_plan else {
            return Ok(());
        };
        let base_owner = self
            .base_publication
            .as_ref()
            .map(PinnedPublication::manifest)
            .and_then(|base| {
                base.source_routes()
                    .iter()
                    .find(|route| route_source_with_lineage(route, source).is_some())
            });
        let owner_authorized_for_active = base_owner.is_some_and(|route| {
            self.active_source_route_stage
                .as_ref()
                .is_some_and(|checkpoint| {
                    checkpoint
                        .source_route_plan
                        .carried_from_base
                        .contains(route.route_identity())
                        && !plan.carried_from_base.contains(route.route_identity())
                })
        });
        // An uncovered base route can only exist after set_source_route_plan
        // authenticated it as an exact provider-root topology retirement.
        // Its source may therefore move directly to the active replacement
        // route without temporarily publishing both owners.
        let owner_authorized_for_topology_transfer = base_owner.is_some_and(|route| {
            !plan.selected.contains(route.route_identity())
                && !plan.carried_from_base.contains(route.route_identity())
        });
        if let Some(route) = base_owner {
            if plan.carried_from_base.contains(route.route_identity())
                && !owner_authorized_for_active
            {
                return Err(IndexError::CarriedSourceRouteMutation {
                    route_id: route.route_identity().as_str().to_owned(),
                    source_id: source.identity().to_string(),
                });
            }
        }
        let active_route = self
            .active_source_route_stage
            .as_ref()
            .ok_or_else(|| {
                IndexError::InvalidSourceRoutePlan(
                    "source mutation requires an active selected route".to_owned(),
                )
            })?
            .route_identity
            .clone();
        if let Some(route) = base_owner {
            let owner_is_active = route.route_identity() == &active_route;
            if !owner_is_active
                && !owner_authorized_for_active
                && !owner_authorized_for_topology_transfer
            {
                return Err(IndexError::SourceRouteOwnershipMutation {
                    active_route_id: active_route.as_str().to_owned(),
                    owner_route_id: route.route_identity().as_str().to_owned(),
                    source_id: source.identity().to_string(),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn source_is_carried_from_base(&self, source: &SourceKey) -> bool {
        let Some(plan) = &self.source_route_plan else {
            return false;
        };
        self.base_publication
            .as_ref()
            .map(PinnedPublication::manifest)
            .is_some_and(|base| {
                base.source_routes().iter().any(|route| {
                    plan.carried_from_base.contains(route.route_identity())
                        && route_source_with_lineage(route, source)
                            .is_some_and(|candidate| candidate.exact_descriptor_eq(source))
                })
            })
    }
}

fn route_contains_exact_source(route: &SourceRouteSnapshot, source: &SourceKey) -> bool {
    #[cfg(test)]
    ROUTE_RETIREMENT_MEMBERSHIP_LOOKUPS.with(|lookups| {
        lookups.set(lookups.get().saturating_add(1));
    });
    let digest = source_sort_key(source);
    route
        .sources()
        .binary_search_by(|candidate| {
            #[cfg(test)]
            ROUTE_RETIREMENT_MEMBER_COMPARISONS.with(|comparisons| {
                comparisons.set(comparisons.get().saturating_add(1));
            });
            source_sort_key(candidate).cmp(&digest)
        })
        .ok()
        .and_then(|index| route.sources().get(index))
        .is_some_and(|candidate| candidate.exact_descriptor_eq(source))
}

fn route_source_with_lineage<'a>(
    route: &'a SourceRouteSnapshot,
    source: &SourceKey,
) -> Option<&'a SourceKey> {
    route
        .sources()
        .binary_search_by_key(&source.identity().digest(), |candidate| {
            candidate.identity().digest()
        })
        .ok()
        .and_then(|index| route.sources().get(index))
}

fn remove_retired_route_from_plan(
    source_route_plan: Option<&mut SourceRoutePlan>,
    retired_route: &SourceRouteIdentity,
) -> Result<()> {
    source_route_plan
        .ok_or(IndexError::WriterInvariant(
            "route retirement lost its route plan",
        ))?
        .carried_from_base
        .remove(retired_route);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_retirement_plan_loss_is_a_typed_invariant() {
        let retired_route = SourceRouteIdentity::from_sha256("ab".repeat(32)).unwrap();

        let error = remove_retired_route_from_plan(None, &retired_route).unwrap_err();

        assert!(matches!(
            error,
            IndexError::WriterInvariant("route retirement lost its route plan")
        ));
    }
}
