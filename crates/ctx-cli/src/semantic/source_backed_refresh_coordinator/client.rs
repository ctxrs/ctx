use super::*;

#[path = "client_observation_recovery.rs"]
mod observation_recovery;
use observation_recovery::{
    request_bound_status_with_recovery, retained_request_unobservable, DISCONNECT_POLICY,
};

type SourceBackedRefreshProgressReporter<'a> =
    &'a mut dyn FnMut(&SourceBackedRefreshProgress) -> Result<()>;

const SOURCE_REFRESH_PROGRESS_HEARTBEAT: StdDuration = StdDuration::from_secs(5);
const SOURCE_REFRESH_STRUCTURED_ROUTE_LIMIT: usize = 256;

#[derive(Debug)]
pub(crate) struct SourceBackedRefreshDaemonUnavailable {
    detail: Option<String>,
}

impl SourceBackedRefreshDaemonUnavailable {
    fn new(detail: Option<String>) -> Self {
        Self { detail }
    }
}

impl fmt::Display for SourceBackedRefreshDaemonUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the ctx daemon is unavailable for source-backed refresh")?;
        if let Some(detail) = self.detail.as_deref() {
            write!(formatter, ": {detail}")?;
        }
        formatter.write_str("; no foreground writer was started")
    }
}

impl std::error::Error for SourceBackedRefreshDaemonUnavailable {}

#[allow(dead_code)] // Request metadata is retained for CLI/status integrations.
pub(crate) struct SourceBackedRefreshObservation {
    pub(crate) mode: SourceBackedRefreshMode,
    pub(crate) status: String,
    pub(crate) request_id: Option<String>,
    pub(crate) daemon_available: bool,
    pub(crate) source_count: usize,
    pub(crate) request_previous_generation: Option<String>,
    pub(crate) request_generation_changed: bool,
    pub(crate) scanned_routes: Option<usize>,
    pub(crate) receipt: Option<SourceBackedRefreshReceipt>,
    pub(crate) pin: PinnedSourceBackedGeneration,
}

#[derive(Debug)]
pub(crate) struct SourceBackedRefreshTerminalError {
    pub(crate) code: String,
    pub(crate) class: String,
    pub(crate) retryable: bool,
    pub(crate) affected_routes: Vec<String>,
    pub(crate) retryable_routes: Vec<String>,
    pub(crate) blocked_routes: Vec<String>,
    pub(crate) physical_attempt_id: String,
    pub(crate) retained_generation: Option<String>,
    pub(crate) retry_advice: Option<String>,
    detail: Option<String>,
    capture_error: Option<CaptureError>,
}

impl fmt::Display for SourceBackedRefreshTerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "daemon-owned source refresh failed (code={}, class={}, retryable={}, attempt={}",
            self.code, self.class, self.retryable, self.physical_attempt_id
        )?;
        write!(
            formatter,
            ", affected_routes={:?}, retryable_routes={:?}, blocked_routes={:?}",
            self.affected_routes, self.retryable_routes, self.blocked_routes
        )?;
        write!(
            formatter,
            ", retained_generation={:?}, retry_advice={:?})",
            self.retained_generation, self.retry_advice
        )?;
        if let Some(detail) = self.detail.as_deref() {
            write!(formatter, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SourceBackedRefreshTerminalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.capture_error
            .as_ref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SourceRefreshProtocolState {
    AdmissionPending,
    Queued,
    Running,
    Published,
    Failed,
}

#[derive(Debug)]
struct SourceRefreshProtocolStatus {
    state: SourceRefreshProtocolState,
    structured_outcome: Option<SourceBackedRefreshTerminalError>,
}

const AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT: usize = 3;
const TYPED_UNKNOWN_RECOVERY_ATTEMPT_LIMIT: usize = 3;

#[derive(Debug)]
struct SourceRefreshAdmissionRecoveryFailed {
    request_id: String,
    recovery_attempts: usize,
}

impl fmt::Display for SourceRefreshAdmissionRecoveryFailed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "daemon source refresh admission acknowledgement for {} remains unresolved after {} same-request recovery attempts; the request may be durably admitted and disconnect does not cancel it",
            self.request_id, self.recovery_attempts
        )
    }
}

