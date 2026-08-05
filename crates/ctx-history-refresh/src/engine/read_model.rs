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
    pub catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
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

    pub(super) fn route_retry_dispositions(
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
pub struct SourceBackedRefreshRouteResult {
    pub route_identity: String,
    pub outcome: SourceBackedRefreshRouteOutcome,
    /// Exact number of source-level failures observed inside this route.
    pub source_failure_total: usize,
    /// Bounded details owned by this route result; omitted detail is derived
    /// from `source_failure_total` and this vector's length.
    pub source_failures: Vec<SourceBackedRefreshSourceFailure>,
    /// Exact rejected-record cardinality in the route's committed sources.
    pub rejected_record_total: u64,
    /// Bounded path/line/payload diagnostics owned by this route result.
    pub rejection_diagnostics: Vec<SourceBackedRefreshRecordRejection>,
}

impl SourceBackedRefreshRouteResult {
    pub fn succeeded(route_identity: String, changed: bool) -> Self {
        Self {
            route_identity,
            outcome: SourceBackedRefreshRouteOutcome::Succeeded { changed },
            source_failure_total: 0,
            source_failures: Vec::new(),
            rejected_record_total: 0,
            rejection_diagnostics: Vec::new(),
        }
    }

    pub fn failed(route_identity: String, class: String, carried_forward: bool) -> Self {
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

    pub fn has_source_failures(&self) -> bool {
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
pub enum SourceBackedRefreshRouteOutcome {
    Succeeded {
        changed: bool,
    },
    Failed {
        class: String,
        carried_forward: bool,
    },
}

impl SourceBackedRefreshRouteOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Succeeded { .. })
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    #[cfg(test)]
    pub fn changed(&self) -> Option<bool> {
        match self {
            Self::Succeeded { changed } => Some(*changed),
            Self::Failed { .. } => None,
        }
    }

    pub fn failure_class(&self) -> Option<&str> {
        match self {
            Self::Succeeded { .. } => None,
            Self::Failed { class, .. } => Some(class),
        }
    }
}

/// Returns `None` when the route is clean, `Some(true)` when it should retry,
/// and `Some(false)` when the admitted observation should remain blocked.
/// The route outcome and exact source-failure count are authoritative. A
/// truncated diagnostic vector can only make a partial success retryable; it
/// can never incorrectly classify unknown failures as permanently blocked.
pub(super) fn source_backed_route_retry_disposition(
    result: &SourceBackedRefreshRouteResult,
) -> Option<bool> {
    if let Some(class) = result.outcome.failure_class() {
        return Some(matches!(class, "unavailable" | "source_changed"));
    }
    if result.source_failure_total == 0 {
        return None;
    }
    if result.source_failures.len() < result.source_failure_total {
        return Some(true);
    }
    Some(
        result
            .source_failures
            .iter()
            .any(|failure| matches!(failure.class.as_str(), "unavailable" | "source_changed")),
    )
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedRefreshCatalogRouteOutcome {
    pub catalog_lineage: String,
    pub route_identity: String,
    pub outcome: String,
    pub failure_class: Option<String>,
    pub changed: Option<bool>,
    pub source_failure_total: usize,
    pub rejected_record_total: u64,
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
pub struct SourceBackedRefreshRecordRejection {
    pub route_identity: String,
    pub source_identity: String,
    pub provider: String,
    pub source_selector: String,
    pub line: u64,
    pub payload_type: String,
    pub class: String,
    pub detail: String,
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
pub struct SourceBackedRefreshSourceFailure {
    pub route_identity: String,
    pub source_identity: String,
    pub provider: String,
    pub class: String,
    pub carried_forward: bool,
    pub source_selector: String,
    pub detail: String,
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct SourceBackedRefreshFailureOutcome {
    pub(super) code: &'static str,
    pub(super) class: &'static str,
    pub(super) retryable: bool,
    pub(super) affected_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) retryable_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) blocked_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) retry_advice: Option<&'static str>,
}

impl SourceBackedRefreshFailureOutcome {
    pub(super) fn new(
        code: &'static str,
        class: &'static str,
        retryable: bool,
        affected_routes: BTreeSet<SourceRouteIdentity>,
        retry_advice: Option<&'static str>,
    ) -> Self {
        let (retryable_routes, blocked_routes) = if retryable {
            (affected_routes.clone(), BTreeSet::new())
        } else {
            (BTreeSet::new(), affected_routes.clone())
        };
        Self::with_route_dispositions(
            code,
            class,
            retryable,
            retryable_routes,
            blocked_routes,
            retry_advice,
        )
    }

    pub(super) fn with_route_dispositions(
        code: &'static str,
        class: &'static str,
        retryable: bool,
        retryable_routes: BTreeSet<SourceRouteIdentity>,
        blocked_routes: BTreeSet<SourceRouteIdentity>,
        retry_advice: Option<&'static str>,
    ) -> Self {
        let affected_routes = retryable_routes.union(&blocked_routes).cloned().collect();
        Self {
            code,
            class,
            retryable,
            affected_routes,
            retryable_routes,
            blocked_routes,
            retry_advice,
        }
    }

    fn to_json(
        &self,
        physical_attempt_id: &str,
        retained_generation: Option<&str>,
        detail: Option<&str>,
    ) -> Value {
        compact_json(json!({
            "code": self.code,
            "class": self.class,
            "retryable": self.retryable,
            "affected_routes": self.affected_routes
                .iter()
                .map(SourceRouteIdentity::as_str)
                .collect::<Vec<_>>(),
            "retryable_routes": self.retryable_routes
                .iter()
                .map(SourceRouteIdentity::as_str)
                .collect::<Vec<_>>(),
            "blocked_routes": self.blocked_routes
                .iter()
                .map(SourceRouteIdentity::as_str)
                .collect::<Vec<_>>(),
            "physical_attempt_id": physical_attempt_id,
            "retained_generation": retained_generation,
            "retry_advice": self.retry_advice,
            "detail": detail,
        }))
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
    pub(super) fresh_after_admitted_snapshot: bool,
    pub(super) request_fingerprint: Option<String>,
    pub(super) admission_durability_indeterminate: bool,
    pub(super) coalesced_into_request_id: Option<String>,
    pub(super) coalesced_logical_demands: u64,
    pub(super) coalesced_requests: u64,
    pub(super) progress: SourceBackedRefreshProgress,
    pub(super) progress_total_sources_known: bool,
    pub(super) physical_attempt_id: Option<String>,
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
    pub(super) daemon_mode: String,
    pub(super) trigger: &'static str,
    pub(super) trigger_provenance: &'static str,
    pub(super) failure_type: Option<&'static str>,
    pub(super) failure_outcome: Option<SourceBackedRefreshFailureOutcome>,
    pub(super) last_error: Option<String>,
}

impl SourceBackedRefreshAttempt {
    fn source_count(&self) -> usize {
        self.certified_source_count
            .or_else(|| {
                self.publication_receipt
                    .as_ref()
                    .map(|receipt| receipt.current.source_count)
            })
            .or_else(|| {
                self.receipt
                    .as_ref()
                    .map(|receipt| receipt.current.source_count)
            })
            .or(self.scanned_routes)
            .unwrap_or(self.progress.total_sources)
    }

    fn failure_code(&self) -> Option<&'static str> {
        self.last_error
            .as_deref()
            .filter(|error| error.contains(TERMINAL_COVERAGE_ERROR_CODE))
            .map(|_| TERMINAL_COVERAGE_ERROR_CODE)
            .or_else(|| self.failure_outcome.as_ref().map(|outcome| outcome.code))
    }

    fn failure_reason(&self) -> Option<&'static str> {
        if self.failure_code() == Some(TERMINAL_COVERAGE_ERROR_CODE) {
            return Some("provider_terminal_coverage_unavailable");
        }
        self.failure_outcome.as_ref().map(|outcome| outcome.class)
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

    fn default_logical_phase(&self) -> &'static str {
        match self.state {
            SourceBackedRefreshState::Published | SourceBackedRefreshState::Failed => "terminal",
            SourceBackedRefreshState::Running => "direct",
            SourceBackedRefreshState::AdmissionPending | SourceBackedRefreshState::Queued => {
                "waiting"
            }
        }
    }

    fn physical_attempt_id(&self) -> &str {
        self.physical_attempt_id
            .as_deref()
            .unwrap_or(self.request_id.as_str())
    }

    fn structured_outcome_json(&self) -> Option<Value> {
        if let Some(receipt) = self.receipt.as_ref() {
            let code = receipt.terminal_outcome();
            let (retryable_routes, blocked_routes) = receipt.route_retry_dispositions();
            let retryable = !retryable_routes.is_empty();
            let affected_routes = receipt
                .route_results
                .iter()
                .filter(|result| {
                    result.outcome.is_failure()
                        || result.source_failure_total != 0
                        || result.rejected_record_total != 0
                })
                .map(|result| result.route_identity.as_str())
                .collect::<Vec<_>>();
            return Some(compact_json(json!({
                "code": code,
                "class": if retryable {
                    "completed_with_retryable_failures"
                } else if code == "completed" {
                    "completed"
                } else {
                    "completed_with_diagnostics"
                },
                "retryable": retryable,
                "affected_routes": affected_routes,
                "retryable_routes": retryable_routes
                    .iter()
                    .map(SourceRouteIdentity::as_str)
                    .collect::<Vec<_>>(),
                "blocked_routes": blocked_routes
                    .iter()
                    .map(SourceRouteIdentity::as_str)
                    .collect::<Vec<_>>(),
                "physical_attempt_id": self.physical_attempt_id(),
                "retained_generation": (code != "completed" || !receipt.generation_changed)
                    .then_some(receipt.published_generation.as_str()),
                "published_generation": receipt.published_generation,
                "retry_advice": retryable.then_some("retry_affected_routes"),
            })));
        }
        self.failure_outcome.as_ref().map(|outcome| {
            outcome.to_json(
                self.physical_attempt_id(),
                self.published_generation.as_deref(),
                self.last_error.as_deref(),
            )
        })
    }

    fn apply_base_read_fields(&self, mut value: Value) -> Value {
        let Some(fields) = value.as_object_mut() else {
            return value;
        };
        fields.insert("logical_request_id".to_owned(), json!(self.request_id));
        fields.insert(
            "logical_phase".to_owned(),
            json!(self.default_logical_phase()),
        );
        fields.insert(
            "physical_attempt_id".to_owned(),
            json!(self.physical_attempt_id()),
        );
        fields.insert(
            "physical_attempt_state".to_owned(),
            json!(self.state.as_str()),
        );
        fields.insert(
            "progress_owner_request_id".to_owned(),
            json!(self.request_id),
        );
        fields.insert(
            "progress_owner_attempt_state".to_owned(),
            json!(self.state.as_str()),
        );
        if let Some(outcome) = self.structured_outcome_json() {
            fields.insert("structured_outcome".to_owned(), outcome);
        }
        value
    }

    pub(super) fn to_json(&self) -> Value {
        let publication_receipt = self.publication_receipt.as_ref().or(self.receipt.as_ref());
        self.apply_base_read_fields(compact_json(json!({
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
            "fresh_after_admitted_snapshot": self.fresh_after_admitted_snapshot,
            "request_fingerprint": self.request_fingerprint,
            "admission_acknowledgement": self.admission_durability_indeterminate
                .then_some("retained_after_durability_error"),
            "admission_durability": self.admission_durability_indeterminate
                .then_some("replacement_visible_or_indeterminate"),
            "disconnect_policy": "retain_after_durable_admission",
            "coalesced_into_request_id": self.coalesced_into_request_id,
            "coalesced_logical_demands": self.coalesced_logical_demands,
            "generation_changed": self.request_generation_changed(),
            "receipt": publication_receipt.map(SourceBackedRefreshReceipt::to_json),
            "request_outcome": self.request_outcome_receipt()
                .map(SourceBackedRefreshReceipt::to_json),
            "outcome": self.receipt.as_ref().map(SourceBackedRefreshReceipt::terminal_outcome),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress
                .to_json_with_total_known(self.progress_total_sources_known),
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
        })))
    }

    pub(super) fn job_json(&self) -> Value {
        let status = match self.state {
            SourceBackedRefreshState::Published => "completed",
            SourceBackedRefreshState::Failed => "failed",
            SourceBackedRefreshState::AdmissionPending
            | SourceBackedRefreshState::Queued
            | SourceBackedRefreshState::Running => "running",
        };
        let publication_receipt = self.publication_receipt.as_ref().or(self.receipt.as_ref());
        self.apply_base_read_fields(compact_json(json!({
            "mode": "background",
            "owner": "daemon",
            "kind": "core_refresh",
            "status": status,
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "operation": self.operation.as_str(),
            "source_count": self.source_count(),
            "requested_at_ms": self.requested_at_ms,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "last_run_at_ms": self.started_at_ms.unwrap_or(self.requested_at_ms),
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "refresh_scope": refresh_scope_json(&self.refresh_scope),
            "requested_explicit_source_catalog": self.receipt.is_none().then(|| {
                self.requested_explicit_source_catalog
                    .as_ref()
                    .map(ExplicitSourceCatalogAuthority::to_json)
            }).flatten(),
            "fresh_after_admitted_snapshot": self.fresh_after_admitted_snapshot,
            "request_fingerprint": self.request_fingerprint,
            "admission_acknowledgement": self.admission_durability_indeterminate
                .then_some("retained_after_durability_error"),
            "admission_durability": self.admission_durability_indeterminate
                .then_some("replacement_visible_or_indeterminate"),
            "disconnect_policy": "retain_after_durable_admission",
            "coalesced_into_request_id": self.coalesced_into_request_id,
            "coalesced_logical_demands": self.coalesced_logical_demands,
            "generation_changed": self.request_generation_changed(),
            "receipt": publication_receipt.map(SourceBackedRefreshReceipt::to_json),
            "request_outcome": self.request_outcome_receipt()
                .map(SourceBackedRefreshReceipt::to_json),
            "outcome": self.receipt.as_ref().map(SourceBackedRefreshReceipt::terminal_outcome),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress
                .to_json_with_total_known(self.progress_total_sources_known),
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
        })))
    }

    fn timings_json(&self) -> Option<Value> {
        self.timings.map(|timings| {
            let mut timings = timings.to_json();
            timings["publication_probe"] = json!(self.publication_probe_us);
            timings
        })
    }
}

pub(super) fn projected_status_json(
    state: &CoreRefreshEngineState,
    request_id: &str,
) -> Option<Value> {
    let attempt = find_attempt(state, request_id)?;
    Some(apply_read_projection(
        state,
        attempt,
        attempt.to_json(),
        false,
    ))
}

pub(super) fn projected_job_json(
    state: &CoreRefreshEngineState,
    request_id: &str,
) -> Option<Value> {
    let attempt = find_attempt(state, request_id)?;
    Some(apply_read_projection(
        state,
        attempt,
        attempt.job_json(),
        true,
    ))
}

fn apply_read_projection(
    state: &CoreRefreshEngineState,
    logical: &SourceBackedRefreshAttempt,
    mut value: Value,
    job: bool,
) -> Value {
    let continuation = state.manual_all_continuations.get(&logical.request_id);
    let logical_phase = if !logical.state.is_active() {
        "terminal"
    } else if state
        .admission_resolutions_in_flight
        .contains(&logical.request_id)
    {
        "coverage_check"
    } else if let Some(continuation) = continuation {
        if !continuation.predecessor_finished {
            "attached"
        } else if continuation.is_fully_covered() {
            "coverage_check"
        } else if logical.state == SourceBackedRefreshState::Running {
            "exact_successor"
        } else {
            "waiting"
        }
    } else {
        logical.default_logical_phase()
    };

    let progress_owner = continuation
        .filter(|continuation| {
            logical.state.is_active()
                && (!continuation.predecessor_finished || continuation.is_fully_covered())
        })
        .and_then(|continuation| find_attempt(state, &continuation.predecessor_request_id))
        .unwrap_or(logical);
    let physical_attempt_id = if continuation.is_some_and(|continuation| {
        logical.state.is_active()
            && continuation.predecessor_finished
            && !continuation.is_fully_covered()
    }) {
        logical.request_id.as_str()
    } else {
        logical.physical_attempt_id.as_deref().unwrap_or_else(|| {
            continuation
                .map(|continuation| continuation.predecessor_request_id.as_str())
                .unwrap_or(logical.request_id.as_str())
        })
    };
    let physical_state = find_attempt(state, physical_attempt_id)
        .map(|attempt| attempt.state)
        .unwrap_or(logical.state);

    let Some(fields) = value.as_object_mut() else {
        return value;
    };
    fields.insert("logical_phase".to_owned(), json!(logical_phase));
    fields.insert("physical_attempt_id".to_owned(), json!(physical_attempt_id));
    fields.insert(
        "physical_attempt_state".to_owned(),
        json!(physical_state.as_str()),
    );
    fields.insert(
        "progress_owner_request_id".to_owned(),
        json!(progress_owner.request_id),
    );
    fields.insert(
        "progress_owner_attempt_state".to_owned(),
        json!(progress_owner.state.as_str()),
    );
    fields.insert(
        "progress".to_owned(),
        progress_owner
            .progress
            .to_json_with_total_known(progress_owner.progress_total_sources_known),
    );
    if job {
        fields.insert(
            "source_count".to_owned(),
            json!(progress_owner.source_count()),
        );
    }
    if let Some(outcome) = fields
        .get_mut("structured_outcome")
        .and_then(Value::as_object_mut)
    {
        outcome.insert("physical_attempt_id".to_owned(), json!(physical_attempt_id));
    }
    value
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
