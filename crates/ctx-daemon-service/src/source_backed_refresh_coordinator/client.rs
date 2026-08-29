use super::*;

#[path = "client_observation_recovery.rs"]
mod observation_recovery;
#[path = "client_request_policy.rs"]
mod request_policy;
mod response;
mod types;
#[cfg(feature = "test-support")]
pub use observation_recovery::SourceRefreshObservationRecoveryFailed;
#[cfg(test)]
use observation_recovery::DISCONNECT_POLICY;
pub(super) use observation_recovery::{
    recover_typed_unknown_coalesced_request_with, recover_typed_unknown_request_with,
    TypedUnknownRequestRecovery,
};
use observation_recovery::{
    request_bound_status_with_outage_budget, retained_request_unobservable,
};
#[cfg(test)]
pub(super) use observation_recovery::{
    SourceRefreshRequestRecoveryFailed, SourceRefreshRequestRecoveryFailureReason,
    SourceRefreshRequestRetention,
};
use request_policy::SourceBackedRefreshRequestPolicy;
use response::*;
pub use types::{
    SourceBackedRefreshObservation, SourceBackedRefreshPendingPublication,
    SourceBackedRefreshTerminalError,
};

type SourceBackedRefreshProgressReporter<'a> = &'a mut dyn FnMut(&RefreshStatus) -> Result<()>;

// Polls observe in-memory progress at 20 Hz; report at most 10 Hz so the
// terminal feels live without making rendering itself the hot loop.
const SOURCE_REFRESH_PROGRESS_HEARTBEAT: StdDuration = StdDuration::from_millis(100);

fn daemon_trigger(trigger: RefreshRequestTrigger) -> crate::DaemonTrigger {
    match trigger {
        RefreshRequestTrigger::Setup => crate::DaemonTrigger::Setup,
        RefreshRequestTrigger::Search => crate::DaemonTrigger::Search,
        RefreshRequestTrigger::Import => crate::DaemonTrigger::Import,
    }
}

fn block_after_daemon_availability_for_test(data_root: &Path) -> Result<()> {
    if !cfg!(debug_assertions) {
        return Ok(());
    }
    let block = data_root.join(".block-source-refresh-after-availability-for-test");
    if !block.exists() {
        return Ok(());
    }
    let blocked = data_root.join(".source-refresh-blocked-after-availability-for-test");
    std::fs::write(&blocked, format!("{}\n", std::process::id())).with_context(|| {
        format!(
            "publish source refresh availability test marker {}",
            blocked.display()
        )
    })?;
    let deadline = StdInstant::now() + StdDuration::from_secs(30);
    while block.exists() && StdInstant::now() < deadline {
        std::thread::sleep(StdDuration::from_millis(10));
    }
    if block.exists() {
        bail!("timed out at source refresh post-availability test gate");
    }
    match std::fs::remove_file(&blocked) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "remove source refresh availability test marker {}",
                blocked.display()
            )
        }),
    }
}

#[derive(Debug)]
pub struct SourceBackedRefreshDaemonUnavailable {
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

const AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT: usize = 3;

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
pub fn coordinate_source_backed_refresh(
    availability: &dyn crate::DaemonAvailabilityPort,
    data_root: &Path,
    mode: SourceBackedRefreshMode,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_source_backed_refresh_inner(availability, data_root, mode, None)
}

pub fn coordinate_source_backed_refresh_with_progress(
    availability: &dyn crate::DaemonAvailabilityPort,
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    report_progress: &mut dyn FnMut(&RefreshStatus) -> Result<()>,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_source_backed_refresh_inner(availability, data_root, mode, Some(report_progress))
}

pub fn coordinate_setup_source_backed_refresh_with_progress(
    availability: &dyn crate::DaemonAvailabilityPort,
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    report_progress: &mut dyn FnMut(&RefreshStatus) -> Result<()>,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_source_backed_refresh_inner_with_trigger(
        availability,
        data_root,
        mode,
        RefreshRequestTrigger::Setup,
        Some(report_progress),
    )
}

fn coordinate_source_backed_refresh_inner(
    availability: &dyn crate::DaemonAvailabilityPort,
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    report_progress: Option<SourceBackedRefreshProgressReporter<'_>>,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_source_backed_refresh_inner_with_trigger(
        availability,
        data_root,
        mode,
        RefreshRequestTrigger::Search,
        report_progress,
    )
}