impl std::error::Error for SourceRefreshAdmissionRecoveryFailed {}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceRefreshRequestRecoveryFailureReason {
    AttemptsExhausted,
    RequestIdChanged,
    ReenqueueFailed,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceRefreshRequestRetention {
    NotRetained,
    MayBeRetained,
}

#[derive(Debug)]
pub(super) struct SourceRefreshRequestRecoveryFailed {
    pub(super) request_id: String,
    pub(super) recovery_attempts: usize,
    pub(super) reason: SourceRefreshRequestRecoveryFailureReason,
    pub(super) retention: SourceRefreshRequestRetention,
    pub(super) disconnect_policy: Option<&'static str>,
    pub(super) detail: Option<String>,
}

impl fmt::Display for SourceRefreshRequestRecoveryFailed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.reason {
            SourceRefreshRequestRecoveryFailureReason::AttemptsExhausted => {
                "typed unknown-request recovery attempts were exhausted"
            }
            SourceRefreshRequestRecoveryFailureReason::RequestIdChanged => {
                "typed unknown-request recovery returned a different logical request ID"
            }
            SourceRefreshRequestRecoveryFailureReason::ReenqueueFailed => {
                "same-ID recovery could not durably re-admit the logical request"
            }
        };
        write!(
            formatter,
            "daemon source refresh request {} could not be conclusively recovered after {} recovery attempts: {reason}",
            self.request_id, self.recovery_attempts
        )?;
        match self.retention {
            SourceRefreshRequestRetention::NotRetained => {
                formatter.write_str("; request_retained=false")?;
            }
            SourceRefreshRequestRetention::MayBeRetained => {
                write!(
                    formatter,
                    "; request_retained=unknown; disconnect_policy={}",
                    self.disconnect_policy.unwrap_or(DISCONNECT_POLICY)
                )?;
            }
        }
        if let Some(detail) = self.detail.as_deref() {
            write!(formatter, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SourceRefreshRequestRecoveryFailed {}

#[derive(Debug)]
pub(super) struct TypedUnknownRequestRecovery {
    attempts: usize,
}

impl TypedUnknownRequestRecovery {
    pub(super) fn new(_initial_request_id: &str) -> Self {
        Self { attempts: 0 }
    }

    fn begin_attempt(&mut self, request_id: &str) -> Result<StdDuration> {
        if self.attempts >= TYPED_UNKNOWN_RECOVERY_ATTEMPT_LIMIT {
            return Err(SourceRefreshRequestRecoveryFailed {
                request_id: request_id.to_owned(),
                recovery_attempts: self.attempts,
                reason: SourceRefreshRequestRecoveryFailureReason::AttemptsExhausted,
                retention: SourceRefreshRequestRetention::NotRetained,
                disconnect_policy: None,
                detail: None,
            }
            .into());
        }
        let backoff = match self.attempts {
            0 => StdDuration::from_millis(25),
            1 => StdDuration::from_millis(50),
            _ => StdDuration::from_millis(100),
        };
        self.attempts = self.attempts.saturating_add(1);
        Ok(backoff)
    }

    fn accept_recovered_request_id(
        &mut self,
        previous_request_id: &str,
        recovered_request_id: String,
    ) -> Result<String> {
        if recovered_request_id != previous_request_id {
            return Err(SourceRefreshRequestRecoveryFailed {
                request_id: previous_request_id.to_owned(),
                recovery_attempts: self.attempts,
                reason: SourceRefreshRequestRecoveryFailureReason::RequestIdChanged,
                retention: SourceRefreshRequestRetention::NotRetained,
                disconnect_policy: None,
                detail: Some(format!(
                    "recovery response named logical request {recovered_request_id}"
                )),
            }
            .into());
        }
        Ok(recovered_request_id)
    }
}

pub(super) fn recover_typed_unknown_request_with<S, R>(
    recovery: &mut TypedUnknownRequestRecovery,
    request_id: &str,
    sleep: S,
    reenqueue: R,
) -> Result<String>
where
    S: FnOnce(StdDuration),
    R: FnOnce() -> Result<String>,
{
    let backoff = recovery.begin_attempt(request_id)?;
    sleep(backoff);
    let recovered_request_id = reenqueue().map_err(|error| {
        let retention = if error
            .downcast_ref::<SourceRefreshAdmissionRecoveryFailed>()
            .is_some()
        {
            SourceRefreshRequestRetention::MayBeRetained
        } else {
            SourceRefreshRequestRetention::NotRetained
        };
        SourceRefreshRequestRecoveryFailed {
            request_id: request_id.to_owned(),
            recovery_attempts: recovery.attempts,
            reason: SourceRefreshRequestRecoveryFailureReason::ReenqueueFailed,
            retention,
            disconnect_policy: (retention == SourceRefreshRequestRetention::MayBeRetained)
                .then_some(DISCONNECT_POLICY),
            detail: Some(format!("{error:#}")),
        }
    })?;
    recovery.accept_recovered_request_id(request_id, recovered_request_id)
}

fn request_admission_with_recovery<S, R>(
    request_id: &str,
    mut sleep: S,
    mut roundtrip: R,
) -> Result<Option<Value>>
where
    S: FnMut(StdDuration),
    R: FnMut() -> Result<Option<Value>>,
{
    match roundtrip() {
        Ok(response) => return Ok(response),
        Err(error)
            if error
                .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                .is_some() =>
        {
            return Err(error)
        }
        Err(error)
            if DaemonSourceRefreshServiceUnavailable::request_may_have_been_submitted(&error) => {}
        Err(error) => return Err(error),
    }

    for recovery_attempt in 0..AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT {
        let backoff = match recovery_attempt {
            0 => StdDuration::from_millis(25),
            1 => StdDuration::from_millis(50),
            _ => StdDuration::from_millis(100),
        };
        sleep(backoff);
        if let Ok(Some(response)) = roundtrip() {
            return Ok(Some(response));
        }
    }

    Err(SourceRefreshAdmissionRecoveryFailed {
        request_id: request_id.to_owned(),
        recovery_attempts: AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT,
    }
    .into())
}

/// Coordinates source-backed refresh without ever falling back to a foreground
/// writer. The returned reader is already pinned to one verified generation.
pub(crate) fn coordinate_source_backed_refresh(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_source_backed_refresh_with_catalog(
        data_root,
        mode,
        SourceBackedRefreshOperation::Refresh,
        None,
        false,
        true,
        None,
    )
}

pub(crate) fn coordinate_import_source_backed_refresh_with_progress(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    allow_daemon_autostart: bool,
    report_progress: &mut dyn FnMut(&SourceBackedRefreshProgress) -> Result<()>,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_import_source_backed_refresh_inner(
        data_root,
        mode,
        explicit_source_catalog,
        allow_daemon_autostart,
        Some(report_progress),
    )
}

fn coordinate_import_source_backed_refresh_inner(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    allow_daemon_autostart: bool,
    report_progress: Option<SourceBackedRefreshProgressReporter<'_>>,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_source_backed_refresh_with_catalog(
        data_root,
        mode,
        if explicit_source_catalog.is_some() {
            SourceBackedRefreshOperation::Import
        } else {
            SourceBackedRefreshOperation::Refresh
        },
        explicit_source_catalog,
        true,
        allow_daemon_autostart,
        report_progress,
    )
}

fn coordinate_source_backed_refresh_with_catalog(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    operation: SourceBackedRefreshOperation,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    fresh_after_admitted_snapshot: bool,
    allow_daemon_autostart: bool,
    report_progress: Option<SourceBackedRefreshProgressReporter<'_>>,
) -> Result<SourceBackedRefreshObservation> {
    if mode == SourceBackedRefreshMode::Off {
        if operation == SourceBackedRefreshOperation::Import {
            bail!("explicit source catalog imports require daemon refresh mode `wait`");
        }
        let pin = pin_published_generation(data_root)?.ok_or_else(|| {
            anyhow!("the Core index does not exist; retry with daemon refresh enabled")
        })?;
        return Ok(SourceBackedRefreshObservation {
            mode,
            status: "off".to_owned(),
            request_id: None,
            daemon_available: false,
            source_count: 0,
            request_previous_generation: None,
            request_generation_changed: false,
            scanned_routes: None,
            receipt: None,
            pin,
        });
    }

    let config = AppConfig::load(data_root)
        .context("load daemon configuration before source-backed refresh")?;
    if allow_daemon_autostart && config.daemon.enabled {
        super::super::daemon_autostart::autostart_daemon_and_wait(
            data_root,
            &config,
            crate::DaemonTriggerCommandArg::Search,
        )
        .context("start or recover enabled daemon before source-backed refresh")?;
    }

    let logical_request_id = Uuid::now_v7().to_string();
    let admission_request = wait_authority_request_json(
        data_root,
        &logical_request_id,
        mode,
        operation,
        explicit_source_catalog,
        fresh_after_admitted_snapshot,
    )?;
    let response =
        match request_admission_with_recovery(&logical_request_id, std::thread::sleep, || {
            daemon_source_refresh_request(
                data_root,
                admission_request.clone(),
                SOURCE_REFRESH_IPC_TIMEOUT,
                SOURCE_REFRESH_RESPONSE_MAX_BYTES,
            )
        }) {
            Ok(Some(response)) => response,
            Ok(None) => return daemon_unavailable_fallback(data_root, mode, None),
            Err(error)
                if error
                    .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                    .is_some() =>
            {
                return daemon_unavailable_fallback(data_root, mode, Some(error))
            }
            Err(error) => return Err(error),
        };
    validate_daemon_refresh_response(&response)?;
    let accepted_request_id = response_request_id(&response, "daemon source refresh response")?;
    let request_id = if fresh_after_admitted_snapshot {
        validate_source_refresh_status_response_authority(&response, &logical_request_id)?;
        logical_request_id
    } else {
        validate_source_refresh_status_response_authority(&response, &accepted_request_id)?;
        accepted_request_id
    };
    source_refresh_protocol_state(&response)?;

    if mode == SourceBackedRefreshMode::Background {
        let pin = pin_published_generation(data_root)?.ok_or_else(|| {
            anyhow!(
                "daemon source refresh was queued but no published generation exists; retry with --refresh wait"
            )
        })?;
        return Ok(SourceBackedRefreshObservation {
            mode,
            status: response
                .get("request_state")
                .and_then(Value::as_str)
                .unwrap_or("queued")
                .to_owned(),
            request_id: Some(request_id),
            daemon_available: true,
            source_count: response_source_count(&response),
            request_previous_generation: None,
            request_generation_changed: false,
            scanned_routes: None,
            receipt: None,
            pin,
        });
    }

    wait_for_published_generation_inner(
        data_root,
        request_id,
        PublishedGenerationWait {
            mode,
            operation,
            expected_catalog: explicit_source_catalog,
            fresh_after_admitted_snapshot,
            allow_daemon_autostart,
            report_progress,
        },
    )
}

#[cfg(test)]
pub(super) fn wait_for_published_generation(
    data_root: &Path,
    request_id: String,
    mode: SourceBackedRefreshMode,
    operation: SourceBackedRefreshOperation,
    expected_catalog: Option<&ExplicitSourceCatalogAuthority>,
    allow_daemon_autostart: bool,
) -> Result<SourceBackedRefreshObservation> {
    wait_for_published_generation_inner(
        data_root,
        request_id,
        PublishedGenerationWait {
            mode,
            operation,
            expected_catalog,
            fresh_after_admitted_snapshot: false,
            allow_daemon_autostart,
            report_progress: None,
        },
    )
}

struct PublishedGenerationWait<'catalog, 'progress> {
    mode: SourceBackedRefreshMode,
    operation: SourceBackedRefreshOperation,
    expected_catalog: Option<&'catalog ExplicitSourceCatalogAuthority>,
    fresh_after_admitted_snapshot: bool,
    allow_daemon_autostart: bool,
    report_progress: Option<SourceBackedRefreshProgressReporter<'progress>>,
}

fn wait_for_published_generation_inner(
    data_root: &Path,
    mut request_id: String,
    wait: PublishedGenerationWait<'_, '_>,
) -> Result<SourceBackedRefreshObservation> {
    let PublishedGenerationWait {
        mode,
        operation,
        expected_catalog,
        fresh_after_admitted_snapshot,
        allow_daemon_autostart,
        mut report_progress,
    } = wait;
    let mut unknown_request_recovery = TypedUnknownRequestRecovery::new(&request_id);
    let mut last_reported_progress = None;
    let mut last_reported_at = None;
    loop {
        let status_request = compact_json(json!({
            "schema_version": 1,
            "op": SOURCE_REFRESH_STATUS_OP,
            "request_id": request_id,
        }));
        let response = match request_bound_status_with_recovery(
            &request_id,
            std::thread::sleep,
            || {
                daemon_source_refresh_request(
                    data_root,
                    status_request.clone(),
                    SOURCE_REFRESH_IPC_TIMEOUT,
                    SOURCE_REFRESH_RESPONSE_MAX_BYTES,
                )
            },
        ) {
            Ok(Some(response)) => response,
            Ok(None) => {
                if !allow_daemon_autostart {
                    return Err(retained_request_unobservable(&request_id, 0));
                }
                request_id = recover_wait_refresh_request(
                    data_root,
                    &request_id,
                    operation,
                    expected_catalog,
                    fresh_after_admitted_snapshot,
                    allow_daemon_autostart,
                )
                .with_context(|| {
                    format!("recover daemon while waiting for source refresh request {request_id}")
                })?;
                continue;
            }
            Err(error)
                if error
                    .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                    .is_some() =>
            {
                if !allow_daemon_autostart {
                    return Err(retained_request_unobservable(&request_id, 0));
                }
                request_id = recover_wait_refresh_request(
                    data_root,
                    &request_id,
                    operation,
                    expected_catalog,
                    fresh_after_admitted_snapshot,
                    allow_daemon_autostart,
                )
                .with_context(|| {
                    format!(
                        "recover unavailable daemon while waiting for source refresh request {request_id}: {error:#}"
                    )
                })?;
                continue;
            }
            Err(error) => {
                return Err(error.context("wait for daemon-owned source-backed refresh publication"))
            }
        };
        if source_refresh_request_is_unknown(&response, &request_id)? {
            let lost_request_id = request_id.clone();
            request_id = recover_typed_unknown_request_with(
                &mut unknown_request_recovery,
                &lost_request_id,
                std::thread::sleep,
                || {
                    enqueue_equivalent_wait_refresh_request(
                        data_root,
                        &lost_request_id,
                        operation,
                        expected_catalog,
                        fresh_after_admitted_snapshot,
                    )
                },
            )
            .with_context(|| {
                format!(
                    "reattach unknown daemon source refresh request {lost_request_id} using caller authority"
                )
            })?;
            continue;
        }
        validate_source_refresh_status_response_authority(&response, &request_id)?;
        validate_daemon_refresh_response(&response)?;
        let protocol = source_refresh_protocol_status(&response)?;
        let protocol_state = protocol.state;
        if let Some(report_progress) = report_progress.as_deref_mut() {
            let progress = SourceBackedRefreshProgress::from_status_json(&response)?;
            if should_report_progress(
                last_reported_progress.as_ref(),
                last_reported_at,
                &progress,
                protocol_state,
                StdInstant::now(),
            ) {
                report_progress(&progress)
                    .context("render daemon-owned source refresh progress")?;
                last_reported_progress = Some(progress);
                last_reported_at = Some(StdInstant::now());
            }
        }
        match protocol_state {
            SourceRefreshProtocolState::Published => {
                let expected = response
                    .get("published_generation")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow!("published daemon source refresh has no generation ID")
                    })?;
                let pin = pin_retained_generation(data_root, expected).with_context(|| {
                    format!(
                        "daemon published Core generation {expected}, but its retained terminal generation cannot be opened"
                    )
                })?;
                let publication_receipt = published_refresh_receipt(&response, &pin)?;
                validate_status_publication_authority(&publication_receipt, &pin)?;
                let receipt = published_request_outcome(&response, &pin)?;
                if let Some(expected_catalog) = expected_catalog {
                    if !explicit_catalog_request_is_accounted_for(
                        expected_catalog,
                        receipt.published_explicit_source_catalog.as_ref(),
                        &receipt.catalog_route_bindings,
                        &receipt.route_results,
                    ) {
                        bail!(
                            "daemon published an unexpected explicit source catalog authority: expected {:?}, published {:?}",
                            expected_catalog,
                            receipt.published_explicit_source_catalog,
                        );
                    }
                }
                let request_generation_changed = response
                    .get("generation_changed")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        anyhow!("published daemon source refresh has no request generation outcome")
                    })?;
                let request_previous_generation =
                    optional_generation(response.get("previous_generation"))?;
                let scanned_routes = response
                    .get("scanned_routes")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| {
                        anyhow!("published daemon source refresh has no scanned route count")
                    })?;
                return Ok(SourceBackedRefreshObservation {
                    mode,
                    status: "published".to_owned(),
                    request_id: Some(request_id),
                    daemon_available: true,
                    source_count: response_source_count(&response),
                    request_previous_generation,
                    request_generation_changed,
                    scanned_routes: Some(scanned_routes),
                    receipt: Some(receipt),
                    pin,
                });
            }
            SourceRefreshProtocolState::Failed => {
                return failed_refresh_response(&response, protocol.structured_outcome);
            }
            SourceRefreshProtocolState::AdmissionPending
            | SourceRefreshProtocolState::Queued
            | SourceRefreshProtocolState::Running => {
                std::thread::sleep(SOURCE_REFRESH_POLL_INTERVAL);
            }
        }
    }
}

