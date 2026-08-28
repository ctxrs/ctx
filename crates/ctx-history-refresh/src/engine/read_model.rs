use super::*;
use sha2::{Digest as _, Sha256};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceBackedAutomaticRetryState {
    Confirming,
    Paused,
}

impl SourceBackedAutomaticRetryState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Confirming => "confirming",
            Self::Paused => "paused",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct SourceBackedAutomaticRetryCheckpoint {
    pub(super) state: SourceBackedAutomaticRetryState,
    pub(super) matching_failures: u8,
    pub(super) source_observation: String,
    pub(super) failure_fingerprint: String,
    pub(super) build_version: String,
}

impl SourceBackedAutomaticRetryCheckpoint {
    pub(super) fn confirming(
        outcome: &SourceBackedRefreshFailureOutcome,
        route: &SourceRouteIdentity,
        source_observation: &str,
        terminal_error: &str,
    ) -> Self {
        Self {
            state: SourceBackedAutomaticRetryState::Confirming,
            matching_failures: 1,
            source_observation: source_observation.to_owned(),
            failure_fingerprint: automatic_retry_failure_fingerprint(
                outcome,
                route,
                source_observation,
                SOURCE_REFRESH_BUILD_VERSION,
                terminal_error,
            ),
            build_version: SOURCE_REFRESH_BUILD_VERSION.to_owned(),
        }
    }

    pub(super) fn matches(&self, candidate: &Self) -> bool {
        self.source_observation == candidate.source_observation
            && self.failure_fingerprint == candidate.failure_fingerprint
            && self.build_version == candidate.build_version
    }

    pub(super) fn pause(&mut self) {
        self.state = SourceBackedAutomaticRetryState::Paused;
        self.matching_failures = SOURCE_REFRESH_AUTOMATIC_RETRY_CONFIRMATION_LIMIT;
    }

    pub(super) fn is_paused(&self) -> bool {
        self.state == SourceBackedAutomaticRetryState::Paused
    }

    fn to_json(&self) -> Value {
        json!({
            "state": self.state.as_str(),
            "matching_failures": self.matching_failures,
            "source_observation": self.source_observation,
            "failure_fingerprint": self.failure_fingerprint,
            "build_version": self.build_version,
        })
    }
}

fn automatic_retry_failure_fingerprint(
    outcome: &SourceBackedRefreshFailureOutcome,
    route: &SourceRouteIdentity,
    source_observation: &str,
    build_version: &str,
    terminal_error: &str,
) -> String {
    let mut digest = Sha256::new();
    for (label, value) in [
        ("code", outcome.code.as_str().as_bytes()),
        ("class", outcome.class.as_str().as_bytes()),
        ("route", route.as_str().as_bytes()),
        ("source_observation", source_observation.as_bytes()),
        ("build_version", build_version.as_bytes()),
    ] {
        digest.update(label.as_bytes());
        digest.update([0]);
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value);
    }
    let summary = terminal_error
        .as_bytes()
        .get(
            ..terminal_error
                .len()
                .min(SOURCE_REFRESH_AUTOMATIC_RETRY_ERROR_SUMMARY_BYTES),
        )
        .unwrap_or_default();
    digest.update(b"terminal_error\0");
    digest.update((summary.len() as u64).to_le_bytes());
    digest.update(summary);
    format!("{:x}", digest.finalize())
}

fn automatic_retry_json(
    checkpoints: &BTreeMap<SourceRouteIdentity, SourceBackedAutomaticRetryCheckpoint>,
) -> Option<Value> {
    if checkpoints.is_empty() {
        return None;
    }
    let confirming = checkpoints
        .values()
        .any(|checkpoint| checkpoint.state == SourceBackedAutomaticRetryState::Confirming);
    let paused = checkpoints
        .values()
        .any(|checkpoint| checkpoint.state == SourceBackedAutomaticRetryState::Paused);
    let state = match (confirming, paused) {
        (true, true) => "mixed",
        (true, false) => "confirming",
        (false, true) => "paused",
        (false, false) => return None,
    };
    let routes = checkpoints
        .iter()
        .map(|(route, checkpoint)| (route.as_str().to_owned(), checkpoint.to_json()))
        .collect::<serde_json::Map<_, _>>();
    Some(json!({
        "state": state,
        "reason": if paused {
            "repeated_internal_failure"
        } else {
            "internal_failure_confirmation"
        },
        "confirmation_limit": SOURCE_REFRESH_AUTOMATIC_RETRY_CONFIRMATION_LIMIT,
        "routes": routes,
        "resume_on": ["source_change", "ctx_upgrade", "manual_import"],
    }))
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct SourceBackedRefreshFailureOutcome {
    pub(super) code: RefreshOutcomeCode,
    pub(super) class: RefreshOutcomeClass,
    pub(super) retryable: bool,
    pub(super) affected_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) retryable_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) blocked_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) retry_advice: Option<RefreshRetryAdvice>,
}

