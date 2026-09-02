use super::super::read_model::{
    SourceBackedAutomaticRetryCheckpoint, SourceBackedAutomaticRetryState,
    SourceBackedRefreshFailureType,
};
use super::*;

mod automatic_retry;
use automatic_retry::durable_build_rearmed_automatic_retry_routes;
pub(in crate::engine) use automatic_retry::recover_automatic_retry_checkpoints;

impl CoreRefreshEngine {
    pub fn recover(&self, data_root: &Path) -> Result<bool> {
        self.recover_interrupted_publication(data_root)
    }

    /// Restores exact durable terminals, or queues one bounded replay.
    pub fn recover_interrupted_publication(&self, data_root: &Path) -> Result<bool> {
        prepare_generation_control_state(data_root)?;
        let Some(job) = self.journal.load(data_root)? else {
            return Ok(false);
        };
        let build_rearmed_routes = durable_build_rearmed_automatic_retry_routes(&job)?;
        let verified = open_generation_for_request_recovery(data_root)?;
        let active_generation = verified
            .as_ref()
            .map(|verified| verified.generation_id().to_owned());
        let mut queued_successors = recover_queued_successors(&job)?;
        for successor in &mut queued_successors {
            require_scoped_rehydration(successor)?;
        }
        let request_state = job
            .get("request_state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");

        if request_state == "published" {
            let attempt = recover_published_attempt(&job)?;
            let receipt = attempt
                .receipt
                .as_ref()
                .expect("recovered published request has a terminal receipt")
                .clone();
            let terminal = verified
                .as_ref()
                .filter(|index| index.generation_id() == receipt.published_generation)
                .and_then(|index| {
                    published_refresh_receipt_for_index(&job, index)
                        .ok()
                        .filter(|verified_receipt| *verified_receipt == receipt)
                        .map(|_| Arc::clone(index))
                })
                .map(|index| CoreRefreshTerminalSuccess::bind(receipt, index))
                .transpose()?;
            let has_successors = !queued_successors.is_empty();
            self.install_published_recovery(
                attempt,
                terminal,
                queued_successors,
                active_generation,
                &build_rearmed_routes,
            )?;
            return Ok(has_successors);
        }

        let interrupted_running = request_state == "running";

        if request_state == "failed" {
            let terminal_progress_needs_normalization = job
                .get("progress")
                .and_then(Value::as_object)
                .is_some_and(|progress| progress.contains_key("current_source_progress"));
            let failed = recover_failed_attempt(&job)?;
            let durable_blocked_routes = job
                .get("structured_outcome")
                .and_then(Value::as_object)
                .map(|fields| recover_outcome_routes(fields, "blocked_routes"))
                .transpose()?
                .unwrap_or_default();
            let recovery_rearmed_routes = failed
                .failure_outcome
                .as_ref()
                .map(|outcome| {
                    durable_blocked_routes
                        .difference(&outcome.blocked_routes)
                        .cloned()
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            let failed_request_id = failed.request_id.clone();
            let failed_intent = failed.intent.clone();
            let failed_reconciliation_demand = failed.reconciliation_demand;
            let failure_route_dispositions = failed.failure_outcome.as_ref().map(|outcome| {
                (
                    outcome.retryable_routes.clone(),
                    outcome.blocked_routes.clone(),
                )
            });
            {
                let mut state = self.lock_state();
                state.automatic_retry_checkpoints = failed.automatic_retry_checkpoints.clone();
                state.attempts.push_back(failed);
                install_recovered_successors(&mut state, queued_successors)?;
                state.current_published_generation = active_generation;
                if let Some((retryable_routes, blocked_routes)) =
                    failure_route_dispositions.as_ref()
                {
                    Self::restore_route_dispositions_locked(
                        &mut state,
                        retryable_routes,
                        blocked_routes,
                        Some(&failed_intent),
                    );
                    if failed_reconciliation_demand == SourceBackedReconciliationDemand::Exhaustive
                    {
                        // Crash recovery restores admission's exhaustive obligation.
                        state
                            .routes_requiring_exhaustive_reconciliation
                            .extend(retryable_routes.iter().cloned());
                    }
                }
                Self::seed_rearmed_automatic_retry_routes_locked(&mut state, &build_rearmed_routes);
                trim_terminal_attempt_history(&mut state);
            }
            let has_successors = {
                let state = self.lock_state();
                state.active_request_id.is_some() || !state.pending_request_ids.is_empty()
            };
            if has_successors
                || terminal_progress_needs_normalization
                || !recovery_rearmed_routes.is_empty()
            {
                self.persist_job_status(data_root, &failed_request_id)?;
            }
            return Ok(has_successors);
        }

        if !matches!(request_state, "admission_pending" | "queued" | "running") {
            if let Some(verified) = verified {
                self.lock_state().current_published_generation =
                    Some(verified.generation_id().to_owned());
            }
            return Ok(false);
        }

        let recovered_previous_generation = active_generation.clone();
        let mut root = recover_queued_root(&job, recovered_previous_generation)?;
        if interrupted_running {
            root.reconciliation_demand = SourceBackedReconciliationDemand::Exhaustive;
        }
        require_scoped_rehydration(&mut root)?;
        let request_id = root.request_id.clone();
        let automatic_retry_checkpoints = root.automatic_retry_checkpoints.clone();
        {
            let mut state = self.lock_state();
            if state.active_request_id.is_some() || !state.pending_request_ids.is_empty() {
                bail!("interrupted source refresh recovery conflicts with an active queue");
            }
            state.active_request_id = Some(request_id.clone());
            state.automatic_retry_checkpoints = automatic_retry_checkpoints;
            state.attempts.push_back(root);
            install_recovered_successors(&mut state, queued_successors)?;
            state.current_published_generation = active_generation;
            Self::seed_rearmed_automatic_retry_routes_locked(&mut state, &build_rearmed_routes);
        }
        self.persist_job_status(data_root, &request_id)?;
        Ok(true)
    }

    fn install_published_recovery(
        &self,
        attempt: SourceBackedRefreshAttempt,
        terminal: Option<CoreRefreshTerminalSuccess>,
        queued_successors: Vec<SourceBackedRefreshAttempt>,
        active_generation: Option<String>,
        build_rearmed_routes: &BTreeSet<SourceRouteIdentity>,
    ) -> Result<()> {
        let route_dispositions = attempt
            .receipt
            .as_ref()
            .map(SourceBackedRefreshReceipt::route_retry_dispositions)
            .unwrap_or_default();
        let retry_intent = attempt.intent.clone();
        let mut state = self.lock_state();
        if let Some(terminal) = terminal {
            terminal.install(&mut state);
        }
        state.automatic_retry_checkpoints = attempt.automatic_retry_checkpoints.clone();
        state.attempts.push_back(attempt);
        install_recovered_successors(&mut state, queued_successors)?;
        state.current_published_generation = active_generation;
        Self::restore_route_dispositions_locked(
            &mut state,
            &route_dispositions.0,
            &route_dispositions.1,
            Some(&retry_intent),
        );
        Self::seed_rearmed_automatic_retry_routes_locked(&mut state, build_rearmed_routes);
        trim_terminal_attempt_history(&mut state);
        Ok(())
    }
}

fn open_generation_for_request_recovery(data_root: &Path) -> Result<Option<Arc<VerifiedIndex>>> {
    let index_root = source_backed_index_root(data_root);
    if !index_root.is_dir() {
        return Ok(None);
    }
    match open_verified_index(&index_root) {
        Ok(index) => Ok(Some(Arc::new(index))),
        Err(IndexError::MissingActiveGenerationPointer) => Ok(None),
        Err(error) if generation_incompatibility_requires_recovery_rebuild(&error) => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("open verified Core index {}", index_root.display()))
        }
    }
}

