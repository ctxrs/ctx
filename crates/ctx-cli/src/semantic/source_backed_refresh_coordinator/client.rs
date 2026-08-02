use super::*;

type SourceBackedRefreshProgressReporter<'a> =
    &'a mut dyn FnMut(&SourceBackedRefreshProgress) -> Result<()>;

const SOURCE_REFRESH_PROGRESS_HEARTBEAT: StdDuration = StdDuration::from_secs(5);

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
    pub(crate) receipt: Option<SourceBackedRefreshReceipt>,
    pub(crate) pin: PinnedSourceBackedGeneration,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SourceRefreshProtocolState {
    Queued,
    Running,
    Published,
    Failed,
}

const TYPED_UNKNOWN_RECOVERY_ATTEMPT_LIMIT: usize = 3;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceRefreshRequestRecoveryFailureReason {
    AttemptsExhausted,
    NoRequestIdProgress,
}

#[derive(Debug)]
pub(super) struct SourceRefreshRequestRecoveryFailed {
    pub(super) request_id: String,
    pub(super) recovery_attempts: usize,
    pub(super) reason: SourceRefreshRequestRecoveryFailureReason,
}

impl fmt::Display for SourceRefreshRequestRecoveryFailed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.reason {
            SourceRefreshRequestRecoveryFailureReason::AttemptsExhausted => {
                "typed unknown-request recovery attempts were exhausted"
            }
            SourceRefreshRequestRecoveryFailureReason::NoRequestIdProgress => {
                "typed unknown-request recovery repeated a prior request ID"
            }
        };
        write!(
            formatter,
            "daemon source refresh request {} was lost after {} recovery attempts: {reason}",
            self.request_id, self.recovery_attempts
        )
    }
}

impl std::error::Error for SourceRefreshRequestRecoveryFailed {}

#[derive(Debug)]
pub(super) struct TypedUnknownRequestRecovery {
    attempts: usize,
    seen_request_ids: BTreeSet<String>,
}

impl TypedUnknownRequestRecovery {
    pub(super) fn new(initial_request_id: &str) -> Self {
        Self {
            attempts: 0,
            seen_request_ids: BTreeSet::from([initial_request_id.to_owned()]),
        }
    }