impl SourceBackedRefreshFailureOutcome {
    pub(super) fn is_automatic_retry_eligible(&self) -> bool {
        self.code == RefreshOutcomeCode::SourceRefreshFailed
            && self.class == RefreshOutcomeClass::Internal
    }

    pub(super) fn pause_automatic_retry_routes(&mut self, routes: &BTreeSet<SourceRouteIdentity>) {
        if !self.is_automatic_retry_eligible() {
            return;
        }
        let mut changed = false;
        for route in routes {
            if self.retryable_routes.remove(route) {
                self.blocked_routes.insert(route.clone());
                changed = true;
            }
        }
        if changed {
            self.refresh_automatic_retry_disposition();
        }
    }

    pub(super) fn rearm_automatic_retry_routes(&mut self, routes: &BTreeSet<SourceRouteIdentity>) {
        if !self.is_automatic_retry_eligible() {
            return;
        }
        let mut changed = false;
        for route in routes {
            if self.blocked_routes.remove(route) {
                self.retryable_routes.insert(route.clone());
                changed = true;
            }
        }
        if changed {
            self.refresh_automatic_retry_disposition();
        }
    }

    fn refresh_automatic_retry_disposition(&mut self) {
        self.retryable = !self.retryable_routes.is_empty();
        self.retry_advice = Some(if self.retryable {
            RefreshRetryAdvice::RetryAffectedRoutes
        } else {
            RefreshRetryAdvice::InspectSources
        });
    }

    pub(super) fn new(
        code: RefreshOutcomeCode,
        class: RefreshOutcomeClass,
        retryable: bool,
        affected_routes: BTreeSet<SourceRouteIdentity>,
        retry_advice: Option<RefreshRetryAdvice>,
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
        code: RefreshOutcomeCode,
        class: RefreshOutcomeClass,
        retryable: bool,
        retryable_routes: BTreeSet<SourceRouteIdentity>,
        blocked_routes: BTreeSet<SourceRouteIdentity>,
        retry_advice: Option<RefreshRetryAdvice>,
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
            "code": self.code.as_str(),
            "class": self.class.as_str(),
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
            "retry_advice": self.retry_advice.map(RefreshRetryAdvice::as_str),
            "detail": detail,
        }))
    }
}

/// Exact vocabulary of the durable legacy `failure_type` field.
///
/// Structured outcomes use the broader `RefreshOutcomeCode`; keeping this
/// field narrow makes values its writer cannot produce fail closed on recovery.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceBackedRefreshFailureType {
    UnsupportedSchema,
    MalformedSource,
    SourceUnavailable,
    SourceChanged,
    SourceFailures,
    AllProviderTerminalCoverageUnavailable,
}

impl SourceBackedRefreshFailureType {
    pub(super) const fn outcome_code(self) -> RefreshOutcomeCode {
        match self {
            Self::UnsupportedSchema => RefreshOutcomeCode::UnsupportedSchema,
            Self::MalformedSource => RefreshOutcomeCode::MalformedSource,
            Self::SourceUnavailable => RefreshOutcomeCode::SourceUnavailable,
            Self::SourceChanged => RefreshOutcomeCode::SourceChanged,
            Self::SourceFailures => RefreshOutcomeCode::SourceFailures,
            Self::AllProviderTerminalCoverageUnavailable => {
                RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable
            }
        }
    }

    pub(super) const fn as_str(self) -> &'static str {
        self.outcome_code().as_str()
    }
}

