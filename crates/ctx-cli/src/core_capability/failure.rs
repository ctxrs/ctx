use super::*;

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
    // Keep arbitrary display/detail text behind the boundary. Only the
    // terminal type's validated structured fields enter the failure frame.
    Some(json!({
        "details": {
            "affected_routes": &terminal.affected_routes,
            "blocked_routes": &terminal.blocked_routes,
            "class": terminal.class.as_str(),
            "physical_attempt_id": neutral_dynamic_text(&terminal.physical_attempt_id),
            "retained_generation": terminal.retained_generation.as_deref().map(neutral_dynamic_text),
            "retry_advice": terminal.retry_advice.as_deref(),
            "retryable_routes": &terminal.retryable_routes,
        },
        "error_code": terminal.code.as_str(),
        "ok": false,
        "operation": operation.name(),
        "protocol_version": CORE_PRO_PROTOCOL_VERSION.get(),
        "retryable": terminal.retryable,
        "schema_version": 1,
    }))
}

#[derive(Clone, Copy)]
enum FailureCode {
    SourceUnavailable,
    SourceChanged,
    MalformedSource,
    UnsupportedSchema,
    SourceFailures,
    LogicalSourceFailures,
    SourceUnclaimed,
    SourceRefreshFailed,
    SourceRefreshInternal,
    ResourceUnavailable,
    IndexIncompatible,
    IndexCorruption,
    SourceRefreshAdmissionFailed,
    AllProviderTerminalCoverageUnavailable,
}

impl FailureCode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "source_unavailable" => Some(Self::SourceUnavailable),
            "source_changed" => Some(Self::SourceChanged),
            "malformed_source" => Some(Self::MalformedSource),
            "unsupported_schema" => Some(Self::UnsupportedSchema),
            "source_failures" => Some(Self::SourceFailures),
            "logical_source_failures" => Some(Self::LogicalSourceFailures),
            "source_unclaimed" => Some(Self::SourceUnclaimed),
            "source_refresh_failed" => Some(Self::SourceRefreshFailed),
            "source_refresh_internal" => Some(Self::SourceRefreshInternal),
            "resource_unavailable" => Some(Self::ResourceUnavailable),
            "index_incompatible" => Some(Self::IndexIncompatible),
            "index_corruption" => Some(Self::IndexCorruption),
            "source_refresh_admission_failed" => Some(Self::SourceRefreshAdmissionFailed),
            "all_provider_terminal_coverage_unavailable" => {
                Some(Self::AllProviderTerminalCoverageUnavailable)
            }
            _ => None,
        }
    }

    const fn expected_class(self) -> &'static str {
        match self {
            Self::SourceUnavailable => "unavailable",
            Self::SourceChanged => "source_changed",
            Self::MalformedSource => "unreadable",
            Self::UnsupportedSchema | Self::IndexIncompatible => "incompatible",
            Self::SourceFailures | Self::LogicalSourceFailures => "mixed",
            Self::SourceUnclaimed => "coverage",
            Self::SourceRefreshFailed | Self::SourceRefreshInternal => "internal",
            Self::ResourceUnavailable => "resource_unavailable",
            Self::IndexCorruption => "corruption",
            Self::SourceRefreshAdmissionFailed => "control_plane",
            Self::AllProviderTerminalCoverageUnavailable => "coverage",
        }
    }

    const fn accepts_retryability(self, retryable: bool) -> bool {
        match self {
            Self::SourceUnavailable
            | Self::SourceChanged
            | Self::SourceRefreshFailed
            | Self::SourceRefreshInternal
            | Self::ResourceUnavailable
            | Self::SourceRefreshAdmissionFailed
            | Self::AllProviderTerminalCoverageUnavailable => retryable,
            Self::MalformedSource
            | Self::UnsupportedSchema
            | Self::IndexIncompatible
            | Self::IndexCorruption => !retryable,
            Self::SourceFailures | Self::LogicalSourceFailures | Self::SourceUnclaimed => true,
        }
    }

    fn accepts_advice(self, retryable: bool, advice: &str) -> bool {
        match self {
            Self::SourceUnavailable | Self::SourceChanged => advice == "retry_affected_routes",
            Self::MalformedSource => advice == "inspect_sources",
            Self::UnsupportedSchema => advice == "upgrade_or_reconfigure",
            Self::SourceFailures | Self::LogicalSourceFailures => {
                matches!(
                    (retryable, advice),
                    (true, "retry_affected_routes") | (false, "inspect_sources")
                )
            }
            Self::SourceUnclaimed => matches!(
                (retryable, advice),
                (false, "inspect_sources") | (true, "retry_retryable_routes_and_inspect_blocked")
            ),
            Self::SourceRefreshFailed | Self::SourceRefreshInternal | Self::ResourceUnavailable => {
                matches!(advice, "retry_request" | "retry_affected_routes")
            }
            Self::IndexIncompatible | Self::IndexCorruption => advice == "rebuild_index",
            Self::SourceRefreshAdmissionFailed => advice == "retry_admission",
            Self::AllProviderTerminalCoverageUnavailable => advice == "retry_request",
        }
    }
}

