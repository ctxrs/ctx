use super::*;

impl CoreRefreshEngine {
    pub fn recover(&self, data_root: &Path) -> Result<bool> {
        self.recover_interrupted_publication(data_root)
    }

    /// Restores exact durable terminal responses, or queues one bounded replay
    /// when Core may have committed past the last terminal job snapshot.
    pub fn recover_interrupted_publication(&self, data_root: &Path) -> Result<bool> {
        let Some(job) = self.journal.load(data_root)? else {
            return Ok(false);
        };
        let verified = open_published_generation(data_root, self.journal.as_ref())?.map(Arc::new);
        let active_generation = verified
            .as_ref()
            .map(|verified| verified.generation_id().to_owned());
        let queued_successors = recover_queued_successors(&job)?;
        let recovered_continuations = recover_logical_demand_continuations(&job)?;
        let request_state = job
            .get("request_state")
            .and_then(Value::as_str)
            .unwrap_or("unknown");

        if request_state == "published" {
            if let Some(verified) = verified.as_ref() {
                if let (Ok(status_receipt), Ok(metadata)) = (
                    published_refresh_receipt_for_index(&job, verified),
                    SourceBackedPublicationMetadata::decode(verified),
                ) {
                    let durable_receipt =
                        published_refresh_receipt_for_index(&metadata.response_value(), verified);
                    if durable_receipt.as_ref().is_ok_and(|receipt| {
                        receipt == &status_receipt
                            && receipt.published_generation == verified.generation_id()
                    }) {
                        let durable_receipt = durable_receipt?;
                        let attempt = recover_exact_published_attempt(
                            &job,
                            &metadata,
                            durable_receipt.clone(),
                            verified,
                        )?;
                        let request_id = attempt.request_id.clone();
                        let terminal = CoreRefreshTerminalSuccess::bind(
                            durable_receipt,
                            Arc::clone(verified),
                        )?;
                        let has_successors = !queued_successors.is_empty();
                        self.install_published_recovery(
                            attempt,
                            terminal,
                            queued_successors,
                            recovered_continuations,
                            active_generation,
                        )?;
                        let _ = self.finish_route_admissions(&request_id, true, None);
                        self.persist_job_status(data_root, &request_id)?;
                        return Ok(has_successors);
                    }
                }
            }
        }

        let job_request_id = required_nonempty_string(&job, "request_id", "source refresh job")?;
        let continuation_predecessor_is_active = verified.as_ref().is_some_and(|verified| {
            recovered_continuations
                .get(job_request_id)
                .filter(|continuation| continuation.predecessor_finished)
                .and_then(|continuation| {
                    SourceBackedPublicationMetadata::decode(verified)
                        .ok()
                        .map(|metadata| metadata.request_id == continuation.predecessor_request_id)
                })
                == Some(true)
        });
        let previous_generation = job.get("previous_generation").and_then(Value::as_str);
        let pointer_advanced = active_generation.as_deref() != previous_generation
            && !continuation_predecessor_is_active;
        // A terminal job must always recover or reject its exact publication,
        // even when its persisted previous-generation pointer already equals
        // the active generation.
        if pointer_advanced || request_state == "published" {
            let active_generation = active_generation.ok_or_else(|| {
                anyhow!("interrupted source refresh advanced Core without an active generation")
            })?;
            let verified = verified.ok_or_else(|| {
                anyhow!("interrupted source refresh advanced Core without a verified generation")
            })?;
            if verified.publication_metadata().is_none() && request_state == "published" {
                return self.recover_legacy_published(
                    data_root,
                    &job,
                    active_generation,
                    queued_successors,
                    recovered_continuations,
                );
            }
            let metadata = SourceBackedPublicationMetadata::decode(&verified)
                .context("recover exact terminal refresh receipt from Core publication metadata")?;
            if metadata.request_id != job_request_id {
                bail!("active Core refresh metadata belongs to a different request");
            }
            let job_operation = SourceBackedRefreshOperation::from_request_json(&job)?;
            let job_scope = refresh_scope_from_json(job.get("refresh_scope"))?;
            if metadata.operation != job_operation || metadata.refresh_scope != job_scope {
                bail!("active Core refresh metadata does not match the interrupted request");
            }
            let receipt =
                published_refresh_receipt_for_index(&metadata.response_value(), verified.as_ref())?;
            if receipt.published_generation != active_generation {
                bail!("active Core refresh metadata names a different generation");
            }
            let attempt =
                recover_committed_attempt(&job, &metadata, receipt.clone(), verified.as_ref())?;
            let terminal = CoreRefreshTerminalSuccess::bind(receipt, Arc::clone(&verified))?;
            let has_successors = !queued_successors.is_empty();
            self.install_published_recovery(
                attempt,
                terminal,
                queued_successors,
                recovered_continuations,
                Some(active_generation),
            )?;
            let _ = self.finish_route_admissions(job_request_id, true, None);
            self.persist_job_status(data_root, job_request_id)?;
            return Ok(has_successors);
        }

        if request_state == "failed" {
            let failed = recover_failed_attempt(&job)?;
            let failed_request_id = failed.request_id.clone();
            let failure_route_dispositions = failed.failure_outcome.as_ref().map(|outcome| {
                (
                    outcome.retryable_routes.clone(),
                    outcome.blocked_routes.clone(),
                )
            });
            {
                let mut state = self.lock_state();
                state.attempts.push_back(failed);
                install_recovered_successors(&mut state, queued_successors)?;
                state
                    .manual_all_continuations
                    .extend(recovered_continuations);
                state.current_published_generation = active_generation;
                if let Some((retryable_routes, blocked_routes)) =
                    failure_route_dispositions.as_ref()
                {
                    Self::restore_route_dispositions_locked(
                        &mut state,
                        retryable_routes,
                        blocked_routes,
                    );
                }
                trim_terminal_attempt_history(&mut state);
            }
            let finish = self.finish_route_admissions(&failed_request_id, false, None);
            let has_successors = {
                let state = self.lock_state();
                state.active_request_id.is_some() || !state.pending_request_ids.is_empty()
            };
            if finish.durable_request_id != failed_request_id || has_successors {
                self.persist_job_status(data_root, &finish.durable_request_id)?;
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

        let recovered_previous_generation = if continuation_predecessor_is_active {
            optional_generation(job.get("previous_generation"))?
        } else {
            active_generation.clone()
        };
        let root = recover_queued_root(&job, recovered_previous_generation)?;
        let request_id = root.request_id.clone();
        {
            let mut state = self.lock_state();
            if state.active_request_id.is_some() || !state.pending_request_ids.is_empty() {
                bail!("interrupted source refresh recovery conflicts with an active queue");
            }
            state.active_request_id = Some(request_id.clone());
            state.attempts.push_back(root);
            install_recovered_successors(&mut state, queued_successors)?;
            state
                .manual_all_continuations
                .extend(recovered_continuations);
            state.current_published_generation = active_generation;
        }
        self.persist_job_status(data_root, &request_id)?;
        Ok(true)
    }

    fn install_published_recovery(
        &self,
        attempt: SourceBackedRefreshAttempt,
        terminal: CoreRefreshTerminalSuccess,
        queued_successors: Vec<SourceBackedRefreshAttempt>,
        recovered_continuations: BTreeMap<String, ManualAllContinuation>,
        active_generation: Option<String>,
    ) -> Result<()> {
        let route_dispositions = attempt
            .receipt
            .as_ref()
            .map(SourceBackedRefreshReceipt::route_retry_dispositions)
            .unwrap_or_default();
        let mut state = self.lock_state();
        terminal.install(&mut state);
        state.attempts.push_back(attempt);
        install_recovered_successors(&mut state, queued_successors)?;
        state
            .manual_all_continuations
            .extend(recovered_continuations);
        state.current_published_generation = active_generation;
        Self::restore_route_dispositions_locked(
            &mut state,
            &route_dispositions.0,
            &route_dispositions.1,
        );
        trim_terminal_attempt_history(&mut state);
        Ok(())
    }

    fn recover_legacy_published(
        &self,
        data_root: &Path,
        job: &Value,
        active_generation: String,
        queued_successors: Vec<SourceBackedRefreshAttempt>,
        recovered_continuations: BTreeMap<String, ManualAllContinuation>,
    ) -> Result<bool> {
        let job_generation = required_generation(
            job.get("published_generation"),
            "legacy published refresh generation",
        )?;
        if job_generation != active_generation {
            bail!("legacy Core refresh job names a different published generation");
        }
        if queued_successors.is_empty() {
            self.lock_state().current_published_generation = Some(active_generation);
            return Ok(false);
        }
        let durable_request_id = {
            let mut state = self.lock_state();
            install_recovered_successors(&mut state, queued_successors)?;
            state
                .manual_all_continuations
                .extend(recovered_continuations);
            state.current_published_generation = Some(active_generation);
            state
                .active_request_id
                .as_deref()
                .ok_or_else(|| anyhow!("recovered source refresh successor is unavailable"))?
                .to_owned()
        };
        self.persist_job_status(data_root, &durable_request_id)?;
        Ok(true)
    }
}

fn recover_exact_published_attempt(
    job: &Value,
    metadata: &SourceBackedPublicationMetadata,
    publication_receipt: SourceBackedRefreshReceipt,
    verified: &VerifiedIndex,
) -> Result<SourceBackedRefreshAttempt> {
    require_terminal_state(job, "published", "completed")?;
    let request_receipt = match job.get("request_outcome") {
        Some(outcome) => {
            let mut response = job.clone();
            response["receipt"] = outcome.clone();
            let receipt = published_refresh_receipt_for_index(&response, verified)
                .context("recover exact logical source refresh outcome")?;
            if receipt == publication_receipt {
                bail!("durable logical source refresh redundantly stores its publication receipt");
            }
            receipt
        }
        None => publication_receipt.clone(),
    };
    validate_terminal_receipt_fields(job, &request_receipt)?;
    let mut attempt = recover_terminal_attempt(job, SourceBackedRefreshState::Published)?;
    attempt.request_source_count = Some(request_receipt.source_count(verified));
    attempt.previous_generation = request_receipt.previous_generation.clone();
    attempt.published_generation = Some(request_receipt.published_generation.clone());
    attempt.receipt = Some(request_receipt);
    attempt.publication_receipt = Some(publication_receipt);
    attempt.route_observations = metadata.route_observations.clone();
    attempt.failure_type = None;
    attempt.failure_outcome = None;
    attempt.last_error = None;
    Ok(attempt)
}

fn recover_committed_attempt(
    job: &Value,
    metadata: &SourceBackedPublicationMetadata,
    receipt: SourceBackedRefreshReceipt,
    verified: &VerifiedIndex,
) -> Result<SourceBackedRefreshAttempt> {
    let mut attempt = recover_terminal_attempt(job, SourceBackedRefreshState::Published)?;
    let now = utc_now().timestamp_millis();
    let route_total = receipt.route_results.len();
    attempt.state = SourceBackedRefreshState::Published;
    attempt.finished_at_ms = Some(now);
    attempt.previous_generation = receipt.previous_generation.clone();
    attempt.published_generation = Some(receipt.published_generation.clone());
    attempt.progress = SourceBackedRefreshProgress {
        phase: "published".to_owned(),
        completed_sources: route_total,
        total_sources: route_total,
        ..SourceBackedRefreshProgress::default()
    };
    attempt.progress_total_sources_known = true;
    attempt.scanned_routes = Some(route_total);
    attempt.unsupported_routes = Some(
        receipt
            .route_results
            .iter()
            .filter(|result| result.outcome.failure_class() == Some("incompatible"))
            .count(),
    );
    attempt.request_source_count = Some(receipt.source_count(verified));
    attempt.certified_source_count = Some(receipt.current.source_count);
    attempt.certified_source_bytes = Some(receipt.current.certified_source_bytes);
    attempt.receipt = Some(receipt.clone());
    attempt.publication_receipt = Some(receipt);
    attempt.route_observations = metadata.route_observations.clone();
    attempt.timings = Some(SourceBackedRefreshTimings::default());
    attempt.publication_probe_us = 0;
    attempt.failure_type = None;
    attempt.failure_outcome = None;
    attempt.last_error = None;
    Ok(attempt)
}

fn recover_failed_attempt(job: &Value) -> Result<SourceBackedRefreshAttempt> {
    require_terminal_state(job, "failed", "failed")?;
    if job.get("receipt").is_some() || job.get("request_outcome").is_some() {
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
        &["search", "periodic", "import", "recovery"],
    )?;
    let trigger_provenance = recover_static_field(
        job,
        "trigger_provenance",
        &[
            "manual",
            "autostart",
            "daemon_scheduler",
            "explicit_source_catalog",
            "commit_payload",
        ],
    )?;
    let requested_catalog = job
        .get("requested_explicit_source_catalog")
        .filter(|value| !value.is_null())
        .map(ExplicitSourceCatalogAuthority::from_json)
        .transpose()?;
    let previous_generation = optional_generation(job.get("previous_generation"))?;
    let mut attempt = new_refresh_attempt(
        previous_generation,
        SourceRefreshRuntimeMetadata {
            operation,
            daemon_mode,
            trigger,
            trigger_provenance,
        },
        requested_catalog,
        refresh_scope_from_json(job.get("refresh_scope"))?,
    );
    attempt.request_id =
        required_nonempty_string(job, "request_id", "terminal source refresh")?.to_owned();
    attempt.physical_attempt_id = optional_string(job, "physical_attempt_id")?;
    attempt.state = state;
    attempt.requested_at_ms = optional_i64(job, "requested_at_ms")?
        .or(optional_i64(job, "last_run_at_ms")?)
        .ok_or_else(|| anyhow!("durable terminal source refresh has no request timestamp"))?;
    attempt.started_at_ms = optional_i64(job, "started_at_ms")?;
    attempt.finished_at_ms = optional_i64(job, "finished_at_ms")?;
    attempt.published_generation = optional_generation(job.get("published_generation"))?;
    attempt.fresh_after_admitted_snapshot = job
        .get("fresh_after_admitted_snapshot")
        .and_then(Value::as_bool)
        .unwrap_or_default();
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
    attempt.coalesced_into_request_id = optional_string(job, "coalesced_into_request_id")?;
    attempt.coalesced_logical_demands =
        optional_u64(job, "coalesced_logical_demands")?.unwrap_or_default();
    attempt.coalesced_requests = optional_u64(job, "coalesced_requests")?.unwrap_or_default();
    attempt.progress = SourceBackedRefreshProgress::from_status_json(job)?;
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
    if attempt.physical_attempt_id.is_none() {
        attempt.physical_attempt_id = Some(
            attempt
                .coalesced_into_request_id
                .clone()
                .filter(|_| attempt.scanned_routes == Some(0))
                .unwrap_or_else(|| attempt.request_id.clone()),
        );
    }
    Ok(attempt)
}

fn recover_failure_outcome(
    job: &Value,
    scope: &SourceBackedRefreshScope,
    legacy_failure_type: Option<&'static str>,
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
    let retry_advice = match fields.get("retry_advice") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(
            value
                .parse::<RefreshRetryAdvice>()
                .map_err(|_| {
                    anyhow!("durable terminal source refresh outcome has invalid retry advice")
                })?
                .as_str(),
        ),
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
    match (retryable_routes, blocked_routes) {
        (Some(retryable_routes), Some(blocked_routes)) => {
            if !retryable_routes.is_disjoint(&blocked_routes)
                || retryable_routes
                    .union(&blocked_routes)
                    .ne(affected_routes.iter())
                || (!affected_routes.is_empty() && retryable != !retryable_routes.is_empty())
            {
                bail!("durable terminal source refresh outcome has inconsistent route disposition");
            }
            Ok(Some(
                SourceBackedRefreshFailureOutcome::with_route_dispositions(
                    code.as_str(),
                    class.as_str(),
                    retryable,
                    retryable_routes,
                    blocked_routes,
                    retry_advice,
                ),
            ))
        }
        (None, None) => Ok(Some(SourceBackedRefreshFailureOutcome::new(
            code.as_str(),
            class.as_str(),
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
    failure_type: &'static str,
    scope: &SourceBackedRefreshScope,
) -> SourceBackedRefreshFailureOutcome {
    let (class, retryable, retry_advice) = match failure_type {
        "source_unavailable" => ("unavailable", true, "retry_affected_routes"),
        "source_changed" => ("source_changed", true, "retry_affected_routes"),
        "malformed_source" => ("unreadable", false, "inspect_sources"),
        "unsupported_schema" => ("incompatible", false, "upgrade_or_reconfigure"),
        _ => ("mixed", true, "retry_affected_routes"),
    };
    let affected_routes = match scope {
        SourceBackedRefreshScope::All => BTreeSet::new(),
        SourceBackedRefreshScope::Exact(routes) => routes.clone(),
    };
    SourceBackedRefreshFailureOutcome::new(
        failure_type,
        class,
        retryable,
        affected_routes,
        Some(retry_advice),
    )
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
        bail!("durable logical source refresh response does not match its exact outcome receipt");
    }
    Ok(())
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

fn recover_optional_failure_type(job: &Value) -> Result<Option<&'static str>> {
    match job.get("failure_type") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => [
            "unsupported_schema",
            "malformed_source",
            "source_unavailable",
            "source_changed",
            "source_failures",
        ]
        .into_iter()
        .find(|accepted| accepted == value)
        .map(Some)
        .ok_or_else(|| anyhow!("durable terminal source refresh has invalid failure type")),
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