impl std::str::FromStr for SourceBackedRefreshFailureType {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.parse::<RefreshOutcomeCode>()? {
            RefreshOutcomeCode::UnsupportedSchema => Ok(Self::UnsupportedSchema),
            RefreshOutcomeCode::MalformedSource => Ok(Self::MalformedSource),
            RefreshOutcomeCode::SourceUnavailable => Ok(Self::SourceUnavailable),
            RefreshOutcomeCode::SourceChanged => Ok(Self::SourceChanged),
            RefreshOutcomeCode::SourceFailures => Ok(Self::SourceFailures),
            RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable => {
                Ok(Self::AllProviderTerminalCoverageUnavailable)
            }
            _ => bail!("unknown source-backed refresh failure type"),
        }
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
    /// The sole durable logical request authority.
    pub(super) intent: RefreshIntent,
    /// Durable admitted target. `All` records complete-catalog certification;
    /// `Exact` records a fail-closed selected/retry checkpoint. This value is
    /// never passed to physical execution.
    pub(super) refresh_scope: SourceBackedRefreshScope,
    pub(super) reconciliation_demand: SourceBackedReconciliationDemand,
    /// Attempt-local authority resolved from the logical intent. Durable state
    /// persists the intent and admitted target; recovery re-admits both through
    /// the same resolver before execution.
    pub(super) admitted_authority: Option<ctx_history_refresh_execution::AdmittedRefresh>,
    pub(super) request_fingerprint: Option<String>,
    pub(super) admission_durability_indeterminate: bool,
    pub(super) coalesced_requests: u64,
    pub(super) progress: SourceBackedRefreshProgress,
    /// Transient producer-owned scanner facts. This is deliberately excluded
    /// from the durable job representation and terminal receipts.
    pub(super) attempt_history_progress:
        Option<ctx_history_capture_model::SharedAttemptHistoryProgress>,
    pub(super) progress_total_sources_known: bool,
    pub(super) whole_run_eta: WholeRunEtaEstimator,
    pub(super) scanned_routes: Option<usize>,
    pub(super) unsupported_routes: Option<usize>,
    pub(super) request_source_count: Option<usize>,
    pub(super) certified_source_count: Option<usize>,
    pub(super) certified_source_bytes: Option<u64>,
    /// Request-scoped route/result/rejection facts. This is mutable daemon
    /// status, never publication authority.
    pub(super) receipt: Option<SourceBackedRefreshReceipt>,
    /// The sole publication receipt, decoded from Core CommitPayload metadata.
    pub(super) publication_receipt: Option<SourceBackedRefreshReceipt>,
    pub(super) route_observations: BTreeMap<SourceRouteIdentity, String>,
    pub(super) automatic_retry_checkpoints:
        BTreeMap<SourceRouteIdentity, SourceBackedAutomaticRetryCheckpoint>,
    pub(super) timings: Option<SourceBackedRefreshTimings>,
    pub(super) publication_probe_us: u64,
    pub(super) daemon_mode: String,
    pub(super) trigger: &'static str,
    pub(super) trigger_provenance: &'static str,
    pub(super) failure_type: Option<SourceBackedRefreshFailureType>,
    pub(super) failure_outcome: Option<SourceBackedRefreshFailureOutcome>,
    pub(super) last_error: Option<String>,
}

impl SourceBackedRefreshAttempt {
    pub(super) fn snapshot_attempt_history_progress(&mut self) {
        let Some(history) = &self.attempt_history_progress else {
            return;
        };
        let history = history.snapshot();
        self.progress.processed_sessions = self
            .progress
            .processed_sessions
            .max(history.processed_sessions);
        self.progress.processed_messages = self
            .progress
            .processed_messages
            .max(history.processed_messages);
        self.progress.processed_tool_calls = self
            .progress
            .processed_tool_calls
            .max(history.processed_tool_calls);
        self.progress.processed_bytes = self.progress.processed_bytes.max(history.processed_bytes);
    }

    fn live_progress(&self) -> SourceBackedRefreshProgress {
        let mut progress = self.progress.clone();
        if self.state == SourceBackedRefreshState::Running {
            if let Some(history) = &self.attempt_history_progress {
                let history = history.snapshot();
                progress.processed_sessions =
                    progress.processed_sessions.max(history.processed_sessions);
                progress.processed_messages =
                    progress.processed_messages.max(history.processed_messages);
                progress.processed_tool_calls = progress
                    .processed_tool_calls
                    .max(history.processed_tool_calls);
                progress.processed_bytes = progress.processed_bytes.max(history.processed_bytes);
            }
        }
        progress
    }

    pub(super) fn operation(&self) -> SourceBackedRefreshOperation {
        self.intent.operation()
    }

    pub(super) fn requested_explicit_source_catalog(
        &self,
    ) -> Option<&ExplicitSourceCatalogAuthority> {
        self.intent.explicit_source_authority()
    }

    fn source_count(&self) -> usize {
        self.request_source_count
            .or(self.scanned_routes)
            .unwrap_or(self.progress.total_sources)
    }