fn published_request_outcome(
    response: &Value,
    pin: &PinnedSourceBackedGeneration,
) -> Result<SourceBackedRefreshReceipt> {
    let Some(request_outcome) = response.get("request_outcome") else {
        return published_refresh_receipt(response, pin);
    };
    let mut projected = response.clone();
    projected["receipt"] = request_outcome.clone();
    published_refresh_receipt(&projected, pin)
        .context("validate daemon source refresh request outcome")
}

fn validate_status_publication_authority(
    status_receipt: &SourceBackedRefreshReceipt,
    pin: &PinnedSourceBackedGeneration,
) -> Result<()> {
    if pin.verified_index().publication_metadata().is_none() {
        return missing_status_publication_authority();
    }
    let metadata = SourceBackedPublicationMetadata::decode(pin.verified_index())
        .context("decode Core publication authority for daemon status")?;
    let durable_receipt =
        published_refresh_receipt_for_index(&metadata.response_value(), pin.verified_index())?;
    if status_receipt != &durable_receipt {
        bail!("daemon source refresh publication receipt does not match Core metadata");
    }
    Ok(())
}

#[cfg(not(test))]
fn missing_status_publication_authority() -> Result<()> {
    bail!("active Core publication has no source-refresh metadata")
}

#[cfg(test)]
fn missing_status_publication_authority() -> Result<()> {
    // Protocol-state unit tests use synthetic generations without production
    // CommitPayload metadata. Real publications always validate above.
    Ok(())
}

