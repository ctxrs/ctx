use super::*;

/// Verified terminal receipt for one daemon-owned source refresh.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshReceipt {
    pub(crate) previous_generation: Option<String>,
    pub(crate) published_generation: String,
    pub(crate) generation_changed: bool,
    pub(crate) published_explicit_source_catalog: ExplicitSourceCatalogAuthority,
    pub(crate) current: SourceBackedRefreshCurrent,
    pub(crate) selected_route_ids: Vec<String>,
    pub(crate) successful_route_ids: Vec<String>,
    pub(crate) successful_route_changes: BTreeMap<String, bool>,
    pub(crate) selected_route_total: usize,
    pub(crate) successful_route_total: usize,
    pub(crate) failed_route_outcomes: Vec<SourceBackedRefreshRouteFailure>,
    pub(crate) catalog_route_outcomes: Vec<SourceBackedRefreshCatalogRouteOutcome>,
    pub(crate) source_failures: Vec<SourceBackedRefreshSourceFailure>,
}

impl SourceBackedRefreshReceipt {
    pub(super) fn from_verified_publication(
        previous_generation: Option<String>,
        published_generation: String,
        publication: &SourceBackedRefreshPublication,
    ) -> Self {
        Self {
            generation_changed: previous_generation.as_deref()
                != Some(published_generation.as_str()),
            previous_generation,
            published_generation,
            published_explicit_source_catalog: publication
                .published_explicit_source_catalog
                .clone(),
            current: publication.current,
            selected_route_ids: publication.selected_route_ids.clone(),
            successful_route_ids: publication.successful_route_ids.clone(),
            successful_route_changes: publication.successful_route_changes.clone(),
            selected_route_total: publication.selected_route_ids.len(),
            successful_route_total: publication.successful_route_ids.len(),
            failed_route_outcomes: publication.failed_route_outcomes.clone(),
            catalog_route_outcomes: publication.catalog_route_outcomes.clone(),
            source_failures: publication.source_failures.clone(),
        }
    }

