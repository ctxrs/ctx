use super::*;
use ctx_history_index::SourceRouteIdentity;
use ctx_history_refresh::{RefreshOutcomeClass, RefreshOutcomeCode, RefreshRetryAdvice};

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

const fn valid_code_class(code: RefreshOutcomeCode, class: RefreshOutcomeClass) -> bool {
    match code {
        RefreshOutcomeCode::SourceUnavailable => {
            matches!(class, RefreshOutcomeClass::Unavailable)
        }
        RefreshOutcomeCode::SourceChanged => matches!(class, RefreshOutcomeClass::SourceChanged),
        RefreshOutcomeCode::MalformedSource => matches!(class, RefreshOutcomeClass::Unreadable),
        RefreshOutcomeCode::UnsupportedSchema | RefreshOutcomeCode::IndexIncompatible => {
            matches!(class, RefreshOutcomeClass::Incompatible)
        }
        RefreshOutcomeCode::SourceFailures | RefreshOutcomeCode::LogicalSourceFailures => {
            matches!(class, RefreshOutcomeClass::Mixed)
        }
        RefreshOutcomeCode::SourceUnclaimed
        | RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable => {
            matches!(class, RefreshOutcomeClass::Coverage)
        }
        RefreshOutcomeCode::SourceRefreshFailed | RefreshOutcomeCode::SourceRefreshInternal => {
            matches!(class, RefreshOutcomeClass::Internal)
        }
        RefreshOutcomeCode::ResourceUnavailable => {
            matches!(class, RefreshOutcomeClass::ResourceUnavailable)
        }
        RefreshOutcomeCode::IndexCorruption => matches!(class, RefreshOutcomeClass::Corruption),
        RefreshOutcomeCode::SourceRefreshAdmissionFailed => {
            matches!(class, RefreshOutcomeClass::ControlPlane)
        }
        RefreshOutcomeCode::Completed
        | RefreshOutcomeCode::CompletedWithRejections
        | RefreshOutcomeCode::CompletedWithSourceFailures
        | RefreshOutcomeCode::CompletedWithRejectionsAndSourceFailures
        | RefreshOutcomeCode::ExplicitSourcePathMissing => false,
    }
}

const fn valid_retryability(code: RefreshOutcomeCode, retryable: bool) -> bool {
    match code {
        RefreshOutcomeCode::SourceUnavailable
        | RefreshOutcomeCode::SourceChanged
        | RefreshOutcomeCode::SourceRefreshFailed
        | RefreshOutcomeCode::SourceRefreshInternal
        | RefreshOutcomeCode::ResourceUnavailable
        | RefreshOutcomeCode::SourceRefreshAdmissionFailed
        | RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable => retryable,
        RefreshOutcomeCode::MalformedSource
        | RefreshOutcomeCode::UnsupportedSchema
        | RefreshOutcomeCode::IndexIncompatible
        | RefreshOutcomeCode::IndexCorruption => !retryable,
        RefreshOutcomeCode::SourceFailures
        | RefreshOutcomeCode::LogicalSourceFailures
        | RefreshOutcomeCode::SourceUnclaimed => true,
        RefreshOutcomeCode::Completed
        | RefreshOutcomeCode::CompletedWithRejections
        | RefreshOutcomeCode::CompletedWithSourceFailures
        | RefreshOutcomeCode::CompletedWithRejectionsAndSourceFailures
        | RefreshOutcomeCode::ExplicitSourcePathMissing => false,
    }
}