fn should_report_progress(
    last_progress: Option<&SourceBackedRefreshProgress>,
    last_reported_at: Option<StdInstant>,
    progress: &SourceBackedRefreshProgress,
    protocol_state: SourceRefreshProtocolState,
    now: StdInstant,
) -> bool {
    matches!(
        protocol_state,
        SourceRefreshProtocolState::Published | SourceRefreshProtocolState::Failed
    ) || last_progress != Some(progress)
        || last_reported_at.is_some_and(|at| {
            now.saturating_duration_since(at) >= SOURCE_REFRESH_PROGRESS_HEARTBEAT
        })
}

fn failed_refresh_response(
    response: &Value,
    structured: Option<SourceBackedRefreshTerminalError>,
) -> Result<SourceBackedRefreshObservation> {
    if let Some(structured) = structured {
        return Err(structured.into());
    }

    legacy_failed_refresh_response(response)
}

fn legacy_failed_refresh_response(response: &Value) -> Result<SourceBackedRefreshObservation> {
    let error = response
        .get("last_error")
        .and_then(Value::as_str)
        .unwrap_or("source-backed refresh failed");
    let retained = response
        .get("published_generation")
        .and_then(Value::as_str)
        .or_else(|| response.get("previous_generation").and_then(Value::as_str))
        .map(|generation| format!("; retained generation {generation}"))
        .unwrap_or_default();
    let detail = format!("daemon-owned source-backed refresh failed: {error}{retained}");
    match response.get("failure_type").and_then(Value::as_str) {
        Some("unsupported_schema") => Err(CaptureError::UnsupportedSchema(detail).into()),
        Some("malformed_source") => Err(CaptureError::InvalidPayload(detail).into()),
        _ => Err(anyhow!("{detail}")),
    }
}