    fn terminal_outcome(&self) -> &'static str {
        if self.source_failures.is_empty() {
            "completed"
        } else {
            "completed_with_source_failures"
        }
    }

    pub(crate) fn source_failure_total(&self) -> usize {
        self.selected_route_total
            .saturating_sub(self.successful_route_total)
    }

    pub(crate) fn source_failures_omitted(&self) -> usize {
        self.source_failure_total()
            .saturating_sub(self.source_failures.len())
    }

    pub(crate) fn to_json(&self) -> Value {
        // Leave ample room beneath the 64 KiB IPC limit for the surrounding
        // attempt envelope. Catalog outcomes and diagnostics share this one
        // budget instead of independently consuming transport capacity.
        const RECEIPT_JSON_BUDGET_BYTES: usize = 48 * 1024;
        let catalog_route_outcomes = self.catalog_route_outcomes_json();
        let mut source_failures = Vec::new();
        for failure in &self.source_failures {
            source_failures.push(failure.to_json());
            let candidate = self.wire_json(&source_failures, &catalog_route_outcomes);
            if serde_json::to_vec(&candidate)
                .map_or(true, |json| json.len() > RECEIPT_JSON_BUDGET_BYTES)
            {
                source_failures.pop();
                break;
            }
        }
        self.wire_json(&source_failures, &catalog_route_outcomes)
    }

    fn wire_json(&self, source_failures: &[Value], catalog_route_outcomes: &Value) -> Value {
        compact_json(json!({
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "generation_changed": self.generation_changed,
            "published_explicit_source_catalog": self
                .published_explicit_source_catalog
                .to_json(),
            "current": self.current.to_json(),
            "outcome": self.terminal_outcome(),
            "selected_route_total": self.selected_route_total,
            "successful_route_total": self.successful_route_total,
            "source_failure_total": self.source_failure_total(),
            "source_failures_omitted": self.source_failure_total()
                .saturating_sub(source_failures.len()),
            "source_failures": source_failures,
            "catalog_route_outcomes": catalog_route_outcomes,
        }))
    }

    fn catalog_route_outcomes_json(&self) -> Value {
        let outcomes = self
            .catalog_route_outcomes
            .iter()
            .map(|outcome| (outcome.catalog_lineage.clone(), outcome.compact_json()))
            .collect::<serde_json::Map<_, _>>();
        Value::Object(outcomes)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshRouteFailure {
    pub(crate) route_identity: String,
    pub(crate) source_identity: String,
    pub(crate) provider: String,
    pub(crate) class: String,
    pub(crate) carried_forward: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshCatalogRouteOutcome {
    pub(crate) catalog_lineage: String,
    pub(crate) route_identity: String,
    pub(crate) outcome: String,
    pub(crate) failure_class: Option<String>,
    pub(crate) changed: Option<bool>,
}

impl SourceBackedRefreshCatalogRouteOutcome {
    fn compact_json(&self) -> Value {
        let outcome = match self.outcome.as_str() {
            "succeeded" => "s",
            "failed" => "f",
            "not_selected" => "n",
            _ => "?",
        };
        let mut fields = vec![json!(self.route_identity), json!(outcome)];
        if let Some(changed) = self.changed {
            fields.push(json!(changed));
        } else if let Some(class) = self.failure_class.as_deref() {
            let class = match class {
                "unavailable" => "u",
                "source_changed" => "c",
                "unreadable" => "r",
                "incompatible" => "i",
                _ => "?",
            };
            fields.push(json!(class));
        }
        Value::Array(fields)
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
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "route_identity": self.route_identity,
            "source_identity": self.source_identity,
            "provider": self.provider,
            "class": self.class,
            "carried_forward": self.carried_forward,
            "source_selector": self.source_selector,
            "detail": self.detail,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshTimings {
    pub(crate) discovery_us: u64,
    pub(crate) scan_stage_us: u64,
    pub(crate) commit_us: u64,
}

impl SourceBackedRefreshTimings {
    pub(crate) fn to_json(self) -> Value {
        json!({
            "discovery": self.discovery_us,
            "scan_stage": self.scan_stage_us,
            "commit": self.commit_us,
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceBackedRefreshState {
    Queued,
    Running,
    Published,
    Failed,
}

impl SourceBackedRefreshState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Published => "published",
            Self::Failed => "failed",
        }
    }

    pub(super) fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum SourceBackedCurrentSourceProgressStage {
    SourceFamilyCopy,
    OnlineBackup,
    LogicalFingerprint,
    LogicalScan,
}

impl SourceBackedCurrentSourceProgressStage {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SourceFamilyCopy => "source_family_copy",
            Self::OnlineBackup => "online_backup",
            Self::LogicalFingerprint => "logical_fingerprint",
            Self::LogicalScan => "logical_scan",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "source_family_copy" => Some(Self::SourceFamilyCopy),
            "online_backup" => Some(Self::OnlineBackup),
            "logical_fingerprint" => Some(Self::LogicalFingerprint),
            "logical_scan" => Some(Self::LogicalScan),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct SourceBackedCurrentSourceProgress {
    pub(crate) stage: SourceBackedCurrentSourceProgressStage,
    pub(crate) snapshot_pages_completed: Option<u64>,
    pub(crate) snapshot_pages_total: Option<u64>,
    pub(crate) snapshot_bytes_completed: Option<u64>,
    pub(crate) snapshot_bytes_total: Option<u64>,
    pub(crate) logical_rows_scanned: Option<u64>,
    pub(crate) logical_certified_bytes: Option<u64>,
}

impl SourceBackedCurrentSourceProgress {
    pub(crate) fn to_json(self) -> Value {
        compact_json(json!({
            "stage": self.stage.as_str(),
            "snapshot_pages_completed": self.snapshot_pages_completed,
            "snapshot_pages_total": self.snapshot_pages_total,
            "snapshot_bytes_completed": self.snapshot_bytes_completed,
            "snapshot_bytes_total": self.snapshot_bytes_total,
            "logical_rows_scanned": self.logical_rows_scanned,
            "logical_certified_bytes": self.logical_certified_bytes,
        }))
    }

    fn from_json(value: &Value) -> Result<Self> {
        let fields = value.as_object().ok_or_else(|| {
            anyhow!("daemon source refresh current-source progress is not an object")
        })?;
        let stage = fields
            .get("stage")
            .and_then(Value::as_str)
            .and_then(SourceBackedCurrentSourceProgressStage::parse)
            .ok_or_else(|| {
                anyhow!("daemon source refresh current-source progress has an invalid stage")
            })?;
        Ok(Self {
            stage,
            snapshot_pages_completed: optional_progress_u64(fields, "snapshot_pages_completed")?,
            snapshot_pages_total: optional_progress_u64(fields, "snapshot_pages_total")?,
            snapshot_bytes_completed: optional_progress_u64(fields, "snapshot_bytes_completed")?,
            snapshot_bytes_total: optional_progress_u64(fields, "snapshot_bytes_total")?,
            logical_rows_scanned: optional_progress_u64(fields, "logical_rows_scanned")?,
            logical_certified_bytes: optional_progress_u64(fields, "logical_certified_bytes")?,
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshProgress {
    pub(crate) phase: String,
    pub(crate) completed_sources: usize,
    pub(crate) total_sources: usize,
    pub(crate) current_source: Option<String>,
    pub(crate) completed_records: Option<u64>,
    pub(crate) completed_bytes: Option<u64>,
    pub(crate) current_source_progress: Option<SourceBackedCurrentSourceProgress>,
}

impl Default for SourceBackedRefreshProgress {
    fn default() -> Self {
        Self {
            phase: "queued".to_owned(),
            completed_sources: 0,
            total_sources: 0,
            current_source: None,
            completed_records: None,
            completed_bytes: None,
            current_source_progress: None,
        }
    }
}

impl SourceBackedRefreshProgress {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "phase": self.phase,
            "completed_sources": self.completed_sources,
            "total_sources": self.total_sources,
            "current_source": self.current_source,
            "completed_records": self.completed_records,
            "completed_bytes": self.completed_bytes,
            "current_source_progress": self.current_source_progress
                .map(SourceBackedCurrentSourceProgress::to_json),
        }))
    }

    pub(crate) fn from_status_json(response: &Value) -> Result<Self> {
        let progress = response
            .get("progress")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("daemon source refresh status has no progress object"))?;
        let phase = progress
            .get("phase")
            .and_then(Value::as_str)
            .filter(|phase| !phase.is_empty())
            .ok_or_else(|| anyhow!("daemon source refresh progress has an invalid phase"))?
            .to_owned();
        let current_source = match progress.get("current_source") {
            None | Some(Value::Null) => None,
            Some(Value::String(source)) => Some(source.clone()),
            Some(_) => bail!("daemon source refresh progress has an invalid current_source"),
        };
        let current_source_progress = match progress.get("current_source_progress") {
            None | Some(Value::Null) => None,
            Some(value) => Some(SourceBackedCurrentSourceProgress::from_json(value)?),
        };
        Ok(Self {
            phase,
            completed_sources: required_progress_usize(progress, "completed_sources")?,
            total_sources: required_progress_usize(progress, "total_sources")?,
            current_source,
            completed_records: optional_progress_u64(progress, "completed_records")?,
            completed_bytes: optional_progress_u64(progress, "completed_bytes")?,
            current_source_progress,
        })
    }
}

fn required_progress_usize(fields: &serde_json::Map<String, Value>, field: &str) -> Result<usize> {
    fields
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| anyhow!("daemon source refresh progress has an invalid {field}"))
}

fn optional_progress_u64(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<u64>> {
    match fields.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            anyhow!("daemon source refresh current-source progress has an invalid {field}")
        }),
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
    pub(super) published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(super) coalesced_requests: u64,
    pub(super) progress: SourceBackedRefreshProgress,
    pub(super) scanned_routes: Option<usize>,
    pub(super) unsupported_routes: Option<usize>,
    pub(super) certified_source_count: Option<usize>,
    pub(super) certified_source_bytes: Option<u64>,
    pub(super) receipt: Option<SourceBackedRefreshReceipt>,
    pub(super) timings: Option<SourceBackedRefreshTimings>,
    pub(super) publication_probe_us: u64,
    pub(super) daemon_mode: DaemonMode,
    pub(super) trigger: &'static str,
    pub(super) trigger_provenance: &'static str,
    pub(super) failure_type: Option<&'static str>,
    pub(super) last_error: Option<String>,
    pub(super) post_publication_error: Option<String>,
}

impl SourceBackedRefreshAttempt {
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

    pub(super) fn to_json(&self) -> Value {
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
            "requested_explicit_source_catalog": self.requested_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "published_explicit_source_catalog": self.published_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "generation_changed": self.receipt.as_ref().map(|receipt| receipt.generation_changed),
            "receipt": self.receipt.as_ref().map(SourceBackedRefreshReceipt::to_json),
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
            "post_publication_error": self.post_publication_error,
        }))
    }

    pub(super) fn job_json(&self) -> Value {
        let status = match self.state {
            SourceBackedRefreshState::Published => "completed",
            SourceBackedRefreshState::Failed => "failed",
            SourceBackedRefreshState::Queued | SourceBackedRefreshState::Running => "running",
        };
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
            "requested_explicit_source_catalog": self.requested_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "published_explicit_source_catalog": self.published_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "generation_changed": self.receipt.as_ref().map(|receipt| receipt.generation_changed),
            "receipt": self.receipt.as_ref().map(SourceBackedRefreshReceipt::to_json),
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
            "post_publication_error": self.post_publication_error,
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