fn coordinate_source_backed_refresh_inner_with_trigger(
    availability: &dyn crate::DaemonAvailabilityPort,
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    trigger: RefreshRequestTrigger,
    report_progress: Option<SourceBackedRefreshProgressReporter<'_>>,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_source_backed_refresh_with_policy(
        availability,
        data_root,
        mode,
        SourceBackedRefreshRequestPolicy::refresh(trigger),
        report_progress,
    )
}

pub fn coordinate_import_source_backed_refresh_with_progress(
    availability: &dyn crate::DaemonAvailabilityPort,
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    selection: RefreshSelection,
    allow_daemon_autostart: bool,
    report_progress: &mut dyn FnMut(&RefreshStatus) -> Result<()>,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_import_source_backed_refresh_inner(
        availability,
        data_root,
        mode,
        selection,
        allow_daemon_autostart,
        Some(report_progress),
    )
}

fn coordinate_import_source_backed_refresh_inner(
    availability: &dyn crate::DaemonAvailabilityPort,
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    selection: RefreshSelection,
    allow_daemon_autostart: bool,
    report_progress: Option<SourceBackedRefreshProgressReporter<'_>>,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_source_backed_refresh_with_policy(
        availability,
        data_root,
        mode,
        SourceBackedRefreshRequestPolicy::import(selection, allow_daemon_autostart),
        report_progress,
    )
}

