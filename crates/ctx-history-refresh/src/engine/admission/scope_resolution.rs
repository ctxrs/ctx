use super::*;

impl CoreRefreshEngine {
    pub(super) fn resolve_pending_admission_claim(
        &self,
        data_root: &Path,
        claim: &PendingAdmissionClaim,
    ) -> Result<ctx_history_refresh_execution::AdmittedRefresh> {
        let discovery = self
            .runtime
            .discovery_context(data_root)?
            .with_data_root(data_root);
        if let (RefreshIntent::AutomaticMaintenance, SourceBackedRefreshScope::Exact(routes)) =
            (&claim.intent, &claim.persisted_scope)
        {
            return self.resolve_exact_maintenance_admission(data_root, claim, routes);
        }
        match &claim.intent {
            RefreshIntent::AutomaticMaintenance
            | RefreshIntent::SelectedImport {
                selection: RefreshSelection::All,
                ..
            } => {
                let resolved =
                    (self.admission_fence)(&discovery, self.journal.as_ref(), data_root, None)?;
                self.bound_resolved_admission(claim, resolved)
            }
            RefreshIntent::SelectedImport {
                selection: RefreshSelection::Provider(provider),
                ..
            } => {
                let started = StdInstant::now();
                let report =
                    ctx_history_capture::discover_provider_sources_for_provider_with_context(
                        &discovery, *provider,
                    );
                let duration = started.elapsed();
                self.resolve_scoped_admission_report(data_root, claim, &discovery, report, duration)
                    .with_context(|| {
                        format!(
                            "resolve automatic provider `{}` source refresh admission",
                            provider.as_str()
                        )
                    })
            }
            RefreshIntent::SelectedImport {
                selection: RefreshSelection::ExactSource(authority),
                ..
            } => {
                authority
                    .validate_source_roots(data_root)
                    .context("validate explicit source roots before scoped admission")?;
                let started = StdInstant::now();
                let installed_catalog = self.lock_state().watch_catalog.clone();
                let report = match installed_catalog.as_ref() {
                    Some(catalog) => authority
                        .admission_discovery_report_with_automatic_catalog(data_root, catalog)?,
                    None => authority.admission_discovery_report(data_root)?,
                };
                let duration = started.elapsed();
                self.resolve_scoped_admission_report(data_root, claim, &discovery, report, duration)
                    .context("resolve explicit-catalog source refresh admission")
            }
        }
    }

    fn resolve_exact_maintenance_admission(
        &self,
        data_root: &Path,
        claim: &PendingAdmissionClaim,
        routes: &BTreeSet<SourceRouteIdentity>,
    ) -> Result<ctx_history_refresh_execution::AdmittedRefresh> {
        let catalog = self.lock_state().watch_catalog.clone();
        #[cfg(any(test, feature = "test-support"))]
        if catalog.is_none() {
            let admitted = admitted_refresh_for_test(
                routes.iter().cloned().map(|route| (route, None)).collect(),
            );
            return self.bound_resolved_admission(claim, admitted);
        }
        let catalog = catalog
            .ok_or_else(|| anyhow!("exact maintenance admission has no installed route catalog"))?;
        let installed_routes = catalog.route_ids().cloned().collect::<BTreeSet<_>>();
        let missing = routes
            .difference(&installed_routes)
            .cloned()
            .collect::<BTreeSet<_>>();
        if !missing.is_empty() {
            bail!(
                "exact maintenance admission routes are missing from the installed catalog: {missing:?}"
            );
        }
        let started = StdInstant::now();
        let admitted =
            ctx_history_refresh_execution::AdmittedRefresh::from_exact_catalog_authority(
                routes.clone(),
                started.elapsed(),
                catalog,
            )?;
        validate_automatic_provider_source_roots_outside_data_root(
            data_root,
            admitted.discovery().report().sources.iter(),
        )
        .context("validate exact maintenance roots before admission")?;
        self.bound_resolved_admission(claim, admitted)
    }

    fn bound_resolved_admission(
        &self,
        claim: &PendingAdmissionClaim,
        resolved: ctx_history_refresh_execution::AdmittedRefresh,
    ) -> Result<ctx_history_refresh_execution::AdmittedRefresh> {
        let selected_routes = match &claim.persisted_scope {
            SourceBackedRefreshScope::All => resolved.exact_routes().clone(),
            SourceBackedRefreshScope::Exact(persisted) => {
                let missing = persisted
                    .difference(resolved.exact_routes())
                    .cloned()
                    .collect::<BTreeSet<_>>();
                if !missing.is_empty() {
                    bail!(
                        "recovered source refresh admission is missing persisted exact routes: {missing:?}"
                    );
                }
                persisted.clone()
            }
        };
        let admitted = match &claim.persisted_scope {
            SourceBackedRefreshScope::All => resolved,
            SourceBackedRefreshScope::Exact(_) => resolved.narrow_to(selected_routes)?,
        };
        Ok(admitted)
    }