    fn begin_attempt(&mut self, request_id: &str) -> Result<StdDuration> {
        if self.attempts >= TYPED_UNKNOWN_RECOVERY_ATTEMPT_LIMIT {
            return Err(SourceRefreshRequestRecoveryFailed {
                request_id: request_id.to_owned(),
                recovery_attempts: self.attempts,
                reason: SourceRefreshRequestRecoveryFailureReason::AttemptsExhausted,
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
        if recovered_request_id == previous_request_id
            || !self.seen_request_ids.insert(recovered_request_id.clone())
        {
            return Err(SourceRefreshRequestRecoveryFailed {
                request_id: recovered_request_id,
                recovery_attempts: self.attempts,
                reason: SourceRefreshRequestRecoveryFailureReason::NoRequestIdProgress,
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
    let recovered_request_id = reenqueue()?;
    recovery.accept_recovered_request_id(request_id, recovered_request_id)
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
    let implicit_catalog;
    let explicit_source_catalog = if let Some(explicit_source_catalog) = explicit_source_catalog {
        explicit_source_catalog
    } else {
        implicit_catalog = load_explicit_source_catalog_authority(data_root)
            .context("load implicit catalog authority for automatic import")?;
        &implicit_catalog
    };
    coordinate_source_backed_refresh_with_catalog(
        data_root,
        mode,
        SourceBackedRefreshOperation::Import,
        Some(explicit_source_catalog),
        allow_daemon_autostart,
        report_progress,
    )
}

fn coordinate_source_backed_refresh_with_catalog(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    operation: SourceBackedRefreshOperation,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
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

    let response =
        match send_wait_authority_request(data_root, mode, operation, explicit_source_catalog) {
            Ok(Some(response)) => response,
            Ok(None) => return daemon_unavailable_fallback(data_root, mode, None),
            Err(error)
                if error
                    .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                    .is_some() =>
            {
                return daemon_unavailable_fallback(data_root, mode, Some(error))
            }
            Err(error) => return Err(error.context("request daemon-owned source-backed refresh")),
        };
    validate_daemon_refresh_response(&response)?;
    let request_id = response_request_id(&response, "daemon source refresh response")?;
    validate_source_refresh_status_response_authority(&response, &request_id)?;
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
            receipt: None,
            pin,
        });
    }

    wait_for_published_generation_with_progress(
        data_root,
        request_id,
        mode,
        operation,
        explicit_source_catalog,
        allow_daemon_autostart,
        report_progress,
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
        mode,
        operation,
        expected_catalog,
        allow_daemon_autostart,
        None,
    )
}

fn wait_for_published_generation_with_progress(
    data_root: &Path,
    request_id: String,
    mode: SourceBackedRefreshMode,
    operation: SourceBackedRefreshOperation,
    expected_catalog: Option<&ExplicitSourceCatalogAuthority>,
    allow_daemon_autostart: bool,
    report_progress: Option<SourceBackedRefreshProgressReporter<'_>>,
) -> Result<SourceBackedRefreshObservation> {
    wait_for_published_generation_inner(
        data_root,
        request_id,
        mode,
        operation,
        expected_catalog,
        allow_daemon_autostart,
        report_progress,
    )
}

fn wait_for_published_generation_inner(
    data_root: &Path,
    mut request_id: String,
    mode: SourceBackedRefreshMode,
    operation: SourceBackedRefreshOperation,
    expected_catalog: Option<&ExplicitSourceCatalogAuthority>,
    allow_daemon_autostart: bool,
    mut report_progress: Option<SourceBackedRefreshProgressReporter<'_>>,
) -> Result<SourceBackedRefreshObservation> {
    let mut unknown_request_recovery = TypedUnknownRequestRecovery::new(&request_id);
    let mut last_reported_progress = None;
    let mut last_reported_at = None;
    loop {
        let response = match daemon_source_refresh_request(
            data_root,
            compact_json(json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_STATUS_OP,
                "request_id": request_id,
            })),
            SOURCE_REFRESH_IPC_TIMEOUT,
            SOURCE_REFRESH_RESPONSE_MAX_BYTES,
        ) {
            Ok(Some(response)) => response,
            Ok(None) => {
                request_id = recover_wait_refresh_request(
                    data_root,
                    operation,
                    expected_catalog,
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
                request_id = recover_wait_refresh_request(
                    data_root,
                    operation,
                    expected_catalog,
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
                        operation,
                        expected_catalog,
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
        let protocol_state = source_refresh_protocol_state(&response)?;
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
                if let Some(expected_catalog) = expected_catalog {
                    let published_catalog = response
                        .get("published_explicit_source_catalog")
                        .ok_or_else(|| {
                            anyhow!(
                                "published daemon source refresh has no explicit source catalog authority"
                            )
                        })
                        .and_then(ExplicitSourceCatalogAuthority::from_json)?;
                    if &published_catalog != expected_catalog {
                        bail!(
                            "daemon published an unexpected explicit source catalog authority: expected {:?}, published {:?}",
                            expected_catalog,
                            published_catalog
                        );
                    }
                }
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
                let receipt = published_refresh_receipt(&response, &pin)?;
                return Ok(SourceBackedRefreshObservation {
                    mode,
                    status: "published".to_owned(),
                    request_id: Some(request_id),
                    daemon_available: true,
                    source_count: response_source_count(&response),
                    receipt: Some(receipt),
                    pin,
                });
            }
            SourceRefreshProtocolState::Failed => {
                return failed_refresh_response(&response);
            }
            SourceRefreshProtocolState::Queued | SourceRefreshProtocolState::Running => {
                std::thread::sleep(SOURCE_REFRESH_POLL_INTERVAL);
            }
        }
    }
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

fn failed_refresh_response(response: &Value) -> Result<SourceBackedRefreshObservation> {
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
    operation: SourceBackedRefreshOperation,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    allow_daemon_autostart: bool,
) -> Result<String> {
    if !allow_daemon_autostart {
        return Err(SourceBackedRefreshDaemonUnavailable::new(Some(
            "the explicit source import disabled daemon autostart".to_owned(),
        ))
        .into());
    }
    let config =
        AppConfig::load(data_root).context("load daemon configuration for refresh recovery")?;
    if !config.daemon.enabled {
        return Err(SourceBackedRefreshDaemonUnavailable::new(Some(
            "daemon was disabled while waiting for source refresh".to_owned(),
        ))
        .into());
    }
    super::super::daemon_autostart::autostart_daemon_and_wait(
        data_root,
        &config,
        crate::DaemonTriggerCommandArg::Search,
    )
    .context("restart daemon-owned source refresh service")?;
    enqueue_equivalent_wait_refresh_request(data_root, operation, explicit_source_catalog)
}

fn enqueue_equivalent_wait_refresh_request(
    data_root: &Path,
    operation: SourceBackedRefreshOperation,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
) -> Result<String> {
    let response = send_wait_authority_request(
        data_root,
        SourceBackedRefreshMode::Wait,
        operation,
        explicit_source_catalog,
    )?
    .ok_or_else(|| {
        SourceBackedRefreshDaemonUnavailable::new(Some(
            "daemon did not publish a source refresh endpoint".to_owned(),
        ))
    })?;
    validate_daemon_refresh_response(&response)?;
    let request_id = response_request_id(&response, "recovered daemon source refresh response")?;
    validate_source_refresh_status_response_authority(&response, &request_id)?;
    source_refresh_protocol_state(&response)?;
    Ok(request_id)
}

fn send_wait_authority_request(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    operation: SourceBackedRefreshOperation,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
) -> Result<Option<Value>> {
    let request = SourceBackedRefreshRequest::new(mode, operation, explicit_source_catalog)
        .to_json(data_root)?;
    daemon_source_refresh_request(
        data_root,
        request,
        SOURCE_REFRESH_IPC_TIMEOUT,
        SOURCE_REFRESH_RESPONSE_MAX_BYTES,
    )
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
    match response.get("request_state").and_then(Value::as_str) {
        Some("queued") => Ok(SourceRefreshProtocolState::Queued),
        Some("running") => Ok(SourceRefreshProtocolState::Running),
        Some("published") => Ok(SourceRefreshProtocolState::Published),
        Some("failed") => Ok(SourceRefreshProtocolState::Failed),
        Some(state) => Err(anyhow!(
            "daemon source refresh response has unknown typed state `{state}`"
        )),
        None => Err(anyhow!(
            "daemon source refresh response has no request state"
        )),
    }
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

pub(super) fn unknown_refresh_request_response(request_id: &str) -> Value {
    compact_json(json!({
        "ok": false,
        "schema_version": 1,
        "owner": "daemon",
        "request_id": request_id,
        "request_state": SOURCE_REFRESH_UNKNOWN_REQUEST_STATE,
        "error_code": SOURCE_REFRESH_UNKNOWN_REQUEST_ERROR_CODE,
        "reason": "request_not_retained_after_restart",
        "retryable": true,
        "error": "source refresh request is not retained by this daemon process",
    }))
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
}