fn valid_terminal_failure(terminal: &crate::semantic::SourceBackedRefreshTerminalError) -> bool {
    let Some(code) = FailureCode::parse(&terminal.code) else {
        return false;
    };
    if terminal.class != code.expected_class()
        || !code.accepts_retryability(terminal.retryable)
        || !valid_route_list(&terminal.affected_routes)
        || !valid_route_list(&terminal.retryable_routes)
        || !valid_route_list(&terminal.blocked_routes)
        || !valid_physical_attempt_id(&terminal.physical_attempt_id)
        || terminal
            .retained_generation
            .as_deref()
            .is_some_and(|generation| !valid_lower_hex(generation))
        || !is_subset(&terminal.retryable_routes, &terminal.affected_routes)
        || !is_subset(&terminal.blocked_routes, &terminal.affected_routes)
        || !is_disjoint(&terminal.retryable_routes, &terminal.blocked_routes)
        || !is_exact_disposition(
            &terminal.affected_routes,
            &terminal.retryable_routes,
            &terminal.blocked_routes,
        )
        || (!terminal.affected_routes.is_empty()
            && terminal.retryable == terminal.retryable_routes.is_empty())
        || (matches!(code, FailureCode::SourceUnclaimed)
            && (terminal.blocked_routes.is_empty() || terminal.retry_advice.is_none()))
    {
        return false;
    }
    terminal.retry_advice.as_deref().is_none_or(|advice| {
        retry_advice_is_retryable(advice) == Some(terminal.retryable)
            && code.accepts_advice(terminal.retryable, advice)
    })
}

fn retry_advice_is_retryable(advice: &str) -> Option<bool> {
    match advice {
        "retry_affected_routes"
        | "retry_retryable_routes_and_inspect_blocked"
        | "retry_request"
        | "retry_admission"
        | "retry_finalization" => Some(true),
        "inspect_sources" | "upgrade_or_reconfigure" | "rebuild_index" => Some(false),
        _ => None,
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

fn valid_route_list(routes: &[String]) -> bool {
    routes.len() <= MAX_FAILURE_ROUTES
        && routes.iter().all(|route| valid_lower_hex(route))
        && routes.windows(2).all(|pair| pair[0] < pair[1])
}

fn is_subset(candidate: &[String], authority: &[String]) -> bool {
    candidate
        .iter()
        .all(|route| authority.binary_search(route).is_ok())
}

fn is_disjoint(left: &[String], right: &[String]) -> bool {
    left.iter().all(|route| right.binary_search(route).is_err())
}

fn is_exact_disposition(affected: &[String], retryable: &[String], blocked: &[String]) -> bool {
    affected
        .iter()
        .all(|route| retryable.binary_search(route).is_ok() || blocked.binary_search(route).is_ok())
}