fn recover_wait_refresh_request(
    data_root: &Path,
    request_id: &str,
    operation: SourceBackedRefreshOperation,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    fresh_after_admitted_snapshot: bool,
    allow_daemon_autostart: bool,
) -> Result<String> {
    if !allow_daemon_autostart {
        return Err(retained_request_unobservable(request_id, 0));
    }
    let recovery = (|| {
        let config =
            AppConfig::load(data_root).context("load daemon configuration for refresh recovery")?;
        if !config.daemon.enabled {
            bail!("daemon was disabled while waiting for source refresh");
        }
        super::super::daemon_autostart::autostart_daemon_and_wait(
            data_root,
            &config,
            crate::DaemonTriggerCommandArg::Search,
        )
        .context("restart daemon-owned source refresh service")?;
        enqueue_equivalent_wait_refresh_request(
            data_root,
            request_id,
            operation,
            explicit_source_catalog,
            fresh_after_admitted_snapshot,
        )
    })();
    recovery.map_err(|error| {
        retained_request_unobservable(request_id, 0).context(format!(
            "recover daemon observation for durably admitted request {request_id}: {error:#}"
        ))
    })
}

fn enqueue_equivalent_wait_refresh_request(
    data_root: &Path,
    request_id: &str,
    operation: SourceBackedRefreshOperation,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    fresh_after_admitted_snapshot: bool,
) -> Result<String> {
    let request = wait_authority_request_json(
        data_root,
        request_id,
        SourceBackedRefreshMode::Wait,
        operation,
        explicit_source_catalog,
        fresh_after_admitted_snapshot,
    )?;
    let response = request_admission_with_recovery(request_id, std::thread::sleep, || {
        daemon_source_refresh_request(
            data_root,
            request.clone(),
            SOURCE_REFRESH_IPC_TIMEOUT,
            SOURCE_REFRESH_RESPONSE_MAX_BYTES,
        )
    })?
    .ok_or_else(|| retained_request_unobservable(request_id, 0))?;
    validate_daemon_refresh_response(&response)?;
    validate_source_refresh_status_response_authority(&response, request_id)?;
    source_refresh_protocol_state(&response)?;
    Ok(request_id.to_owned())
}

fn wait_authority_request_json(
    data_root: &Path,
    request_id: &str,
    mode: SourceBackedRefreshMode,
    operation: SourceBackedRefreshOperation,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    fresh_after_admitted_snapshot: bool,
) -> Result<Value> {
    SourceBackedRefreshRequest::new(
        mode,
        operation,
        explicit_source_catalog,
        fresh_after_admitted_snapshot,
    )
    .with_request_id(request_id)
    .to_json(data_root)
}

fn response_request_id(response: &Value, label: &str) -> Result<String> {
    response
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|request_id| !request_id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{label} has no request ID"))
}

fn source_refresh_protocol_state(response: &Value) -> Result<SourceRefreshProtocolState> {
    Ok(source_refresh_protocol_status(response)?.state)
}