fn coordinate_source_backed_refresh_with_policy(
    availability: &dyn crate::DaemonAvailabilityPort,
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    policy: SourceBackedRefreshRequestPolicy,
    report_progress: Option<SourceBackedRefreshProgressReporter<'_>>,
) -> Result<SourceBackedRefreshObservation> {
    let SourceBackedRefreshRequestPolicy {
        intent,
        trigger,
        allow_daemon_autostart,
    } = policy;
    if mode == SourceBackedRefreshMode::Off {
        if intent.operation() == ctx_history_refresh::RefreshOperation::Import {
            bail!("explicit source catalog imports require daemon refresh mode `wait`");
        }
        let pin = pin_active_verified_generation(data_root)?;
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

    if allow_daemon_autostart
        && availability
            .ensure_available(
                data_root,
                daemon_trigger(trigger),
                match mode {
                    SourceBackedRefreshMode::Background => {
                        crate::DaemonAvailabilityDemand::Background
                    }
                    SourceBackedRefreshMode::Wait => crate::DaemonAvailabilityDemand::ExplicitWait,
                    SourceBackedRefreshMode::Off => {
                        unreachable!("off returned before availability")
                    }
                },
            )
            .context("start or recover daemon before source-backed refresh")?
            == crate::DaemonAvailability::Disabled
    {
        return daemon_unavailable_fallback(data_root, mode, None);
    }
    if allow_daemon_autostart && mode == SourceBackedRefreshMode::Wait {
        block_after_daemon_availability_for_test(data_root)?;
    }

    let logical_request_id = Uuid::now_v7().to_string();
    let canonical_request =
        RefreshRequest::new(logical_request_id.clone(), intent.clone(), trigger);
    let admission_request = wait_authority_request_json(mode, &canonical_request)?;
    let mut retirement_recovery_attempted = false;
    let response = loop {
        let retirement_error =
            match request_admission_with_recovery(&logical_request_id, std::thread::sleep, || {
                daemon_source_refresh_request(
                    data_root,
                    admission_request.clone(),
                    SOURCE_REFRESH_IPC_TIMEOUT,
                    SOURCE_REFRESH_RESPONSE_MAX_BYTES,
                )
            }) {
                Ok(Some(response)) => break response,
                Ok(None)
                    if mode == SourceBackedRefreshMode::Wait
                        && allow_daemon_autostart
                        && !retirement_recovery_attempted =>
                {
                    None
                }
                Ok(None) => return daemon_unavailable_fallback(data_root, mode, None),
                Err(error)
                    if error
                        .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                        .is_some()
                        && mode == SourceBackedRefreshMode::Wait
                        && allow_daemon_autostart
                        && !retirement_recovery_attempted =>
                {
                    Some(error)
                }
                Err(error)
                    if error
                        .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                        .is_some() =>
                {
                    return daemon_unavailable_fallback(data_root, mode, Some(error));
                }
                Err(error) => return Err(error),
            };
        retirement_recovery_attempted = true;
        if availability
            .ensure_available(
                data_root,
                daemon_trigger(trigger),
                crate::DaemonAvailabilityDemand::ExplicitWait,
            )
            .context("recover daemon after source refresh endpoint retirement")?
            == crate::DaemonAvailability::Disabled
        {
            return daemon_unavailable_fallback(data_root, mode, retirement_error);
        }
    };
    validate_daemon_refresh_response(&response)?;
    let accepted_request_id = response_request_id(&response, "daemon source refresh response")?;
    let request_id = if intent.is_selected_import() {
        validate_source_refresh_status_response_authority(&response, &logical_request_id)?;
        logical_request_id
    } else {
        validate_source_refresh_status_response_authority(&response, &accepted_request_id)?;
        accepted_request_id
    };
    let protocol = source_refresh_protocol_status(&response)?;

    if mode == SourceBackedRefreshMode::Background {
        if let Some(report_progress) = report_progress {
            let status = source_refresh_progress_status(response.clone())?;
            report_progress(&status).context("render daemon-owned source refresh progress")?;
        }
        let request_state = protocol.request_state();
        match request_state {
            RefreshRequestState::Published => {
                return published_refresh_observation(
                    data_root,
                    &response,
                    request_id,
                    mode,
                    intent.explicit_source_authority(),
                );
            }
            RefreshRequestState::Failed => {
                return failed_refresh_response(&response, protocol.into_terminal_outcome());
            }
            RefreshRequestState::AdmissionPending
            | RefreshRequestState::Queued
            | RefreshRequestState::Running => {}
        }
        let source_count = response_source_count(&response);
        let Some(pin) = pin_published_generation(data_root)? else {
            return Err(SourceBackedRefreshPendingPublication::new(
                request_id,
                refresh_request_state_name(request_state).to_owned(),
                source_count,
            )
            .into());
        };
        return Ok(SourceBackedRefreshObservation {
            mode,
            status: refresh_request_state_name(request_state).to_owned(),
            request_id: Some(request_id),
            daemon_available: true,
            source_count,
            request_previous_generation: None,
            request_generation_changed: false,
            scanned_routes: None,
            receipt: None,
            pin,
        });
    }

    wait_for_published_generation_inner(
        availability,
        data_root,
        request_id,
        PublishedGenerationWait {
            mode,
            intent,
            trigger,
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
    operation: ctx_history_refresh::RefreshOperation,
    expected_catalog: Option<&ExplicitSourceCatalogAuthority>,
    allow_daemon_autostart: bool,
) -> Result<SourceBackedRefreshObservation> {
    wait_for_published_generation_inner(
        &crate::test_support::AVAILABILITY,
        data_root,
        request_id,
        PublishedGenerationWait {
            mode,
            intent: match (operation, expected_catalog) {
                (_, Some(authority)) => {
                    RefreshIntent::SelectedImport(RefreshSelection::ExactSource(authority.clone()))
                }
                (ctx_history_refresh::RefreshOperation::Import, None) => {
                    RefreshIntent::SelectedImport(RefreshSelection::All)
                }
                (ctx_history_refresh::RefreshOperation::Refresh, None) => {
                    RefreshIntent::AutomaticMaintenance
                }
            },
            trigger: match operation {
                ctx_history_refresh::RefreshOperation::Refresh => RefreshRequestTrigger::Search,
                ctx_history_refresh::RefreshOperation::Import => RefreshRequestTrigger::Import,
            },
            allow_daemon_autostart,
            report_progress: None,
        },
    )
}

struct PublishedGenerationWait<'progress> {
    mode: SourceBackedRefreshMode,
    intent: RefreshIntent,
    trigger: RefreshRequestTrigger,
    allow_daemon_autostart: bool,
    report_progress: Option<SourceBackedRefreshProgressReporter<'progress>>,
}

fn wait_for_published_generation_inner(
    availability: &dyn crate::DaemonAvailabilityPort,
    data_root: &Path,
    mut request_id: String,
    wait: PublishedGenerationWait<'_>,
) -> Result<SourceBackedRefreshObservation> {
    let PublishedGenerationWait {
        mode,
        intent,
        trigger,
        allow_daemon_autostart,
        mut report_progress,
    } = wait;
    let mut last_reported_status = None;
    let mut last_reported_at = None;
    loop {
        let status_request = compact_json(json!({
            "schema_version": 1,
            "op": SOURCE_REFRESH_STATUS_OP,
            "request_id": request_id,
        }));
        let response = match request_bound_status_with_outage_budget(
            &request_id,
            std::thread::sleep,
            StdInstant::now,
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
                    availability,
                    data_root,
                    &request_id,
                    trigger,
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
                    availability,
                    data_root,
                    &request_id,
                    trigger,
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
            // Reaching this wait loop means the client already received an
            // admission acknowledgement. A subsequent typed unknown response
            // cannot safely distinguish a lost retained request from daemon
            // state loss, so never replay equivalent work under its UUID.
            return Err(retained_request_unobservable(&request_id, 0));
        }
        validate_source_refresh_status_response_authority(&response, &request_id)?;
        validate_daemon_refresh_response(&response)?;
        let status = source_refresh_progress_status(response.clone())?;
        let protocol = status.kind()?;
        let protocol_state = protocol.request_state();
        if let Some(report_progress) = report_progress.as_deref_mut() {
            if should_report_progress(
                last_reported_status.as_ref(),
                last_reported_at,
                &status,
                protocol_state,
                StdInstant::now(),
            ) {
                report_progress(&status).context("render daemon-owned source refresh progress")?;
                last_reported_status = Some(status.clone());
                last_reported_at = Some(StdInstant::now());
            }
        }
        match protocol_state {
            RefreshRequestState::Published => {
                return published_refresh_observation(
                    data_root,
                    &response,
                    request_id,
                    mode,
                    intent.explicit_source_authority(),
                );
            }
            RefreshRequestState::Failed => {
                return failed_refresh_response(&response, protocol.into_terminal_outcome());
            }
            RefreshRequestState::AdmissionPending
            | RefreshRequestState::Queued
            | RefreshRequestState::Running => {
                std::thread::sleep(SOURCE_REFRESH_POLL_INTERVAL);
            }
        }
    }
}

fn published_refresh_observation(
    data_root: &Path,
    response: &Value,
    request_id: String,
    mode: SourceBackedRefreshMode,
    expected_catalog: Option<&ExplicitSourceCatalogAuthority>,
) -> Result<SourceBackedRefreshObservation> {
    let expected = response
        .get("published_generation")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("published daemon source refresh has no generation ID"))?;
    let pin = pin_retained_generation(data_root, expected).with_context(|| {
        format!(
            "daemon published Core generation {expected}, but its retained terminal generation cannot be opened"
        )
    })?;
    let publication_receipt = published_refresh_receipt(response, &pin)?;
    validate_status_publication_authority(&publication_receipt, &pin)?;
    let receipt = published_request_outcome(response, &pin)?;
    let source_count = published_source_count(response, &receipt, pin.verified_index())?;
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
    let request_previous_generation = optional_generation(response.get("previous_generation"))?;
    let scanned_routes = response
        .get("scanned_routes")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| anyhow!("published daemon source refresh has no scanned route count"))?;
    Ok(SourceBackedRefreshObservation {
        mode,
        status: "published".to_owned(),
        request_id: Some(request_id),
        daemon_available: true,
        source_count,
        request_previous_generation,
        request_generation_changed,
        scanned_routes: Some(scanned_routes),
        receipt: Some(receipt),
        pin,
    })
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
    last_status: Option<&RefreshStatus>,
    last_reported_at: Option<StdInstant>,
    status: &RefreshStatus,
    protocol_state: RefreshRequestState,
    now: StdInstant,
) -> bool {
    matches!(
        protocol_state,
        RefreshRequestState::Published | RefreshRequestState::Failed
    ) || last_status != Some(status)
        || last_reported_at.is_some_and(|at| {
            now.saturating_duration_since(at) >= SOURCE_REFRESH_PROGRESS_HEARTBEAT
        })
}

