use super::*;

impl GenerationWriter {
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
            if let Some(base) = &self.base_manifest {
                canonical.extend(
                    base.source_routes()
                        .iter()
                        .filter(|route| plan.carried_from_base.contains(route.route_identity()))
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
            .base_manifest
            .as_ref()
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
        if let Some(route) = base_routes.difference(&covered_base_routes).next() {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "base route {} is neither selected nor carried",
                route.as_str()
            )));
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
            source_identities: self.source_identities.clone(),
        };
        self.active_source_route_stage = Some(checkpoint);
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
        if let Some(writer) = self.writer.as_mut() {
            writer.commit()?;
        }
        self.active_source_route_stage = None;
        self.source_route_plan
            .as_mut()
            .ok_or(IndexError::WriterInvariant(
                "route staging lost its route plan",
            ))?
            .completed
            .insert(route_identity.clone());
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
        self.source_identities = checkpoint.source_identities;
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
        self.require_active_source_route(replacement_route)?;
        if replacement_route == retired_route {
            return Err(IndexError::InvalidSourceRoutePlan(
                "a source route cannot retire itself".to_owned(),
            ));
        }
        let plan = self.source_route_plan.as_ref().ok_or_else(|| {
            IndexError::InvalidSourceRoutePlan("route retirement requires a route plan".to_owned())
        })?;
        if !plan.carried_from_base.contains(retired_route) {
            return Err(IndexError::InvalidSourceRoutePlan(format!(
                "retired route {} is not carried from the locked base",
                retired_route.as_str()
            )));
        }
        let base = self.base_manifest.as_ref().ok_or_else(|| {
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
                    && candidate
                        .sources()
                        .iter()
                        .any(|member| member.exact_descriptor_eq(source))
            }) {
                return Err(IndexError::InvalidSourceRoutePlan(format!(
                    "retired route {} shares source {} with route {}",
                    retired_route.as_str(),
                    source.identity(),
                    other.route_identity().as_str()
                )));
            }
        }
        let retired_sources = retired.sources().to_vec();
        let source_key_field = self.fields.source_key;
        for source in &retired_sources {
            let token = source_token(source);
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
        self.source_route_plan
            .as_mut()
            .expect("route plan checked above")
            .carried_from_base
            .remove(retired_route);
        Ok(retired_sources)
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
            .base_manifest
            .as_ref()
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
        if self.active_source_route_stage.is_some() {
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
        let base_owner = self.base_manifest.as_ref().and_then(|base| {
            base.source_routes().iter().find(|route| {
                route.sources().iter().any(|candidate| {
                    candidate.exact_descriptor_eq(source)
                        || candidate.is_same_lineage_descriptor_replacement(source)
                })
            })
        });
        if let Some(route) = base_owner {
            if plan.carried_from_base.contains(route.route_identity()) {
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
            if route.route_identity() != &active_route {
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
        self.base_manifest.as_ref().is_some_and(|base| {
            base.source_routes().iter().any(|route| {
                plan.carried_from_base.contains(route.route_identity())
                    && route
                        .sources()
                        .iter()
                        .any(|candidate| candidate.exact_descriptor_eq(source))
            })
        })
    }
}
