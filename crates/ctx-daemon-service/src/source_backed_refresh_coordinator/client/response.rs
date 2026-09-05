use super::*;

pub(super) fn failed_refresh_response(
    response: &Value,
    structured: Option<RefreshTerminalOutcome>,
) -> Result<SourceBackedRefreshObservation> {
    if let Some(structured) = structured {
        return Err(SourceBackedRefreshTerminalError::from(structured).into());
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

pub(super) fn daemon_unavailable_fallback(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    retain_peer: bool,
    error: Option<anyhow::Error>,
) -> Result<SourceBackedRefreshObservation> {
    if mode == SourceBackedRefreshMode::Background {
        let pin = if retain_peer {
            pin_published_generation_with_retained_peer(data_root)?
        } else {
            pin_published_generation(data_root)?
        };
        if let Some(pin) = pin {
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

pub(super) fn background_admission_rejected_fallback(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    intent: &RefreshIntent,
    retain_peer: bool,
    response: &Value,
) -> Result<Option<SourceBackedRefreshObservation>> {
    // Only an explicit rejection can skip optional refresh. An uncertain
    // acknowledgement may already own durable work and is not a rejection.
    let rejected = mode == SourceBackedRefreshMode::Background
        && *intent == RefreshIntent::AutomaticMaintenance
        && response.get("ok").and_then(Value::as_bool) == Some(false)
        && response.get("schema_version").and_then(Value::as_u64) == Some(1)
        && response.get("owner").and_then(Value::as_str) == Some("daemon")
        && response.get("status").and_then(Value::as_str) == Some("busy")
        && response.get("error_code").and_then(Value::as_str) == Some("source_refresh_queue_full")
        && response.get("reason").and_then(Value::as_str) == Some("queue_full")
        && response.get("retryable").and_then(Value::as_bool) == Some(true)
        && response.get("request_id").is_none()
        && response.get("request_state").is_none()
        && response.get("admission_durability").is_none()
        && response.get("admission_acknowledgement").is_none()
        && response
            .get("active_pending_requests")
            .and_then(Value::as_u64)
            .zip(
                response
                    .get("max_active_pending_requests")
                    .and_then(Value::as_u64),
            )
            .is_some_and(|(active, limit)| limit > 0 && active == limit);
    if !rejected {
        return Ok(None);
    }
    let pin = if retain_peer {
        pin_published_generation_with_retained_peer(data_root)?
    } else {
        pin_published_generation(data_root)?
    };
    Ok(pin.map(|pin| SourceBackedRefreshObservation {
        mode,
        status: "admission_rejected".to_owned(),
        request_id: None,
        daemon_available: true,
        source_count: 0,
        request_previous_generation: None,
        request_generation_changed: false,
        scanned_routes: None,
        receipt: None,
        pin,
    }))
}

pub(super) fn validate_daemon_refresh_response(response: &Value) -> Result<()> {
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

pub(super) fn response_source_count(response: &Value) -> usize {
    response
        .get("progress")
        .and_then(|progress| progress.get("total_sources"))
        .or_else(|| response.get("source_count"))
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
}

pub(super) fn published_source_count(
    response: &Value,
    request_receipt: &SourceBackedRefreshReceipt,
    verified: &ctx_history_index::VerifiedIndex,
) -> Result<usize> {
    let _scanned_routes = response
        .get("scanned_routes")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| anyhow!("published daemon source refresh has no scanned route count"))?;
    let _unsupported_routes = response
        .get("unsupported_routes")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| anyhow!("published daemon source refresh has no unsupported route count"))?;
    Ok(request_receipt.source_count(verified))
}
