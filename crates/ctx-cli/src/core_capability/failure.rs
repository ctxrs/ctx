use super::*;
use ctx_history_index::SourceRouteIdentity;
use ctx_history_refresh::{RefreshOutcomeCode, RefreshRetryAdvice};

pub(super) const MAX_FAILURE_ROUTES: usize = 256;

pub(super) fn produce_response(
    input: Vec<u8>,
    execute_request: impl FnOnce(Request) -> Result<Value>,
) -> Result<(Vec<u8>, Option<anyhow::Error>)> {
    let request = parse_frame(input)?;
    let operation = request.operation;
    let (response, terminal_error) = match execute_request(request) {
        Ok(response) => (response, None),
        Err(error) => match terminal_failure_response(operation, &error) {
            Some(response) => (response, Some(error)),
            None => return Err(error),
        },
    };
    let bytes = canonical(&response)?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(anyhow!("response exceeds bound"));
    }
    Ok((bytes, terminal_error))
}

fn terminal_failure_response(operation: Operation, error: &anyhow::Error) -> Option<Value> {
    use super::progress_events::neutral_dynamic_text;

    let terminal = error.chain().find_map(|cause| {
        cause.downcast_ref::<crate::semantic::SourceBackedRefreshTerminalError>()
    })?;
    if !valid_terminal_failure(terminal) {
        return None;
    }
    let outcome = terminal.outcome();
    // Keep arbitrary display/detail text behind the boundary. Only the
    // terminal type's validated structured fields enter the failure frame.
    Some(json!({
        "details": {
            "affected_routes": route_names(&outcome.affected_routes),
            "blocked_routes": route_names(&outcome.blocked_routes),
            "class": outcome.class.as_str(),
            "physical_attempt_id": neutral_dynamic_text(&outcome.physical_attempt_id),
            "retained_generation": outcome.retained_generation.as_deref().map(neutral_dynamic_text),
            "retry_advice": outcome.retry_advice.map(RefreshRetryAdvice::as_str),
            "retryable_routes": route_names(&outcome.retryable_routes),
        },
        "error_code": outcome.code.as_str(),
        "ok": false,
        "operation": operation.name(),
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "retryable": outcome.retryable,
        "schema_version": 1,
    }))
}

fn valid_terminal_failure(terminal: &crate::semantic::SourceBackedRefreshTerminalError) -> bool {
    let outcome = terminal.outcome();
    supported_failure_code(outcome.code)
        && outcome.validate().is_ok()
        && valid_route_set(&outcome.affected_routes)
        && valid_route_set(&outcome.retryable_routes)
        && valid_route_set(&outcome.blocked_routes)
        && valid_physical_attempt_id(&outcome.physical_attempt_id)
        && outcome
            .retained_generation
            .as_deref()
            .is_none_or(valid_lower_hex)
}

const fn supported_failure_code(code: RefreshOutcomeCode) -> bool {
    match code {
        RefreshOutcomeCode::SourceUnavailable
        | RefreshOutcomeCode::SourceChanged
        | RefreshOutcomeCode::MalformedSource
        | RefreshOutcomeCode::UnsupportedSchema
        | RefreshOutcomeCode::SourceFailures
        | RefreshOutcomeCode::LogicalSourceFailures
        | RefreshOutcomeCode::SourceUnclaimed
        | RefreshOutcomeCode::SourceRefreshFailed
        | RefreshOutcomeCode::SourceRefreshInternal
        | RefreshOutcomeCode::ResourceUnavailable
        | RefreshOutcomeCode::IndexIncompatible
        | RefreshOutcomeCode::IndexCorruption
        | RefreshOutcomeCode::SourceRefreshAdmissionFailed
        | RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable => true,
        RefreshOutcomeCode::Completed
        | RefreshOutcomeCode::CompletedWithRejections
        | RefreshOutcomeCode::CompletedWithSourceFailures
        | RefreshOutcomeCode::CompletedWithRejectionsAndSourceFailures
        | RefreshOutcomeCode::ExplicitSourcePathMissing => false,
    }
}

fn valid_lower_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_physical_attempt_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
        })
}

fn valid_route_set(routes: &BTreeSet<SourceRouteIdentity>) -> bool {
    routes.len() <= MAX_FAILURE_ROUTES && routes.iter().all(|route| valid_lower_hex(route.as_str()))
}

fn route_names(routes: &BTreeSet<SourceRouteIdentity>) -> Vec<&str> {
    routes
        .iter()
        .map(SourceRouteIdentity::as_str)
        .collect::<Vec<_>>()
}
