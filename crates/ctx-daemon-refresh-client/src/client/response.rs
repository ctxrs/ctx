//! Source-refresh client response validation and fallback policy.

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
    host: &dyn SourceRefreshClientHost,
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    error: Option<anyhow::Error>,
) -> Result<SourceBackedRefreshObservation> {
    if mode == SourceBackedRefreshMode::Background {
        if let Some(pin) = host.pin_published_generation(data_root)? {
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