pub(super) fn recover_wait_refresh_request(
    availability: &dyn crate::DaemonAvailabilityPort,
    data_root: &Path,
    request_id: &str,
    trigger: RefreshRequestTrigger,
    allow_daemon_autostart: bool,
) -> Result<String> {
    if !allow_daemon_autostart {
        return Err(retained_request_unobservable(request_id, 0));
    }
    let recovery = (|| {
        if availability.ensure_available(
            data_root,
            daemon_trigger(trigger),
            crate::DaemonAvailabilityDemand::ExplicitWait,
        )? == crate::DaemonAvailability::Disabled
        {
            bail!("daemon was disabled while waiting for source refresh");
        }
        // The acknowledged request may be a command waiter coalesced onto a
        // periodic/search attempt. Restarting and immediately re-submitting
        // the command payload under that physical ID would be a genuine
        // idempotency conflict. Re-observe the durable ID first; only the
        // typed unknown-request branch may re-admit it.
        Ok(request_id.to_owned())
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
    intent: RefreshIntent,
    trigger: RefreshRequestTrigger,
) -> Result<String> {
    let selected_import = intent.is_selected_import();
    let canonical_request = RefreshRequest::new(request_id.to_owned(), intent, trigger);
    let request = wait_authority_request_json(SourceBackedRefreshMode::Wait, &canonical_request)?;
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
    let accepted_request_id = response_request_id(&response, "daemon source refresh response")?;
    let request_id = if selected_import {
        validate_source_refresh_status_response_authority(&response, request_id)?;
        request_id.to_owned()
    } else {
        validate_source_refresh_status_response_authority(&response, &accepted_request_id)?;
        accepted_request_id
    };
    source_refresh_protocol_state(&response)?;
    Ok(request_id)
}

fn wait_authority_request_json(
    mode: SourceBackedRefreshMode,
    request: &RefreshRequest,
) -> Result<Value> {
    SourceBackedRefreshRequest::new(mode, request).to_json()
}

fn response_request_id(response: &Value, label: &str) -> Result<String> {
    response
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|request_id| !request_id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{label} has no request ID"))
}

fn source_refresh_protocol_state(response: &Value) -> Result<RefreshRequestState> {
    Ok(source_refresh_protocol_status(response)?.request_state())
}

fn refresh_request_state_name(state: RefreshRequestState) -> &'static str {
    match state {
        RefreshRequestState::AdmissionPending => "admission_pending",
        RefreshRequestState::Queued => "queued",
        RefreshRequestState::Running => "running",
        RefreshRequestState::Published => "published",
        RefreshRequestState::Failed => "failed",
    }
}

fn source_refresh_protocol_status(response: &Value) -> Result<RefreshStatusKind> {
    RefreshStatus::classify_schema_v1(response)
        .context("validate engine-owned source refresh status")
}

fn source_refresh_progress_status(response: Value) -> Result<RefreshStatus> {
    RefreshStatus::parse_schema_v1(response)
        .context("validate engine-owned source refresh progress status")
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

#[cfg(test)]
#[path = "client_admission_recovery_tests.rs"]
mod admission_recovery_tests;
#[cfg(test)]
mod progress_poll_tests;

#[cfg(all(test, unix))]
#[path = "client_transport_recovery_tests.rs"]
mod transport_recovery_tests;