fn source_refresh_protocol_status(response: &Value) -> Result<SourceRefreshProtocolStatus> {
    let state = typed_refresh_state(
        response
            .get("request_state")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("daemon source refresh response has no request state"))?,
    )?;
    let Some(logical_phase) = response.get("logical_phase") else {
        if TYPED_STATUS_FIELDS
            .iter()
            .any(|field| response.get(*field).is_some())
        {
            bail!("daemon source refresh response has a partial typed logical status");
        }
        // Explicit compatibility for records written before logical/physical
        // status and structured terminal outcomes existed.
        return Ok(SourceRefreshProtocolStatus {
            state,
            structured_outcome: None,
        });
    };
    let logical_phase = logical_phase
        .as_str()
        .filter(|phase| {
            [
                "waiting",
                "attached",
                "coverage_check",
                "exact_successor",
                "direct",
                "terminal",
            ]
            .contains(phase)
        })
        .ok_or_else(|| anyhow!("daemon source refresh response has invalid logical phase"))?;
    let request_id = required_status_string(response, "request_id")?;
    if required_status_string(response, "logical_request_id")? != request_id {
        bail!("daemon source refresh logical request authority does not match its request ID");
    }
    let physical_attempt_id = required_status_string(response, "physical_attempt_id")?;
    typed_refresh_state(required_status_string(response, "physical_attempt_state")?)?;
    required_status_string(response, "progress_owner_request_id")?;
    typed_refresh_state(required_status_string(
        response,
        "progress_owner_attempt_state",
    )?)?;

    let terminal = matches!(
        state,
        SourceRefreshProtocolState::Published | SourceRefreshProtocolState::Failed
    );
    if (logical_phase == "terminal") != terminal {
        bail!("daemon source refresh logical phase disagrees with its request state");
    }
    let structured_outcome = response
        .get("structured_outcome")
        .map(parse_structured_outcome)
        .transpose()?;
    if terminal != structured_outcome.is_some() {
        bail!("daemon source refresh terminal state has no structured outcome");
    }
    if structured_outcome
        .as_ref()
        .is_some_and(|outcome| outcome.physical_attempt_id != physical_attempt_id)
    {
        bail!("daemon source refresh outcome names a different physical attempt");
    }
    Ok(SourceRefreshProtocolStatus {
        state,
        structured_outcome,
    })
}

const TYPED_STATUS_FIELDS: &[&str] = &[
    "logical_request_id",
    "physical_attempt_id",
    "physical_attempt_state",
    "progress_owner_request_id",
    "progress_owner_attempt_state",
    "structured_outcome",
];

fn typed_refresh_state(state: &str) -> Result<SourceRefreshProtocolState> {
    match state {
        "admission_pending" => Ok(SourceRefreshProtocolState::AdmissionPending),
        "queued" => Ok(SourceRefreshProtocolState::Queued),
        "running" => Ok(SourceRefreshProtocolState::Running),
        "published" => Ok(SourceRefreshProtocolState::Published),
        "failed" => Ok(SourceRefreshProtocolState::Failed),
        _ => Err(anyhow!(
            "daemon source refresh response has unknown typed state `{state}`"
        )),
    }
}

fn required_status_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("daemon source refresh response has invalid `{field}`"))
}

const OUTCOME_CODES: &[&str] = &[
    "completed",
    "completed_with_rejections",
    "completed_with_source_failures",
    "completed_with_rejections_and_source_failures",
    "source_unavailable",
    "source_changed",
    "malformed_source",
    "unsupported_schema",
    "source_failures",
    "logical_source_failures",
    "source_refresh_failed",
    "source_refresh_internal",
    "resource_unavailable",
    "index_incompatible",
    "index_corruption",
    "source_refresh_admission_failed",
    "all_provider_terminal_coverage_unavailable",
];
const OUTCOME_CLASSES: &[&str] = &[
    "completed",
    "completed_with_retryable_failures",
    "completed_with_diagnostics",
    "unavailable",
    "source_changed",
    "unreadable",
    "incompatible",
    "mixed",
    "internal",
    "resource_unavailable",
    "corruption",
    "control_plane",
    "coverage",
];
const RETRY_ADVICES: &[&str] = &[
    "retry_affected_routes",
    "retry_request",
    "retry_admission",
    "retry_finalization",
    "inspect_sources",
    "upgrade_or_reconfigure",
    "rebuild_index",
];