fn require_scoped_rehydration(attempt: &mut SourceBackedRefreshAttempt) -> Result<()> {
    if matches!(
        attempt.intent,
        RefreshIntent::SelectedImport(
            RefreshSelection::Provider(_) | RefreshSelection::ExactSource(_)
        )
    ) && attempt.state == SourceBackedRefreshState::Queued
        && !matches!(attempt.refresh_scope, SourceBackedRefreshScope::Exact(_))
    {
        bail!("durable resolved scoped source refresh has no exact physical scope");
    }
    attempt.admitted_authority = None;
    attempt.state = SourceBackedRefreshState::AdmissionPending;
    attempt.progress.phase = "admission_pending".to_owned();
    Ok(())
}

fn recover_published_attempt(job: &Value) -> Result<SourceBackedRefreshAttempt> {
    require_terminal_state(job, "published", "completed")?;
    let receipt = published_refresh_receipt_for_recovery(job)
        .context("recover durable terminal source refresh receipt")?;
    validate_terminal_receipt_fields(job, &receipt)?;
    let mut attempt = recover_terminal_attempt(job, SourceBackedRefreshState::Published)?;
    attempt.previous_generation = receipt.previous_generation.clone();
    attempt.published_generation = Some(receipt.published_generation.clone());
    attempt.receipt = Some(receipt);
    attempt.failure_type = None;
    attempt.failure_outcome = None;
    attempt.last_error = None;
    Ok(attempt)
}

