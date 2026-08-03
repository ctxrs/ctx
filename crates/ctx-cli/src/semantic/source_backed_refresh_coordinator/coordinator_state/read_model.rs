use super::*;

/// Verified terminal receipt for one daemon-owned source refresh.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshReceipt {
    pub(crate) previous_generation: Option<String>,
    pub(crate) published_generation: String,
    pub(crate) generation_changed: bool,
    pub(crate) published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(crate) current: SourceBackedRefreshCurrent,
    pub(crate) route_results: Vec<SourceBackedRefreshRouteResult>,
    pub(crate) catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
}

impl SourceBackedRefreshReceipt {
    pub(in super::super) fn from_verified_publication(
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
                                SourceBackedRefreshRouteOutcome::Failed {
                                    carried_forward: false,
                                    ..
                                }
                            )
                        })
            })
        {
            bail!(
                "terminal Core publication has incomplete or inconsistent catalog lineage bindings"
            );
        }
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
            catalog_route_bindings: publication.catalog_route_bindings.clone(),
        };
        if serde_json::to_vec(&receipt.to_json()).map_or(true, |json| {
            json.len() > SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES
        }) {
            bail!("terminal Core publication cannot fit its bounded exact receipt");
        }
        Ok(receipt)
    }

    pub(crate) fn terminal_outcome(&self) -> &'static str {
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

    pub(crate) fn source_failure_total(&self) -> usize {
        self.route_results
            .iter()
            .map(|result| result.source_failure_total)
            .sum()
    }

    pub(crate) fn source_failures_omitted(&self) -> usize {
        self.source_failure_total()
            .saturating_sub(self.source_failure_diagnostic_count())
    }

    pub(crate) fn source_failure_diagnostic_count(&self) -> usize {
        self.route_results
            .iter()
            .map(|result| result.source_failures.len())
            .sum()
    }

    pub(crate) fn source_failures(
        &self,
    ) -> impl Iterator<Item = &SourceBackedRefreshSourceFailure> {
        self.route_results
            .iter()
            .flat_map(|result| result.source_failures.iter())
    }

    pub(crate) fn rejected_record_total(&self) -> u64 {
        self.route_results
            .iter()
            .map(|result| result.rejected_record_total)
            .sum()
    }

    pub(crate) fn rejection_diagnostics_omitted(&self) -> u64 {
        self.rejected_record_total()
            .saturating_sub(self.rejection_diagnostic_count() as u64)
    }

    pub(crate) fn rejection_diagnostic_count(&self) -> usize {
        self.route_results
            .iter()
            .map(|result| result.rejection_diagnostics.len())
            .sum()
    }

    pub(crate) fn rejection_diagnostics(
        &self,
    ) -> impl Iterator<Item = &SourceBackedRefreshRecordRejection> {
        self.route_results
            .iter()
            .flat_map(|result| result.rejection_diagnostics.iter())
    }

    pub(crate) fn selected_route_total(&self) -> usize {
        self.route_results.len()
    }

    pub(crate) fn successful_route_total(&self) -> usize {
        self.route_results
            .iter()
            .filter(|result| result.outcome.is_success())
            .count()
    }

    pub(crate) fn catalog_route_outcome(
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

    pub(crate) fn to_json(&self) -> Value {
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshRouteResult {
    pub(crate) route_identity: String,
    pub(crate) outcome: SourceBackedRefreshRouteOutcome,
    /// Exact number of source-level failures observed inside this route.
    pub(crate) source_failure_total: usize,
    /// Bounded details owned by this route result; omitted detail is derived
    /// from `source_failure_total` and this vector's length.
    pub(crate) source_failures: Vec<SourceBackedRefreshSourceFailure>,
    /// Exact rejected-record cardinality in the route's committed sources.
    pub(crate) rejected_record_total: u64,
    /// Bounded path/line/payload diagnostics owned by this route result.
    pub(crate) rejection_diagnostics: Vec<SourceBackedRefreshRecordRejection>,
}

impl SourceBackedRefreshRouteResult {
    pub(crate) fn succeeded(route_identity: String, changed: bool) -> Self {
        Self {
            route_identity,
            outcome: SourceBackedRefreshRouteOutcome::Succeeded { changed },
            source_failure_total: 0,
            source_failures: Vec::new(),
            rejected_record_total: 0,
            rejection_diagnostics: Vec::new(),
        }
    }

    pub(crate) fn failed(route_identity: String, class: String, carried_forward: bool) -> Self {
        Self {
            route_identity,
            outcome: SourceBackedRefreshRouteOutcome::Failed {
                class,
                carried_forward,
            },
            source_failure_total: 1,
            source_failures: Vec::new(),
            rejected_record_total: 0,
            rejection_diagnostics: Vec::new(),
        }
    }

    pub(crate) fn has_source_failures(&self) -> bool {
        self.source_failure_total != 0
    }

    pub(in super::super) fn validate_source_failures(&self) -> Result<()> {
        if self.source_failures.len() > self.source_failure_total {
            bail!("terminal route result has more diagnostics than source failures");
        }
        if self.outcome.is_failure() && self.source_failure_total == 0 {
            bail!("failed terminal route result has no source failure count");
        }
        let mut diagnostics = BTreeSet::new();
        for failure in &self.source_failures {
            if failure.route_identity != self.route_identity
                || !is_sha256_identity(&failure.source_identity)
                || !source_failure_class_is_typed(&failure.class)
                || failure.provider.is_empty()
                || failure.source_selector.is_empty()
                || failure.detail.is_empty()
                || !diagnostics.insert((
                    failure.source_identity.as_str(),
                    failure.provider.as_str(),
                    failure.class.as_str(),
                    failure.carried_forward,
                    failure.source_selector.as_str(),
                    failure.detail.as_str(),
                ))
            {
                bail!("terminal route result contains an inconsistent source diagnostic");
            }
        }
        if self.outcome.is_failure()
            && (self.rejected_record_total != 0 || !self.rejection_diagnostics.is_empty())
        {
            bail!("failed terminal route result contains successful-route rejections");
        }
        if self.rejection_diagnostics.len() as u64 > self.rejected_record_total {
            bail!("terminal route result has more rejection diagnostics than rejected records");
        }
        let mut rejections = BTreeSet::new();
        for rejection in &self.rejection_diagnostics {
            if rejection.route_identity != self.route_identity
                || !is_sha256_identity(&rejection.source_identity)
                || rejection.provider.is_empty()
                || rejection.source_selector.is_empty()
                || rejection.line == 0
                || rejection.payload_type.is_empty()
                || !record_rejection_class_is_typed(&rejection.class)
                || rejection.detail.is_empty()
                || !rejections.insert((
                    rejection.source_identity.as_str(),
                    rejection.provider.as_str(),
                    rejection.source_selector.as_str(),
                    rejection.line,
                    rejection.payload_type.as_str(),
                    rejection.class.as_str(),
                    rejection.detail.as_str(),
                ))
            {
                bail!("terminal route result contains an inconsistent rejection diagnostic");
            }
        }
        Ok(())
    }

    pub(super) fn compact_json(&self) -> Value {
        let details = self
            .source_failures
            .iter()
            .map(SourceBackedRefreshSourceFailure::compact_json)
            .collect::<Vec<_>>();
        match &self.outcome {
            SourceBackedRefreshRouteOutcome::Succeeded { changed }
                if self.source_failure_total == 0 && self.rejected_record_total == 0 =>
            {
                json!(["s", changed])
            }
            SourceBackedRefreshRouteOutcome::Succeeded { changed } => {
                json!([
                    "s",
                    changed,
                    self.source_failure_total,
                    details,
                    self.rejected_record_total,
                    self.rejection_diagnostics
                        .iter()
                        .map(SourceBackedRefreshRecordRejection::compact_json)
                        .collect::<Vec<_>>(),
                ])
            }
            SourceBackedRefreshRouteOutcome::Failed {
                class,
                carried_forward,
            } => json!([
                "f",
                source_failure_class_code(class),
                carried_forward,
                self.source_failure_total,
                details,
            ]),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum SourceBackedRefreshRouteOutcome {
    Succeeded {
        changed: bool,
    },
    Failed {
        class: String,
        carried_forward: bool,
    },
}

impl SourceBackedRefreshRouteOutcome {
    pub(crate) fn is_success(&self) -> bool {
        matches!(self, Self::Succeeded { .. })
    }

    pub(crate) fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    #[cfg(test)]
    pub(crate) fn changed(&self) -> Option<bool> {
        match self {
            Self::Succeeded { changed } => Some(*changed),
            Self::Failed { .. } => None,
        }
    }

    pub(crate) fn failure_class(&self) -> Option<&str> {
        match self {
            Self::Succeeded { .. } => None,
            Self::Failed { class, .. } => Some(class),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshCatalogRouteOutcome {
    pub(crate) catalog_lineage: String,
    pub(crate) route_identity: String,
    pub(crate) outcome: String,
    pub(crate) failure_class: Option<String>,
    pub(crate) changed: Option<bool>,
    pub(crate) source_failure_total: usize,
    pub(crate) rejected_record_total: u64,
}

impl SourceBackedRefreshCatalogRouteOutcome {
    fn from_result(catalog_lineage: String, result: &SourceBackedRefreshRouteResult) -> Self {
        let (outcome, failure_class, changed) = match &result.outcome {
            SourceBackedRefreshRouteOutcome::Succeeded { changed } => {
                let outcome = match (
                    result.has_source_failures(),
                    result.rejected_record_total != 0,
                ) {
                    (false, false) => "succeeded",
                    (false, true) => "completed_with_rejections",
                    (true, false) => "succeeded_with_source_failures",
                    (true, true) => "completed_with_rejections_and_source_failures",
                };
                (outcome.to_owned(), None, Some(*changed))
            }
            SourceBackedRefreshRouteOutcome::Failed { class, .. } => {
                ("failed".to_owned(), Some(class.clone()), None)
            }
        };
        Self {
            catalog_lineage,
            route_identity: result.route_identity.clone(),
            outcome,
            failure_class,
            changed,
            source_failure_total: result.source_failure_total,
            rejected_record_total: result.rejected_record_total,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshRecordRejection {
    pub(crate) route_identity: String,
    pub(crate) source_identity: String,
    pub(crate) provider: String,
    pub(crate) source_selector: String,
    pub(crate) line: u64,
    pub(crate) payload_type: String,
    pub(crate) class: String,
    pub(crate) detail: String,
}

impl SourceBackedRefreshRecordRejection {
    fn compact_json(&self) -> Value {
        json!([
            self.source_identity,
            self.provider,
            self.source_selector,
            self.line,
            self.payload_type,
            record_rejection_class_code(&self.class),
            self.detail,
        ])
    }
}

fn record_rejection_class_is_typed(class: &str) -> bool {
    matches!(class, "malformed_record" | "unsupported_record")
}

fn record_rejection_class_code(class: &str) -> &'static str {
    match class {
        "malformed_record" => "m",
        "unsupported_record" => "u",
        _ => "?",
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshSourceFailure {
    pub(crate) route_identity: String,
    pub(crate) source_identity: String,
    pub(crate) provider: String,
    pub(crate) class: String,
    pub(crate) carried_forward: bool,
    pub(crate) source_selector: String,
    pub(crate) detail: String,
}

impl SourceBackedRefreshSourceFailure {
    fn compact_json(&self) -> Value {
        json!([
            self.source_identity,
            self.provider,
            source_failure_class_code(&self.class),
            self.carried_forward,
            self.source_selector,
            self.detail,
        ])
    }
}

fn source_failure_class_is_typed(class: &str) -> bool {
    matches!(
        class,
        "unavailable" | "source_changed" | "unreadable" | "incompatible"
    )
}

fn source_failure_class_code(class: &str) -> &'static str {
    match class {
        "unavailable" => "u",
        "source_changed" => "c",
        "unreadable" => "r",
        "incompatible" => "i",
        _ => "?",
    }
}

#[derive(Debug, Clone)]
pub(super) struct SourceBackedRefreshAttempt {
    pub(super) request_id: String,
    pub(super) state: SourceBackedRefreshState,
    pub(super) requested_at_ms: i64,
    pub(super) started_at_ms: Option<i64>,
    pub(super) finished_at_ms: Option<i64>,
    pub(super) previous_generation: Option<String>,
    pub(super) published_generation: Option<String>,
    pub(super) refresh_scope: SourceBackedRefreshScope,
    pub(super) operation: SourceBackedRefreshOperation,
    pub(super) requested_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(super) coalesced_requests: u64,
    pub(super) progress: SourceBackedRefreshProgress,
    pub(super) scanned_routes: Option<usize>,
    pub(super) unsupported_routes: Option<usize>,
    pub(super) certified_source_count: Option<usize>,
    pub(super) certified_source_bytes: Option<u64>,
    /// Request-scoped route/result/rejection facts. This is mutable daemon
    /// status, never publication authority.
    pub(super) receipt: Option<SourceBackedRefreshReceipt>,
    /// The sole publication receipt, decoded from Core CommitPayload metadata.
    pub(super) publication_receipt: Option<SourceBackedRefreshReceipt>,
    pub(super) route_observations: BTreeMap<SourceRouteIdentity, String>,
    pub(super) timings: Option<SourceBackedRefreshTimings>,
    pub(super) publication_probe_us: u64,
    pub(super) daemon_mode: DaemonMode,
    pub(super) trigger: &'static str,
    pub(super) trigger_provenance: &'static str,
    pub(super) failure_type: Option<&'static str>,
    pub(super) last_error: Option<String>,
}

impl SourceBackedRefreshAttempt {
    pub(super) fn recovered_published(
        job: &Value,
        metadata: &SourceBackedPublicationMetadata,
        receipt: SourceBackedRefreshReceipt,
    ) -> Self {
        let now = utc_now().timestamp_millis();
        let route_total = receipt.route_results.len();
        let unsupported_routes = receipt
            .route_results
            .iter()
            .filter(|result| result.outcome.failure_class() == Some("incompatible"))
            .count();
        Self {
            request_id: metadata.request_id.clone(),
            state: SourceBackedRefreshState::Published,
            requested_at_ms: job
                .get("requested_at_ms")
                .and_then(Value::as_i64)
                .unwrap_or(now),
            started_at_ms: job.get("started_at_ms").and_then(Value::as_i64),
            finished_at_ms: Some(now),
            previous_generation: receipt.previous_generation.clone(),
            published_generation: Some(receipt.published_generation.clone()),
            refresh_scope: metadata.refresh_scope.clone(),
            operation: metadata.operation,
            requested_explicit_source_catalog: None,
            coalesced_requests: job
                .get("coalesced_requests")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            progress: SourceBackedRefreshProgress {
                phase: "published".to_owned(),
                completed_sources: route_total,
                total_sources: route_total,
                ..SourceBackedRefreshProgress::default()
            },
            scanned_routes: Some(route_total),
            unsupported_routes: Some(unsupported_routes),
            certified_source_count: Some(receipt.current.source_count),
            certified_source_bytes: Some(receipt.current.certified_source_bytes),
            receipt: Some(receipt.clone()),
            publication_receipt: Some(receipt),
            route_observations: metadata.route_observations.clone(),
            timings: Some(SourceBackedRefreshTimings::default()),
            publication_probe_us: 0,
            daemon_mode: DaemonMode::default(),
            trigger: "recovery",
            trigger_provenance: "commit_payload",
            failure_type: None,
            last_error: None,
        }
    }

    fn failure_code(&self) -> Option<&'static str> {
        self.last_error
            .as_deref()
            .filter(|error| error.contains(TERMINAL_COVERAGE_ERROR_CODE))
            .map(|_| TERMINAL_COVERAGE_ERROR_CODE)
    }

    fn failure_reason(&self) -> Option<&'static str> {
        self.failure_code()
            .map(|_| "provider_terminal_coverage_unavailable")
    }

    fn request_generation_changed(&self) -> Option<bool> {
        self.receipt
            .as_ref()
            .map(|_| self.published_generation != self.previous_generation)
    }

    fn request_outcome_receipt(&self) -> Option<&SourceBackedRefreshReceipt> {
        let request = self.receipt.as_ref()?;
        self.publication_receipt
            .as_ref()
            .filter(|publication| *publication != request)
            .map(|_| request)
    }

    pub(super) fn to_json(&self) -> Value {
        let publication_receipt = self.publication_receipt.as_ref().or(self.receipt.as_ref());
        compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "operation": self.operation.as_str(),
            "requested_at_ms": self.requested_at_ms,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "refresh_scope": refresh_scope_json(&self.refresh_scope),
            "requested_explicit_source_catalog": self.receipt.is_none().then(|| {
                self.requested_explicit_source_catalog
                    .as_ref()
                    .map(ExplicitSourceCatalogAuthority::to_json)
            }).flatten(),
            "generation_changed": self.request_generation_changed(),
            "receipt": publication_receipt.map(SourceBackedRefreshReceipt::to_json),
            "request_outcome": self.request_outcome_receipt()
                .map(SourceBackedRefreshReceipt::to_json),
            "outcome": self.receipt.as_ref().map(SourceBackedRefreshReceipt::terminal_outcome),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json(),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings_json(),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "failure_type": self.failure_type,
            "error_code": self.failure_code(),
            "reason": self.failure_reason(),
            "last_error": self.last_error,
        }))
    }

    pub(super) fn job_json(&self) -> Value {
        let status = match self.state {
            SourceBackedRefreshState::Published => "completed",
            SourceBackedRefreshState::Failed => "failed",
            SourceBackedRefreshState::Queued | SourceBackedRefreshState::Running => "running",
        };
        let publication_receipt = self.publication_receipt.as_ref().or(self.receipt.as_ref());
        compact_json(json!({
            "mode": SourceBackedRefreshMode::Background.as_str(),
            "owner": "daemon",
            "kind": "core_refresh",
            "status": status,
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "operation": self.operation.as_str(),
            "source_count": self.progress.total_sources,
            "last_run_at_ms": self.started_at_ms.unwrap_or(self.requested_at_ms),
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "refresh_scope": refresh_scope_json(&self.refresh_scope),
            "requested_explicit_source_catalog": self.receipt.is_none().then(|| {
                self.requested_explicit_source_catalog
                    .as_ref()
                    .map(ExplicitSourceCatalogAuthority::to_json)
            }).flatten(),
            "generation_changed": self.request_generation_changed(),
            "receipt": publication_receipt.map(SourceBackedRefreshReceipt::to_json),
            "request_outcome": self.request_outcome_receipt()
                .map(SourceBackedRefreshReceipt::to_json),
            "outcome": self.receipt.as_ref().map(SourceBackedRefreshReceipt::terminal_outcome),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json(),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings_json(),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "failure_type": self.failure_type,
            "error_code": self.failure_code(),
            "reason": self.failure_reason(),
            "last_error": self.last_error,
        }))
    }

    fn timings_json(&self) -> Option<Value> {
        self.timings.map(|timings| {
            let mut timings = timings.to_json();
            timings["publication_probe"] = json!(self.publication_probe_us);
            timings
        })
    }
}

pub(crate) fn refresh_scope_json(scope: &SourceBackedRefreshScope) -> Value {
    match scope {
        SourceBackedRefreshScope::All => json!({ "kind": "all" }),
        SourceBackedRefreshScope::Exact(routes) => json!({
            "kind": "exact",
            "routes": routes.iter().map(SourceRouteIdentity::as_str).collect::<Vec<_>>(),
        }),
    }
}

pub(crate) fn refresh_scope_from_json(value: Option<&Value>) -> Result<SourceBackedRefreshScope> {
    let value = value.ok_or_else(|| anyhow!("source refresh recovery scope is missing"))?;
    match value.get("kind").and_then(Value::as_str) {
        Some("all") => Ok(SourceBackedRefreshScope::All),
        Some("exact") => {
            let routes = value
                .get("routes")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("exact source refresh recovery scope has no route list"))?;
            if routes.is_empty() || routes.len() > SOURCE_REFRESH_RECOVERY_ROUTE_LIMIT {
                bail!(
                    "exact source refresh recovery scope must contain 1..={SOURCE_REFRESH_RECOVERY_ROUTE_LIMIT} routes"
                );
            }
            routes
                .iter()
                .map(|route| {
                    let route = route.as_str().ok_or_else(|| {
                        anyhow!("exact source refresh recovery route is not a string")
                    })?;
                    SourceRouteIdentity::from_sha256(route.to_owned()).map_err(Into::into)
                })
                .collect::<Result<BTreeSet<_>>>()
                .map(SourceBackedRefreshScope::Exact)
        }
        Some(kind) => bail!("unknown source refresh recovery scope kind `{kind}`"),
        None => bail!("source refresh recovery scope kind is missing"),
    }
}