fn parse_structured_outcome(value: &Value) -> Result<SourceBackedRefreshTerminalError> {
    let fields = value
        .as_object()
        .ok_or_else(|| anyhow!("daemon source refresh structured outcome is not an object"))?;
    let code = known_outcome_string(fields, "code", OUTCOME_CODES)?;
    let class = known_outcome_string(fields, "class", OUTCOME_CLASSES)?;
    let retryable = fields
        .get("retryable")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            anyhow!("daemon source refresh structured outcome has invalid retryability")
        })?;
    let affected_routes = structured_outcome_routes(fields, "affected_routes")?;
    let retryable_routes = structured_outcome_routes(fields, "retryable_routes")?;
    let blocked_routes = structured_outcome_routes(fields, "blocked_routes")?;
    if !retryable_routes.is_disjoint(&blocked_routes)
        || !retryable_routes.is_subset(&affected_routes)
        || !blocked_routes.is_subset(&affected_routes)
        || (!affected_routes.is_empty() && retryable != !retryable_routes.is_empty())
    {
        bail!("daemon source refresh structured outcome has inconsistent route dispositions");
    }
    let physical_attempt_id = known_outcome_string(fields, "physical_attempt_id", &[])?;
    let retained_generation = optional_outcome_string(fields, "retained_generation")?;
    let retry_advice = optional_outcome_string(fields, "retry_advice")?;
    if retry_advice
        .as_deref()
        .is_some_and(|advice| !RETRY_ADVICES.contains(&advice))
    {
        bail!("daemon source refresh structured outcome has invalid retry advice");
    }
    let detail = optional_outcome_string(fields, "detail")?;
    optional_outcome_string(fields, "published_generation")?;
    let capture_error = match class.as_str() {
        "incompatible" => Some(CaptureError::UnsupportedSchema(
            detail.clone().unwrap_or_else(|| code.clone()),
        )),
        "unreadable" => Some(CaptureError::InvalidPayload(
            detail.clone().unwrap_or_else(|| code.clone()),
        )),
        _ => None,
    };
    Ok(SourceBackedRefreshTerminalError {
        code,
        class,
        retryable,
        affected_routes: affected_routes
            .into_iter()
            .map(|route| route.as_str().to_owned())
            .collect(),
        retryable_routes: retryable_routes
            .into_iter()
            .map(|route| route.as_str().to_owned())
            .collect(),
        blocked_routes: blocked_routes
            .into_iter()
            .map(|route| route.as_str().to_owned())
            .collect(),
        physical_attempt_id,
        retained_generation,
        retry_advice,
        detail,
        capture_error,
    })
}

fn known_outcome_string(
    fields: &serde_json::Map<String, Value>,
    field: &str,
    accepted: &[&str],
) -> Result<String> {
    let value = fields
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("daemon source refresh structured outcome has invalid `{field}`"))?;
    if !accepted.is_empty() && !accepted.contains(&value) {
        bail!("daemon source refresh structured outcome has unknown {field} `{value}`");
    }
    Ok(value.to_owned())
}

fn optional_outcome_string(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<String>> {
    match fields.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        _ => Err(anyhow!(
            "daemon source refresh structured outcome has invalid `{field}`"
        )),
    }
}