fn validate_terminal_receipt_fields(
    job: &Value,
    receipt: &SourceBackedRefreshReceipt,
) -> Result<()> {
    if optional_generation(job.get("previous_generation"))? != receipt.previous_generation
        || required_generation(
            job.get("published_generation"),
            "durable terminal published generation",
        )? != receipt.published_generation
        || job.get("generation_changed").and_then(Value::as_bool)
            != Some(receipt.generation_changed)
        || job.get("outcome").and_then(Value::as_str) != Some(receipt.terminal_outcome())
    {
        bail!("durable source refresh response does not match its terminal receipt");
    }
    Ok(())
}

fn recover_failed_attempt(job: &Value) -> Result<SourceBackedRefreshAttempt> {
    require_terminal_state(job, "failed", "failed")?;
    if job.get("receipt").is_some() {
        bail!("durable failed source refresh unexpectedly contains a terminal receipt");
    }
    let attempt = recover_terminal_attempt(job, SourceBackedRefreshState::Failed)?;
    if attempt.last_error.as_deref().is_none_or(str::is_empty) {
        bail!("durable failed source refresh has no exact failure response");
    }
    Ok(attempt)
}

fn recover_terminal_attempt(
    job: &Value,
    state: SourceBackedRefreshState,
) -> Result<SourceBackedRefreshAttempt> {
    let operation = SourceBackedRefreshOperation::from_request_json(job)?;
    let daemon_mode = job
        .get("daemon_mode")
        .and_then(Value::as_str)
        .and_then(canonical_daemon_mode)
        .ok_or_else(|| anyhow!("durable terminal source refresh has invalid daemon mode"))?;
    let trigger = recover_static_field(
        job,
        "trigger",
        &["setup", "search", "periodic", "import", "recovery"],
    )?;
    let trigger_provenance = recover_static_field(
        job,
        "trigger_provenance",
        &[
            "manual",
            "autostart",
            "setup_command",
            "import_command",
            "automatic_provider",
            "daemon_scheduler",
            "explicit_source_catalog",
            "commit_payload",
        ],
    )?;
    let intent = recover_refresh_intent(job, operation, true, false)
        .context("recover durable terminal source refresh intent")?;
    let previous_generation = optional_generation(job.get("previous_generation"))?;
    let mut attempt = new_refresh_attempt(
        previous_generation,
        SourceRefreshRuntimeMetadata {
            operation,
            daemon_mode,
            trigger,
            trigger_provenance,
        },
        intent,
        refresh_scope_from_json(job.get("refresh_scope"))?,
    );
    attempt.request_id =
        required_nonempty_string(job, "request_id", "terminal source refresh")?.to_owned();
    let _legacy_physical_attempt_id = optional_string(job, "physical_attempt_id")?;
    attempt.reconciliation_demand = recover_reconciliation_demand(job, operation)?;
    attempt.state = state;
    attempt.requested_at_ms = optional_i64(job, "requested_at_ms")?
        .or(optional_i64(job, "last_run_at_ms")?)
        .ok_or_else(|| anyhow!("durable terminal source refresh has no request timestamp"))?;
    attempt.started_at_ms = optional_i64(job, "started_at_ms")?;
    attempt.finished_at_ms = optional_i64(job, "finished_at_ms")?;
    attempt.published_generation = optional_generation(job.get("published_generation"))?;
    attempt.request_fingerprint = optional_string(job, "request_fingerprint")?;
    if attempt
        .request_fingerprint
        .as_deref()
        .is_some_and(|fingerprint| !is_sha256_identity(fingerprint))
    {
        bail!("durable terminal source refresh request fingerprint is invalid");
    }
    attempt.admission_durability_indeterminate =
        recover_admission_durability(job, "durable terminal source refresh")?;
    let _legacy_coalesced_into_request_id = optional_string(job, "coalesced_into_request_id")?;
    let _legacy_coalesced_logical_demands = optional_u64(job, "coalesced_logical_demands")?;
    attempt.coalesced_requests = optional_u64(job, "coalesced_requests")?.unwrap_or_default();
    attempt.progress = SourceBackedRefreshProgress::from_status_json(job)?;
    // Legacy terminal snapshots may retain stale active-source detail.
    attempt.progress.current_source_progress = None;
    attempt.progress_total_sources_known = status_progress_total_sources_known(job);
    attempt.scanned_routes = optional_usize(job, "scanned_routes")?;
    attempt.unsupported_routes = optional_usize(job, "unsupported_routes")?;
    attempt.request_source_count = optional_usize(job, "source_count")?;
    attempt.certified_source_count = optional_usize(job, "certified_source_count")?;
    attempt.certified_source_bytes = optional_u64(job, "certified_source_bytes")?;
    let (timings, publication_probe_us) = recover_timings(job)?;
    attempt.timings = timings;
    attempt.publication_probe_us = publication_probe_us;
    attempt.failure_type = recover_optional_failure_type(job)?;
    attempt.failure_outcome = if state == SourceBackedRefreshState::Failed {
        recover_failure_outcome(job, &attempt.refresh_scope, attempt.failure_type)?
    } else {
        None
    };
    attempt.last_error = optional_string(job, "last_error")?;
    attempt.automatic_retry_checkpoints = recover_automatic_retry_checkpoints(job)?;
    if let Some(outcome) = attempt
        .failure_outcome
        .as_ref()
        .filter(|outcome| outcome.is_automatic_retry_eligible())
    {
        for (route, checkpoint) in &attempt.automatic_retry_checkpoints {
            if !outcome.affected_routes.contains(route) {
                continue;
            }
            let disposition_matches = if checkpoint.is_paused() {
                outcome.blocked_routes.contains(route)
            } else {
                outcome.retryable_routes.contains(route)
            };
            if !disposition_matches {
                bail!("durable source refresh automatic retry disposition is inconsistent");
            }
        }
    }
    let checkpointless_pauses = attempt
        .failure_outcome
        .as_ref()
        .filter(|outcome| outcome.is_automatic_retry_eligible())
        .map(|outcome| {
            outcome
                .blocked_routes
                .iter()
                .filter(|route| {
                    !attempt
                        .automatic_retry_checkpoints
                        .get(*route)
                        .is_some_and(SourceBackedAutomaticRetryCheckpoint::is_paused)
                })
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    if let Some(outcome) = attempt.failure_outcome.as_mut() {
        outcome.rearm_automatic_retry_routes(&checkpointless_pauses);
    }
    rearm_build_changed_automatic_retry_checkpoints(&mut attempt);
    // Estimator state is deliberately non-durable.
    attempt.whole_run_eta.clear();
    Ok(attempt)
}

fn recover_failure_outcome(
    job: &Value,
    scope: &SourceBackedRefreshScope,
    legacy_failure_type: Option<SourceBackedRefreshFailureType>,
) -> Result<Option<SourceBackedRefreshFailureOutcome>> {
    let Some(value) = job.get("structured_outcome") else {
        return Ok(
            legacy_failure_type.map(|failure_type| legacy_failure_outcome(failure_type, scope))
        );
    };
    let fields = value
        .as_object()
        .ok_or_else(|| anyhow!("durable terminal source refresh has invalid structured outcome"))?;
    let code: RefreshOutcomeCode = required_outcome_text(fields, "code")?.parse()?;
    if !code.is_failure() {
        bail!("durable terminal source refresh outcome has invalid `code`");
    }
    let class: RefreshOutcomeClass = required_outcome_text(fields, "class")?.parse()?;
    if matches!(
        class,
        RefreshOutcomeClass::Completed
            | RefreshOutcomeClass::CompletedWithRetryableFailures
            | RefreshOutcomeClass::CompletedWithDiagnostics
    ) {
        bail!("durable terminal source refresh outcome has invalid `class`");
    }
    let retryable = fields
        .get("retryable")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            anyhow!("durable terminal source refresh outcome has invalid retryability")
        })?;
    let affected_routes = recover_outcome_routes(fields, "affected_routes")?;
    if let SourceBackedRefreshScope::Exact(exact_routes) = scope {
        let out_of_scope = affected_routes
            .difference(exact_routes)
            .cloned()
            .collect::<BTreeSet<_>>();
        if !out_of_scope.is_empty() {
            bail!(
                "durable terminal source refresh outcome exceeds its exact scope: {out_of_scope:?}"
            );
        }
    }
    let retry_advice = match fields.get("retry_advice") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.parse::<RefreshRetryAdvice>().map_err(|_| {
            anyhow!("durable terminal source refresh outcome has invalid retry advice")
        })?),
        Some(_) => bail!("durable terminal source refresh outcome has invalid retry advice"),
    };
    let retryable_routes = fields
        .get("retryable_routes")
        .map(|_| recover_outcome_routes(fields, "retryable_routes"))
        .transpose()?;
    let blocked_routes = fields
        .get("blocked_routes")
        .map(|_| recover_outcome_routes(fields, "blocked_routes"))
        .transpose()?;
    if code == RefreshOutcomeCode::SourceUnclaimed
        && (class != RefreshOutcomeClass::Coverage
            || blocked_routes.as_ref().is_none_or(BTreeSet::is_empty)
            || retry_advice
                != Some(if retryable {
                    RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked
                } else {
                    RefreshRetryAdvice::InspectSources
                }))
    {
        bail!("durable terminal source refresh source-unclaimed outcome is inconsistent");
    }
    match (retryable_routes, blocked_routes) {
        (Some(retryable_routes), Some(blocked_routes)) => {
            if !retryable_routes.is_disjoint(&blocked_routes)
                || retryable_routes
                    .union(&blocked_routes)
                    .ne(affected_routes.iter())
                || (!affected_routes.is_empty() && retryable == retryable_routes.is_empty())
            {
                bail!("durable terminal source refresh outcome has inconsistent route disposition");
            }
            Ok(Some(
                SourceBackedRefreshFailureOutcome::with_route_dispositions(
                    code,
                    class,
                    retryable,
                    retryable_routes,
                    blocked_routes,
                    retry_advice,
                ),
            ))
        }
        (None, None) => Ok(Some(SourceBackedRefreshFailureOutcome::new(
            code,
            class,
            retryable,
            affected_routes,
            retry_advice,
        ))),
        _ => bail!("durable terminal source refresh outcome has incomplete route disposition"),
    }
}