    fn resolve_scoped_admission_report(
        &self,
        data_root: &Path,
        claim: &PendingAdmissionClaim,
        discovery: &DiscoveryContext,
        report: ctx_history_capture::DiscoveryReport,
        discovery_duration: StdDuration,
    ) -> Result<ctx_history_refresh_execution::AdmittedRefresh> {
        if report.sources.is_empty() && report.issues.is_empty() {
            match &claim.intent {
                RefreshIntent::SelectedImport {
                    selection: RefreshSelection::Provider(provider),
                    ..
                } => bail!(
                    "automatic provider `{}` discovery produced no executable source routes",
                    provider.as_str()
                ),
                RefreshIntent::SelectedImport {
                    selection: RefreshSelection::ExactSource(_),
                    ..
                } => {
                    bail!("explicit source catalog produced no executable source routes")
                }
                RefreshIntent::AutomaticMaintenance
                | RefreshIntent::SelectedImport {
                    selection: RefreshSelection::All,
                    ..
                } => {
                    bail!("all-automatic admission unexpectedly resolved as scoped")
                }
            }
        }
        validate_automatic_provider_source_roots_outside_data_root(
            data_root,
            report.sources.iter(),
        )
        .context("validate provider roots before scoped source refresh admission")?;
        prepare_generation_control_state(data_root)?;
        let published_state = crate::orchestration::RetainedPublishedState {
            journal: self.journal.as_ref(),
        };
        let mut admitted_refresh =
            ctx_history_refresh_execution::source_backed_admitted_discovery_from_report(
                discovery,
                report,
                discovery_duration,
                data_root,
                ctx_history_refresh_execution::AdmittedRefreshCoverage::SelectedRoutes,
                claim.intent.explicit_source_authority(),
                &published_state,
            )?;
        if let RefreshIntent::SelectedImport {
            selection: RefreshSelection::Provider(provider),
            ..
        } = &claim.intent
        {
            let catalog = admitted_refresh.discovery().watch_catalog().clone();
            let catalog_provider_routes = catalog.route_ids_for_provider(*provider);
            let freshly_executable = admitted_refresh
                .exact_routes()
                .intersection(&catalog_provider_routes)
                .cloned()
                .collect::<BTreeSet<_>>();
            let mut provider_routes = freshly_executable.clone();
            if provider_routes.is_empty() {
                bail!(
                    "automatic provider `{}` discovery produced no executable source routes",
                    provider.as_str()
                );
            }
            if let SourceBackedRefreshScope::Exact(persisted) = &claim.persisted_scope {
                let missing = persisted
                    .difference(&freshly_executable)
                    .cloned()
                    .collect::<BTreeSet<_>>();
                if !missing.is_empty() {
                    bail!(
                        "recovered scoped source refresh discovery is missing persisted exact routes: {missing:?}"
                    );
                }
                provider_routes = persisted.clone();
            }
            admitted_refresh =
                ctx_history_refresh_execution::AdmittedRefresh::from_exact_catalog_authority(
                    provider_routes,
                    discovery_duration,
                    catalog,
                )?;
        }
        let routes = admitted_refresh.exact_routes().clone();
        if routes.is_empty() {
            match &claim.intent {
                RefreshIntent::SelectedImport {
                    selection: RefreshSelection::Provider(provider),
                    ..
                } => bail!(
                    "automatic provider `{}` discovery produced no executable source routes",
                    provider.as_str()
                ),
                RefreshIntent::SelectedImport {
                    selection: RefreshSelection::ExactSource(_),
                    ..
                } => {
                    bail!("explicit source catalog produced no executable source routes")
                }
                RefreshIntent::AutomaticMaintenance
                | RefreshIntent::SelectedImport {
                    selection: RefreshSelection::All,
                    ..
                } => {
                    bail!("all-automatic admission unexpectedly resolved as scoped")
                }
            }
        }
        let selected_routes = match &claim.persisted_scope {
            SourceBackedRefreshScope::All => routes,
            SourceBackedRefreshScope::Exact(persisted) => {
                let missing = persisted
                    .difference(&routes)
                    .cloned()
                    .collect::<BTreeSet<_>>();
                if !missing.is_empty() {
                    bail!(
                        "recovered scoped source refresh discovery is missing persisted exact routes: {missing:?}"
                    );
                }
                // Provider discovery may legitimately grow while an
                // acknowledged request is interrupted. Resume only the
                // persisted exact selection; newly discovered routes belong
                // to later maintenance.
                persisted.clone()
            }
        };
        if selected_routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
            bail!("scoped source refresh admission exceeds its bounded route capacity");
        }
        let admitted = admitted_refresh.narrow_to(selected_routes)?;
        Ok(admitted)
    }
}