fn structured_outcome_routes(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<BTreeSet<ctx_history_index::SourceRouteIdentity>> {
    let values = fields
        .get(field)
        .and_then(Value::as_array)
        .filter(|routes| routes.len() <= SOURCE_REFRESH_STRUCTURED_ROUTE_LIMIT)
        .ok_or_else(|| anyhow!("daemon source refresh structured outcome has invalid `{field}`"))?;
    let routes = values
        .iter()
        .map(|route| {
            route
                .as_str()
                .ok_or_else(|| anyhow!("daemon source refresh outcome route is not a string"))
                .and_then(|route| {
                    ctx_history_index::SourceRouteIdentity::from_sha256(route.to_owned())
                        .map_err(Into::into)
                })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if routes.len() != values.len() {
        bail!("daemon source refresh structured outcome has duplicate `{field}` routes");
    }
    Ok(routes)
}

pub(super) fn validate_source_refresh_status_response_authority(
    response: &Value,
    expected_request_id: &str,
) -> Result<()> {
    let exact = response.get("schema_version").and_then(Value::as_u64) == Some(1)
        && response.get("owner").and_then(Value::as_str) == Some("daemon")
        && response.get("request_id").and_then(Value::as_str) == Some(expected_request_id);
    if exact {
        Ok(())
    } else {
        Err(anyhow!(
            "daemon source refresh status response does not match the polled request authority"
        ))
    }
}

pub(super) fn source_refresh_request_is_unknown(
    response: &Value,
    expected_request_id: &str,
) -> Result<bool> {
    if response.get("error_code").and_then(Value::as_str)
        != Some(SOURCE_REFRESH_UNKNOWN_REQUEST_ERROR_CODE)
    {
        return Ok(false);
    }
    let exact = response.get("ok").and_then(Value::as_bool) == Some(false)
        && response.get("schema_version").and_then(Value::as_u64) == Some(1)
        && response.get("owner").and_then(Value::as_str) == Some("daemon")
        && response.get("request_id").and_then(Value::as_str) == Some(expected_request_id)
        && response.get("request_state").and_then(Value::as_str)
            == Some(SOURCE_REFRESH_UNKNOWN_REQUEST_STATE)
        && response.get("retryable").and_then(Value::as_bool) == Some(true);
    if exact {
        Ok(true)
    } else {
        Err(anyhow!(
            "daemon source refresh unknown-request response does not match the polled request authority"
        ))
    }
}

fn daemon_unavailable_fallback(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    error: Option<anyhow::Error>,
) -> Result<SourceBackedRefreshObservation> {
    if mode == SourceBackedRefreshMode::Background {
        if let Some(pin) = pin_published_generation(data_root)? {
            return Ok(SourceBackedRefreshObservation {
                mode,
                status: "daemon_unavailable".to_owned(),
                request_id: None,
                daemon_available: false,
                source_count: 0,
                request_previous_generation: None,
                request_generation_changed: false,
                scanned_routes: None,
                receipt: None,
                pin,
            });
        }
    }
    Err(SourceBackedRefreshDaemonUnavailable::new(error.map(|error| format!("{error:#}"))).into())
}

fn validate_daemon_refresh_response(response: &Value) -> Result<()> {
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(anyhow!(
        "{}",
        response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("daemon source refresh request failed")
    ))
}

fn response_source_count(response: &Value) -> usize {
    response
        .get("progress")
        .and_then(|progress| progress.get("total_sources"))
        .or_else(|| response.get("source_count"))
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod progress_poll_tests {
    use super::*;

    fn typed_terminal_status() -> Value {
        let request_id = Uuid::from_u128(0x294_0100).to_string();
        let physical_attempt_id = Uuid::from_u128(0x294_0101).to_string();
        let retryable_route = "a1".repeat(32);
        let blocked_route = "a2".repeat(32);
        json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": request_id,
            "request_state": "failed",
            "logical_request_id": request_id,
            "logical_phase": "terminal",
            "physical_attempt_id": physical_attempt_id,
            "physical_attempt_state": "failed",
            "progress_owner_request_id": physical_attempt_id,
            "progress_owner_attempt_state": "failed",
            "structured_outcome": {
                "code": "source_failures",
                "class": "mixed",
                "retryable": true,
                "affected_routes": [retryable_route.clone(), blocked_route.clone()],
                "retryable_routes": [retryable_route],
                "blocked_routes": [blocked_route],
                "physical_attempt_id": physical_attempt_id,
                "retained_generation": "b1".repeat(32),
                "retry_advice": "retry_affected_routes",
                "detail": "typed mixed route outcome",
            },
        })
    }

    #[test]
    fn identical_poll_is_suppressed_until_heartbeat_or_terminal_state() {
        let progress = SourceBackedRefreshProgress::default();
        let now = StdInstant::now();
        assert!(!should_report_progress(
            Some(&progress),
            Some(now),
            &progress,
            SourceRefreshProtocolState::Running,
            now,
        ));
        assert!(should_report_progress(
            Some(&progress),
            Some(now),
            &progress,
            SourceRefreshProtocolState::Running,
            now + SOURCE_REFRESH_PROGRESS_HEARTBEAT,
        ));
        assert!(should_report_progress(
            Some(&progress),
            Some(now),
            &progress,
            SourceRefreshProtocolState::Published,
            now,
        ));
    }

    #[test]
    fn structured_terminal_error_preserves_engine_route_dispositions() {
        let response = typed_terminal_status();
        let protocol = source_refresh_protocol_status(&response).unwrap();
        assert_eq!(protocol.state, SourceRefreshProtocolState::Failed);
        let error = match failed_refresh_response(&response, protocol.structured_outcome) {
            Ok(_) => panic!("failed status must return a terminal error"),
            Err(error) => error,
        };
        let terminal = error
            .downcast_ref::<SourceBackedRefreshTerminalError>()
            .expect("typed terminal error");

        assert_eq!(terminal.code, "source_failures");
        assert_eq!(terminal.class, "mixed");
        assert!(terminal.retryable);
        assert_eq!(terminal.affected_routes.len(), 2);
        assert_eq!(terminal.retryable_routes, vec!["a1".repeat(32)]);
        assert_eq!(terminal.blocked_routes, vec!["a2".repeat(32)]);
        assert_eq!(
            terminal.physical_attempt_id,
            Uuid::from_u128(0x294_0101).to_string()
        );
        assert_eq!(
            terminal.retained_generation.as_deref(),
            Some("b1".repeat(32).as_str())
        );
        assert_eq!(
            terminal.retry_advice.as_deref(),
            Some("retry_affected_routes")
        );
    }

    #[test]
    fn present_structured_fields_are_strictly_validated() {
        let mut unknown = typed_terminal_status();
        unknown["structured_outcome"]["code"] = json!("invented_code");
        assert!(format!(
            "{:#}",
            source_refresh_protocol_status(&unknown).unwrap_err()
        )
        .contains("unknown code"));

        let mut overlap = typed_terminal_status();
        overlap["structured_outcome"]["blocked_routes"] = json!(["a1".repeat(32)]);
        assert!(format!(
            "{:#}",
            source_refresh_protocol_status(&overlap).unwrap_err()
        )
        .contains("inconsistent route dispositions"));

        let mut partial = typed_terminal_status();
        partial.as_object_mut().unwrap().remove("logical_phase");
        assert!(format!(
            "{:#}",
            source_refresh_protocol_status(&partial).unwrap_err()
        )
        .contains("partial typed logical status"));
    }

    #[test]
    fn attached_logical_phase_remains_active_until_engine_terminalizes_it() {
        let request_id = Uuid::from_u128(0x294_0200).to_string();
        let physical_attempt_id = Uuid::from_u128(0x294_0201).to_string();
        let response = json!({
            "request_id": request_id,
            "request_state": "queued",
            "logical_request_id": request_id,
            "logical_phase": "attached",
            "physical_attempt_id": physical_attempt_id,
            "physical_attempt_state": "running",
            "progress_owner_request_id": physical_attempt_id,
            "progress_owner_attempt_state": "running",
        });

        let protocol = source_refresh_protocol_status(&response).unwrap();
        assert_eq!(protocol.state, SourceRefreshProtocolState::Queued);
        assert!(protocol.structured_outcome.is_none());
    }

    #[test]
    fn legacy_terminal_record_uses_explicit_failure_type_fallback() {
        let response = json!({
            "request_id": Uuid::from_u128(0x294_0300).to_string(),
            "request_state": "failed",
            "failure_type": "unsupported_schema",
            "last_error": "legacy incompatible source",
            "previous_generation": "c1".repeat(32),
        });
        let protocol = source_refresh_protocol_status(&response).unwrap();
        assert!(protocol.structured_outcome.is_none());
        let error = match failed_refresh_response(&response, None) {
            Ok(_) => panic!("legacy failed status must return an error"),
            Err(error) => error,
        };
        assert!(error
            .chain()
            .any(|cause| cause.downcast_ref::<CaptureError>().is_some()));
    }
}

#[cfg(test)]
#[path = "client_admission_recovery_tests.rs"]
mod admission_recovery_tests;

#[cfg(all(test, unix))]
#[path = "client_transport_recovery_tests.rs"]
mod transport_recovery_tests;