fn recover_outcome_routes(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<BTreeSet<SourceRouteIdentity>> {
    let routes = fields
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("durable terminal source refresh outcome has invalid `{field}`"))?;
    if routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
        bail!("durable terminal source refresh outcome exceeds its route bound");
    }
    let parsed = routes
        .iter()
        .map(|route| {
            route
                .as_str()
                .ok_or_else(|| anyhow!("durable terminal source refresh outcome route is invalid"))
                .and_then(|route| {
                    SourceRouteIdentity::from_sha256(route.to_owned()).map_err(Into::into)
                })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if parsed.len() != routes.len() {
        bail!("durable terminal source refresh outcome has duplicate `{field}`");
    }
    Ok(parsed)
}

fn required_outcome_text<'a>(
    fields: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a str> {
    fields
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("durable terminal source refresh outcome has invalid `{field}`"))
}

fn legacy_failure_outcome(
    failure_type: SourceBackedRefreshFailureType,
    scope: &SourceBackedRefreshScope,
) -> SourceBackedRefreshFailureOutcome {
    let (class, retryable, retry_advice) = match failure_type {
        SourceBackedRefreshFailureType::UnsupportedSchema => (
            RefreshOutcomeClass::Incompatible,
            false,
            RefreshRetryAdvice::UpgradeOrReconfigure,
        ),
        SourceBackedRefreshFailureType::MalformedSource => (
            RefreshOutcomeClass::Unreadable,
            false,
            RefreshRetryAdvice::InspectSources,
        ),
        SourceBackedRefreshFailureType::SourceUnavailable => (
            RefreshOutcomeClass::Unavailable,
            true,
            RefreshRetryAdvice::RetryAffectedRoutes,
        ),
        SourceBackedRefreshFailureType::SourceChanged => (
            RefreshOutcomeClass::SourceChanged,
            true,
            RefreshRetryAdvice::RetryAffectedRoutes,
        ),
        SourceBackedRefreshFailureType::SourceFailures => (
            RefreshOutcomeClass::Mixed,
            true,
            RefreshRetryAdvice::RetryAffectedRoutes,
        ),
        SourceBackedRefreshFailureType::AllProviderTerminalCoverageUnavailable => (
            RefreshOutcomeClass::Coverage,
            true,
            RefreshRetryAdvice::RetryRequest,
        ),
    };
    let affected_routes = match scope {
        SourceBackedRefreshScope::All => BTreeSet::new(),
        SourceBackedRefreshScope::Exact(routes) => routes.clone(),
    };
    SourceBackedRefreshFailureOutcome::new(
        failure_type.outcome_code(),
        class,
        retryable,
        affected_routes,
        Some(retry_advice),
    )
}

