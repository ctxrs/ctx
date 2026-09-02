use super::*;

/// Verified terminal receipt for one daemon-owned source refresh.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedRefreshReceipt {
    pub previous_generation: Option<String>,
    pub published_generation: String,
    pub generation_changed: bool,
    pub published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub current: SourceBackedRefreshCurrent,
    pub route_results: Vec<SourceBackedRefreshRouteResult>,
    pub zero_source_authority: Vec<SourceBackedZeroSourceAuthority>,
    pub catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
}

impl SourceBackedRefreshReceipt {
    #[doc(hidden)]
    pub fn from_verified_publication(
        previous_generation: Option<String>,
        published_generation: String,
        publication: &SourceBackedRefreshPublication,
    ) -> Result<Self> {
        if publication.route_results.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
            bail!(
                "terminal Core publication exceeds the bounded route-result limit of {SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT}"
            );
        }
        let mut routes = BTreeMap::new();
        for result in &publication.route_results {
            SourceRouteIdentity::from_sha256(result.route_identity.clone())
                .context("validate terminal route-result identity")?;
            if routes
                .insert(result.route_identity.as_str(), result)
                .is_some()
            {
                bail!("terminal Core publication contains a duplicate route result");
            }
            if result
                .outcome
                .failure_class()
                .is_some_and(|class| !source_failure_class_is_typed(class))
            {
                bail!("terminal Core publication contains an untyped route failure");
            }
            result.validate_source_failures()?;
        }
        let _source_failure_total =
            publication
                .route_results
                .iter()
                .try_fold(0_usize, |total, result| {
                    total
                        .checked_add(result.source_failure_total)
                        .ok_or_else(|| {
                            anyhow!("terminal Core publication source-failure total overflow")
                        })
                })?;
        let rejected_record_total =
            publication
                .route_results
                .iter()
                .try_fold(0_u64, |total, result| {
                    total
                        .checked_add(result.rejected_record_total)
                        .ok_or_else(|| {
                            anyhow!("terminal Core publication rejected-record total overflow")
                        })
                })?;
        if rejected_record_total > publication.current.rejected_records {
            bail!("terminal Core publication route rejections exceed the committed generation");
        }
        let expected_lineages = publication
            .published_explicit_source_catalog
            .as_ref()
            .map(ExplicitSourceCatalogAuthority::route_lineages)
            .unwrap_or_default();
        let actual_lineages = publication
            .catalog_route_bindings
            .iter()
            .map(|binding| binding.catalog_lineage.clone())
            .collect::<BTreeSet<_>>();
        if actual_lineages.len() != publication.catalog_route_bindings.len()
            || !expected_lineages.is_subset(&actual_lineages)
            || publication.catalog_route_bindings.iter().any(|binding| {
                SourceRouteIdentity::from_sha256(binding.route_identity.clone()).is_err()
            })
            || publication.catalog_route_bindings.iter().any(|binding| {
                !expected_lineages.contains(&binding.catalog_lineage)
                    && !routes
                        .get(binding.route_identity.as_str())
                        .is_some_and(|result| {
                            matches!(
                                result.outcome,
                                SourceBackedRefreshRouteOutcome::Failed { .. }
                            )
                        })
            })
        {
            bail!(
                "terminal Core publication has incomplete or inconsistent catalog lineage bindings"
            );
        }
        crate::receipt_parse::validate_zero_source_authority(
            &published_generation,
            publication.current.source_count,
            &publication.route_results,
            &publication.zero_source_authority,
            false,
        )?;
        let receipt = Self {
            generation_changed: previous_generation.as_deref()
                != Some(published_generation.as_str()),
            previous_generation,
            published_generation,
            published_explicit_source_catalog: publication
                .published_explicit_source_catalog
                .clone(),
            current: publication.current,
            route_results: publication.route_results.clone(),
            zero_source_authority: publication.zero_source_authority.clone(),
            catalog_route_bindings: publication.catalog_route_bindings.clone(),
        };
        if serde_json::to_vec(&receipt.to_json()).map_or(true, |json| {
            json.len() > SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES
        }) {
            bail!("terminal Core publication cannot fit its bounded exact receipt");
        }
        Ok(receipt)
    }

    pub fn terminal_outcome(&self) -> &'static str {
        match (
            self.source_failure_total() != 0,
            self.rejected_record_total() != 0,
        ) {
            (false, false) => "completed",
            (false, true) => "completed_with_rejections",
            (true, false) => "completed_with_source_failures",
            (true, true) => "completed_with_rejections_and_source_failures",
        }
    }

    /// Request-scoped source-route contribution in the exact retained
    /// generation.
    ///
    /// `current.source_count` is the cardinality of every certified source in
    /// the generation. It can include multiple sources per route and sources
    /// published by unrelated providers. Receipt route outcomes are request
    /// scoped, while the verified manifest decides whether each successful
    /// route has a present certified source contribution. Failed or missing
    /// routes do not become request contributions merely because an older
    /// source was carried forward.
    pub fn source_count(&self, verified: &VerifiedIndex) -> usize {
        debug_assert_eq!(self.published_generation, verified.generation_id());
        self.route_results
            .iter()
            .filter(|result| {
                result.outcome.is_success()
                    && SourceRouteIdentity::from_sha256(result.route_identity.clone())
                        .ok()
                        .and_then(|route| verified.manifest().source_route(&route))
                        .is_some_and(|route| {
                            route.missing_state().is_none() && !route.sources().is_empty()
                        })
            })
            .count()
    }

    /// Whether one admitted explicit catalog lineage attempted and retained
    /// any records in the verified published generation.
    pub fn catalog_route_content(
        &self,
        verified: &VerifiedIndex,
        catalog_lineage: &str,
    ) -> Result<(bool, bool)> {
        if self.published_generation != verified.generation_id() {
            bail!("terminal receipt and verified generation do not match");
        }
        let outcome = self
            .catalog_route_outcome(catalog_lineage)
            .context("terminal receipt has no exact catalog-lineage result")?;
        let route = SourceRouteIdentity::from_sha256(outcome.route_identity)
            .context("validate exact catalog route identity")?;
        let Some(snapshot) = verified.manifest().source_route(&route) else {
            return Ok((false, false));
        };
        if snapshot.missing_state().is_some() {
            return Ok((false, false));
        }
        snapshot
            .sources()
            .iter()
            .try_fold((false, false), |(attempted, usable), source_key| {
                let source = verified
                    .manifest()
                    .sources
                    .iter()
                    .find(|source| source.observation().source() == source_key)
                    .context("exact catalog route names a missing certified source")?;
                let counts = source.counts();
                Ok((
                    attempted || counts.complete_records > 0,
                    usable || counts.retained_records > 0,
                ))
            })
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn state_only_source_count(&self) -> usize {
        self.route_results
            .iter()
            .filter(|result| result.outcome.is_success())
            .count()
    }

    pub fn source_failure_total(&self) -> usize {
        self.route_results
            .iter()
            .map(|result| result.source_failure_total)
            .sum()
    }

    pub fn source_failures_omitted(&self) -> usize {
        self.source_failure_total()
            .saturating_sub(self.source_failure_diagnostic_count())
    }

    pub fn source_failure_diagnostic_count(&self) -> usize {
        self.route_results
            .iter()
            .map(|result| result.source_failures.len())
            .sum()
    }

    pub fn source_failures(&self) -> impl Iterator<Item = &SourceBackedRefreshSourceFailure> {
        self.route_results
            .iter()
            .flat_map(|result| result.source_failures.iter())
    }

    pub fn rejected_record_total(&self) -> u64 {
        self.route_results
            .iter()
            .map(|result| result.rejected_record_total)
            .sum()
    }

    pub fn rejection_diagnostics_omitted(&self) -> u64 {
        self.rejected_record_total()
            .saturating_sub(self.rejection_diagnostic_count() as u64)
    }

    pub fn rejection_diagnostic_count(&self) -> usize {
        self.route_results
            .iter()
            .map(|result| result.rejection_diagnostics.len())
            .sum()
    }

    pub fn rejection_diagnostics(
        &self,
    ) -> impl Iterator<Item = &SourceBackedRefreshRecordRejection> {
        self.route_results
            .iter()
            .flat_map(|result| result.rejection_diagnostics.iter())
    }

    pub fn selected_route_total(&self) -> usize {
        self.route_results.len()
    }

    pub fn successful_route_total(&self) -> usize {
        self.route_results
            .iter()
            .filter(|result| result.outcome.is_success())
            .count()
    }

    #[doc(hidden)]
    pub fn route_retry_dispositions(
        &self,
    ) -> (BTreeSet<SourceRouteIdentity>, BTreeSet<SourceRouteIdentity>) {
        let mut retryable = BTreeSet::new();
        let mut blocked = BTreeSet::new();
        for result in &self.route_results {
            let Some(disposition) = source_backed_route_retry_disposition(result) else {
                continue;
            };
            let Ok(route) = SourceRouteIdentity::from_sha256(result.route_identity.clone()) else {
                continue;
            };
            if disposition {
                retryable.insert(route);
            } else {
                blocked.insert(route);
            }
        }
        (retryable, blocked)
    }

    pub fn catalog_route_outcome(
        &self,
        catalog_lineage: &str,
    ) -> Option<SourceBackedRefreshCatalogRouteOutcome> {
        let binding = self
            .catalog_route_bindings
            .iter()
            .find(|binding| binding.catalog_lineage == catalog_lineage)?;
        let result = self
            .route_results
            .iter()
            .find(|result| result.route_identity == binding.route_identity)?;
        Some(SourceBackedRefreshCatalogRouteOutcome::from_result(
            binding.catalog_lineage.clone(),
            result,
        ))
    }

    pub fn to_json(&self) -> Value {
        // Keep every route outcome exact. Diagnostic detail is copied into
        // those same outcomes until the bounded IPC budget is reached; exact
        // totals make any omitted detail explicit without a second authority.
        let mut transmitted_results = self
            .route_results
            .iter()
            .cloned()
            .map(|mut result| {
                result.source_failures.clear();
                result.rejection_diagnostics.clear();
                result
            })
            .collect::<Vec<_>>();
        let catalog_route_bindings = self.catalog_route_bindings_json();
        let base = self.wire_json(&transmitted_results, &catalog_route_bindings);
        let mut encoded_bytes = serde_json::to_vec(&base).map_or(usize::MAX, |json| json.len());
        'diagnostics: for (result_index, result) in self.route_results.iter().enumerate() {
            for failure in &result.source_failures {
                let detail_bytes = serde_json::to_vec(&failure.compact_json())
                    .map_or(usize::MAX, |json| json.len());
                let separator_bytes =
                    usize::from(!transmitted_results[result_index].source_failures.is_empty());
                let added_bytes = detail_bytes.saturating_add(separator_bytes);
                if encoded_bytes.saturating_add(added_bytes)
                    > SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES
                {
                    break 'diagnostics;
                }
                transmitted_results[result_index]
                    .source_failures
                    .push(failure.clone());
                encoded_bytes = encoded_bytes.saturating_add(added_bytes);
            }
            for rejection in &result.rejection_diagnostics {
                let detail_bytes = serde_json::to_vec(&rejection.compact_json())
                    .map_or(usize::MAX, |json| json.len());
                let separator_bytes = usize::from(
                    !transmitted_results[result_index]
                        .rejection_diagnostics
                        .is_empty(),
                );
                let added_bytes = detail_bytes.saturating_add(separator_bytes);
                if encoded_bytes.saturating_add(added_bytes)
                    > SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES
                {
                    break 'diagnostics;
                }
                transmitted_results[result_index]
                    .rejection_diagnostics
                    .push(rejection.clone());
                encoded_bytes = encoded_bytes.saturating_add(added_bytes);
            }
        }
        self.wire_json(&transmitted_results, &catalog_route_bindings)
    }

    fn wire_json(
        &self,
        route_results: &[SourceBackedRefreshRouteResult],
        catalog_route_bindings: &Value,
    ) -> Value {
        let source_failure_total = route_results
            .iter()
            .map(|result| result.source_failure_total)
            .sum::<usize>();
        let source_failure_details = route_results
            .iter()
            .map(|result| result.source_failures.len())
            .sum::<usize>();
        let rejected_record_total = route_results
            .iter()
            .map(|result| result.rejected_record_total)
            .sum::<u64>();
        let rejection_diagnostic_total = route_results
            .iter()
            .map(|result| result.rejection_diagnostics.len() as u64)
            .sum::<u64>();
        compact_json(json!({
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "generation_changed": self.generation_changed,
            "published_explicit_source_catalog": self
                .published_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "current": self.current.to_json(),
            "outcome": self.terminal_outcome(),
            "selected_route_total": self.selected_route_total(),
            "successful_route_total": self.successful_route_total(),
            "source_failure_total": source_failure_total,
            "source_failures_omitted": source_failure_total
                .saturating_sub(source_failure_details),
            "rejected_record_total": rejected_record_total,
            "rejection_diagnostics_omitted": rejected_record_total
                .saturating_sub(rejection_diagnostic_total),
            "route_results": self.route_results_json(route_results),
            "zero_source_authority": crate::receipt_parse::zero_source_authority_json(
                &self.zero_source_authority,
                route_results,
            ),
            "catalog_route_bindings": catalog_route_bindings,
        }))
    }

    fn route_results_json(&self, route_results: &[SourceBackedRefreshRouteResult]) -> Value {
        let outcomes = route_results
            .iter()
            .map(|result| (result.route_identity.clone(), result.compact_json()))
            .collect::<serde_json::Map<_, _>>();
        Value::Object(outcomes)
    }

    fn catalog_route_bindings_json(&self) -> Value {
        Value::Object(
            self.catalog_route_bindings
                .iter()
                .map(|binding| {
                    (
                        binding.catalog_lineage.clone(),
                        json!(binding.route_identity),
                    )
                })
                .collect(),
        )
    }
}
