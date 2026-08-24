use super::*;

impl GenerationWriter {
    pub fn delete_source(
        &mut self,
        proof: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
    ) -> Result<()> {
        let deletion = PendingDeletion::new(proof, inventory)?;
        let source = deletion.source();
        self.reject_carried_source_mutation(source)?;
        register_compact_identity(
            &mut self.source_identities,
            source.identity(),
            "source",
            false,
        )?;
        let token = source_token(source);
        if self.pending.contains_key(&token) {
            return Err(IndexError::DuplicateSource(source.identity().to_string()));
        }
        let source_key_field = self.fields.source_key;
        self.writer_mut()?
            .delete_term(Term::from_field_text(source_key_field, &token));
        self.route_deletions.remove(source);
        self.deletions.insert(source.clone(), deletion);
        Ok(())
    }

    /// Advances durable grace for one whole route whose absence is
    /// conclusive and can be revalidated immediately before publication.
    pub fn observe_certified_missing_route<F>(
        &mut self,
        route_identity: SourceRouteIdentity,
        observed_at_unix_ms: u64,
        delete_after_consecutive_observations: u32,
        revalidate_missing: F,
    ) -> Result<CertifiedMissingRouteOutcome>
    where
        F: Fn() -> bool + Send + 'static,
    {
        if self.source_route_plan.is_some() {
            self.require_active_source_route(&route_identity)?;
        }
        if delete_after_consecutive_observations < 2 {
            return Err(IndexError::InvalidSourceRouteDeletionGraceThreshold);
        }
        if self.observed_missing_routes.contains_key(&route_identity)
            || self
                .route_publication_revalidations
                .iter()
                .any(|(candidate, _)| candidate == &route_identity)
        {
            return Err(IndexError::DuplicateSourceRouteMissingObservation(
                route_identity.as_str().to_owned(),
            ));
        }
        self.route_publication_revalidations
            .push((route_identity.clone(), Box::new(revalidate_missing)));
        let Some(base) = self
            .base_publication
            .as_ref()
            .map(PinnedPublication::manifest)
        else {
            self.observed_missing_routes.insert(
                route_identity.clone(),
                SourceRouteSnapshot::present(route_identity, Vec::new())?,
            );
            return Ok(CertifiedMissingRouteOutcome {
                retained_sources: Vec::new(),
                deleted: false,
            });
        };
        let Some(base_route) = base.source_route(&route_identity).cloned() else {
            if self.source_route_plan.is_none() {
                // A legacy direct replay after a route's completed deletion
                // must not resurrect an empty route. Production topology
                // admission installs a selected route plan, which still
                // records newly configured-but-missing routes as empty exact
                // authority.
                return Ok(CertifiedMissingRouteOutcome {
                    retained_sources: Vec::new(),
                    deleted: false,
                });
            }
            self.observed_missing_routes.insert(
                route_identity.clone(),
                SourceRouteSnapshot::present(route_identity, Vec::new())?,
            );
            return Ok(CertifiedMissingRouteOutcome {
                retained_sources: Vec::new(),
                deleted: false,
            });
        };
        if base_route.sources().is_empty() {
            self.observed_missing_routes.insert(
                route_identity.clone(),
                SourceRouteSnapshot::present(route_identity, Vec::new())?,
            );
            return Ok(CertifiedMissingRouteOutcome {
                retained_sources: Vec::new(),
                deleted: false,
            });
        }
        let base_generation = self
            .base_publication
            .as_ref()
            .ok_or(IndexError::WriterInvariant(
                "missing-route observation is missing its base publication",
            ))?
            .generation_id()
            .to_owned();
        let observation = SourceMissingObservationPoint::new(base_generation, observed_at_unix_ms)?;
        let state = match base_route.missing_state() {
            Some(previous) => previous.advance(observation)?,
            None => SourceRouteMissingState::first(observation),
        };
        let retained_sources = base_route.sources().to_vec();
        if state.consecutive_missing().get() >= delete_after_consecutive_observations {
            let source_key_field = self.fields.source_key;
            for source in &retained_sources {
                let token = source_token(source);
                self.writer_mut()?
                    .delete_term(Term::from_field_text(source_key_field, &token));
                self.route_deletions.insert(source.clone());
            }
            return Ok(CertifiedMissingRouteOutcome {
                retained_sources,
                deleted: true,
            });
        }
        let snapshot =
            SourceRouteSnapshot::missing(route_identity.clone(), retained_sources.clone(), state)?;
        self.observed_missing_routes
            .insert(route_identity, snapshot);
        Ok(CertifiedMissingRouteOutcome {
            retained_sources,
            deleted: false,
        })
    }
}