const fn valid_advice(
    code: RefreshOutcomeCode,
    retryable: bool,
    advice: RefreshRetryAdvice,
) -> bool {
    match code {
        RefreshOutcomeCode::SourceUnavailable | RefreshOutcomeCode::SourceChanged => {
            matches!(advice, RefreshRetryAdvice::RetryAffectedRoutes)
        }
        RefreshOutcomeCode::MalformedSource => {
            matches!(advice, RefreshRetryAdvice::InspectSources)
        }
        RefreshOutcomeCode::UnsupportedSchema => {
            matches!(advice, RefreshRetryAdvice::UpgradeOrReconfigure)
        }
        RefreshOutcomeCode::SourceFailures | RefreshOutcomeCode::LogicalSourceFailures => matches!(
            (retryable, advice),
            (true, RefreshRetryAdvice::RetryAffectedRoutes)
                | (false, RefreshRetryAdvice::InspectSources)
        ),
        RefreshOutcomeCode::SourceUnclaimed => matches!(
            (retryable, advice),
            (false, RefreshRetryAdvice::InspectSources)
                | (
                    true,
                    RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked
                )
        ),
        RefreshOutcomeCode::SourceRefreshFailed
        | RefreshOutcomeCode::SourceRefreshInternal
        | RefreshOutcomeCode::ResourceUnavailable => matches!(
            advice,
            RefreshRetryAdvice::RetryRequest | RefreshRetryAdvice::RetryAffectedRoutes
        ),
        RefreshOutcomeCode::IndexIncompatible | RefreshOutcomeCode::IndexCorruption => {
            matches!(advice, RefreshRetryAdvice::RebuildIndex)
        }
        RefreshOutcomeCode::SourceRefreshAdmissionFailed => {
            matches!(advice, RefreshRetryAdvice::RetryAdmission)
        }
        RefreshOutcomeCode::AllProviderTerminalCoverageUnavailable => {
            matches!(advice, RefreshRetryAdvice::RetryRequest)
        }
        RefreshOutcomeCode::Completed
        | RefreshOutcomeCode::CompletedWithRejections
        | RefreshOutcomeCode::CompletedWithSourceFailures
        | RefreshOutcomeCode::CompletedWithRejectionsAndSourceFailures
        | RefreshOutcomeCode::ExplicitSourcePathMissing => false,
    }
}

fn valid_terminal_failure(terminal: &crate::semantic::SourceBackedRefreshTerminalError) -> bool {
    let outcome = terminal.outcome();
    if !valid_code_class(outcome.code, outcome.class)
        || !valid_retryability(outcome.code, outcome.retryable)
        || !valid_route_set(&outcome.affected_routes)
        || !valid_route_set(&outcome.retryable_routes)
        || !valid_route_set(&outcome.blocked_routes)
        || !valid_physical_attempt_id(&outcome.physical_attempt_id)
        || outcome
            .retained_generation
            .as_deref()
            .is_some_and(|generation| !valid_lower_hex(generation))
        || !outcome.retryable_routes.is_subset(&outcome.affected_routes)
        || !outcome.blocked_routes.is_subset(&outcome.affected_routes)
        || !outcome
            .retryable_routes
            .is_disjoint(&outcome.blocked_routes)
        || !is_exact_disposition(
            &outcome.affected_routes,
            &outcome.retryable_routes,
            &outcome.blocked_routes,
        )
        || (!outcome.affected_routes.is_empty()
            && outcome.retryable == outcome.retryable_routes.is_empty())
        || (outcome.code == RefreshOutcomeCode::SourceUnclaimed
            && (outcome.blocked_routes.is_empty() || outcome.retry_advice.is_none()))
    {
        return false;
    }
    outcome.retry_advice.is_none_or(|advice| {
        retry_advice_is_retryable(advice) == outcome.retryable
            && valid_advice(outcome.code, outcome.retryable, advice)
    })
}

const fn retry_advice_is_retryable(advice: RefreshRetryAdvice) -> bool {
    match advice {
        RefreshRetryAdvice::RetryAffectedRoutes
        | RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked
        | RefreshRetryAdvice::RetryRequest
        | RefreshRetryAdvice::RetryAdmission
        | RefreshRetryAdvice::RetryFinalization => true,
        RefreshRetryAdvice::InspectSources
        | RefreshRetryAdvice::UpgradeOrReconfigure
        | RefreshRetryAdvice::RebuildIndex => false,
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

fn is_exact_disposition(
    affected: &BTreeSet<SourceRouteIdentity>,
    retryable: &BTreeSet<SourceRouteIdentity>,
    blocked: &BTreeSet<SourceRouteIdentity>,
) -> bool {
    affected
        .iter()
        .all(|route| retryable.contains(route) || blocked.contains(route))
}