    fn failure_code(&self) -> Option<&'static str> {
        self.last_error
            .as_deref()
            .filter(|error| error.contains(TERMINAL_COVERAGE_ERROR_CODE))
            .map(|_| TERMINAL_COVERAGE_ERROR_CODE)
            .or_else(|| {
                self.failure_outcome
                    .as_ref()
                    .map(|outcome| outcome.code.as_str())
            })
    }

    fn failure_reason(&self) -> Option<&'static str> {
        if self.failure_code() == Some(TERMINAL_COVERAGE_ERROR_CODE) {
            return Some("provider_terminal_coverage_unavailable");
        }
        self.failure_outcome
            .as_ref()
            .map(|outcome| outcome.class.as_str())
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
        self.request_id.as_str()
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
        fields.insert(
            "reconciliation_demand".to_owned(),
            json!(self.reconciliation_demand.as_str()),
        );
        fields.insert("refresh_intent".to_owned(), self.intent.to_json());
        if let Some(outcome) = self.structured_outcome_json() {
            fields.insert("structured_outcome".to_owned(), outcome);
        }
        if let Some(automatic_retry) = automatic_retry_json(&self.automatic_retry_checkpoints) {
            fields.insert("automatic_retry".to_owned(), automatic_retry);
        }
        value
    }

    pub(super) fn to_json(&self) -> Value {
        let publication_receipt = self.publication_receipt.as_ref().or(self.receipt.as_ref());
        let progress = self.live_progress();
        self.apply_base_read_fields(compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "operation": self.operation().as_str(),
            "requested_at_ms": self.requested_at_ms,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "refresh_scope": refresh_scope_json(&self.refresh_scope),
            "requested_explicit_source_catalog": self.requested_explicit_source_catalog()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "request_fingerprint": self.request_fingerprint,
            "admission_acknowledgement": self.admission_durability_indeterminate
                .then_some("retained_after_durability_error"),
            "admission_durability": self.admission_durability_indeterminate
                .then_some("replacement_visible_or_indeterminate"),
            "disconnect_policy": "retain_after_durable_admission",
            "coalesced_into_request_id": None::<String>,
            "coalesced_logical_demands": 0,
            "generation_changed": self.request_generation_changed(),
            "receipt": publication_receipt.map(SourceBackedRefreshReceipt::to_json),
            "request_outcome": self.request_outcome_receipt()
                .map(SourceBackedRefreshReceipt::to_json),
            "outcome": self.receipt.as_ref().map(SourceBackedRefreshReceipt::terminal_outcome),
            "coalesced_requests": self.coalesced_requests,
            "progress": progress.to_json_with_total_known(
                self.progress_total_sources_known,
                self.whole_run_eta.estimated_remaining_millis(),
            ),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings_json(),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "failure_type": self.failure_type.map(SourceBackedRefreshFailureType::as_str),
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
            "operation": self.operation().as_str(),
            "source_count": self.source_count(),
            "requested_at_ms": self.requested_at_ms,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "last_run_at_ms": self.started_at_ms.unwrap_or(self.requested_at_ms),
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "refresh_scope": refresh_scope_json(&self.refresh_scope),
            "request_fingerprint": self.request_fingerprint,
            "admission_acknowledgement": self.admission_durability_indeterminate
                .then_some("retained_after_durability_error"),
            "admission_durability": self.admission_durability_indeterminate
                .then_some("replacement_visible_or_indeterminate"),
            "disconnect_policy": "retain_after_durable_admission",
            "coalesced_into_request_id": None::<String>,
            "coalesced_logical_demands": 0,
            "generation_changed": self.request_generation_changed(),
            "receipt": publication_receipt.map(SourceBackedRefreshReceipt::to_json),
            "request_outcome": self.request_outcome_receipt()
                .map(SourceBackedRefreshReceipt::to_json),
            "outcome": self.receipt.as_ref().map(SourceBackedRefreshReceipt::terminal_outcome),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json_with_total_known(
                self.progress_total_sources_known,
                self.whole_run_eta.estimated_remaining_millis(),
            ),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings_json(),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "failure_type": self.failure_type.map(SourceBackedRefreshFailureType::as_str),
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
    Some(apply_read_projection(attempt, attempt.to_json(), false))
}

pub(super) fn projected_job_json(
    state: &CoreRefreshEngineState,
    request_id: &str,
) -> Option<Value> {
    let attempt = find_attempt(state, request_id)?;
    Some(apply_read_projection(attempt, attempt.job_json(), true))
}

fn apply_read_projection(
    logical: &SourceBackedRefreshAttempt,
    mut value: Value,
    job: bool,
) -> Value {
    let logical_phase = logical.default_logical_phase();
    let progress_owner = logical;
    let physical_attempt_id = logical.request_id.as_str();
    let physical_state = logical.state;

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
    let progress = if job {
        progress_owner.progress.clone()
    } else {
        progress_owner.live_progress()
    };
    fields.insert(
        "progress".to_owned(),
        progress.to_json_with_total_known(
            progress_owner.progress_total_sources_known,
            progress_owner.whole_run_eta.estimated_remaining_millis(),
        ),
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