fn require_terminal_state(job: &Value, request_state: &str, status: &str) -> Result<()> {
    if job.get("request_state").and_then(Value::as_str) != Some(request_state)
        || job.get("status").and_then(Value::as_str) != Some(status)
    {
        bail!("durable source refresh terminal state is inconsistent");
    }
    Ok(())
}

fn required_nonempty_string<'a>(job: &'a Value, field: &str, label: &str) -> Result<&'a str> {
    job.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("durable {label} has no `{field}`"))
}

fn recover_static_field(
    job: &Value,
    field: &str,
    accepted: &[&'static str],
) -> Result<&'static str> {
    let value = required_nonempty_string(job, field, "terminal source refresh")?;
    accepted
        .iter()
        .copied()
        .find(|accepted| *accepted == value)
        .ok_or_else(|| anyhow!("durable terminal source refresh has invalid `{field}`"))
}

fn optional_i64(job: &Value, field: &str) -> Result<Option<i64>> {
    match job.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| anyhow!("durable terminal source refresh has invalid `{field}`")),
    }
}

fn optional_u64(job: &Value, field: &str) -> Result<Option<u64>> {
    match job.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| anyhow!("durable terminal source refresh has invalid `{field}`")),
    }
}

fn optional_usize(job: &Value, field: &str) -> Result<Option<usize>> {
    optional_u64(job, field)?
        .map(|value| {
            usize::try_from(value)
                .map_err(|_| anyhow!("durable terminal source refresh has invalid `{field}`"))
        })
        .transpose()
}

fn optional_string(job: &Value, field: &str) -> Result<Option<String>> {
    match job.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => bail!("durable terminal source refresh has invalid `{field}`"),
    }
}

fn recover_optional_failure_type(job: &Value) -> Result<Option<SourceBackedRefreshFailureType>> {
    match job.get("failure_type") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => value
            .parse::<SourceBackedRefreshFailureType>()
            .map(Some)
            .map_err(|_| anyhow!("durable terminal source refresh has invalid failure type")),
        Some(_) => bail!("durable terminal source refresh has invalid failure type"),
    }
}

fn recover_timings(job: &Value) -> Result<(Option<SourceBackedRefreshTimings>, u64)> {
    let Some(timings) = job.get("timings_us") else {
        return Ok((None, 0));
    };
    if timings.is_null() {
        return Ok((None, 0));
    }
    let timings = timings
        .as_object()
        .ok_or_else(|| anyhow!("durable terminal source refresh has invalid timings"))?;
    let required = |field| {
        timings
            .get(field)
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("durable terminal source refresh has invalid `{field}` timing"))
    };
    Ok((
        Some(SourceBackedRefreshTimings {
            discovery_us: required("discovery")?,
            scan_stage_us: required("scan_stage")?,
            commit_us: required("commit")?,
        }),
        required("publication_probe")?,
    ))
}

#[cfg(test)]
#[path = "recovery/failure_type_recovery_tests.rs"]
mod failure_type_recovery_tests;

#[cfg(test)]
#[path = "recovery/failure_outcome_scope_tests.rs"]
mod failure_outcome_scope_tests;
