use super::*;

/// The last completed failure remains diagnostic while a successor is active.
/// It never supplies that successor's outcome, admission, or retry authority.
#[derive(Debug, Clone)]
pub(super) struct RefreshFailureSummary {
    request_id: String,
    finished_at_ms: i64,
    code: RefreshOutcomeCode,
    error: String,
    diagnostic: Option<RefreshFailureDiagnostic>,
}

impl RefreshFailureSummary {
    pub(super) fn from_attempt(attempt: &SourceBackedRefreshAttempt) -> Option<Self> {
        (attempt.state == SourceBackedRefreshState::Failed).then_some(())?;
        Some(Self {
            request_id: attempt.request_id.clone(),
            finished_at_ms: attempt.finished_at_ms?,
            code: attempt.terminal_outcome.as_ref()?.code(),
            error: attempt.last_error.clone()?,
            diagnostic: attempt.failure_diagnostic,
        })
    }

    pub(super) fn from_job(job: &Value) -> Option<Self> {
        let value = job.get("last_failure")?;
        let request_id = value.get("request_id")?.as_str()?;
        uuid::Uuid::parse_str(request_id).ok()?;
        Some(Self {
            request_id: request_id.to_owned(),
            finished_at_ms: value.get("finished_at_ms")?.as_i64()?,
            code: value.get("error_code")?.as_str()?.parse().ok()?,
            error: value.get("last_error")?.as_str()?.to_owned(),
            diagnostic: RefreshFailureDiagnostic::from_job(value),
        })
    }

    pub(super) fn to_json(&self) -> Value {
        compact_json(json!({
            "request_id": self.request_id,
            "finished_at_ms": self.finished_at_ms,
            "error_code": self.code.as_str(),
            "last_error": self.error,
            "refresh_failure_stage": self.diagnostic.map(|value| value.stage.as_str()),
            "refresh_failure_kind": self.diagnostic.map(|value| value.kind.as_str()),
            "refresh_failure_reason": self.diagnostic.and_then(|value| value.reason).map(RefreshFailureReason::as_str),
            "refresh_coverage_reason": self.diagnostic.and_then(|value| value.coverage_reason).map(ZeroSourcePublicationBlockReason::as_str),
        }))
    }
}

/// Content-free diagnostics; never authority for outcome or retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RefreshFailureDiagnostic {
    pub(super) stage: RefreshFailureStage,
    pub(super) kind: RefreshFailureKind,
    pub(super) reason: Option<RefreshFailureReason>,
    pub(super) coverage_reason: Option<ZeroSourcePublicationBlockReason>,
}

impl RefreshFailureDiagnostic {
    pub(super) fn new(stage: RefreshFailureStage, error: Option<&anyhow::Error>) -> Self {
        let kind = error.map_or(RefreshFailureKind::Unknown, |error| {
            if error.chain().any(|cause| {
                cause.is::<std::io::Error>()
                    || cause
                        .downcast_ref::<IndexError>()
                        .and_then(IndexError::io_error)
                        .is_some()
            }) {
                RefreshFailureKind::Io
            } else if error.chain().any(|cause| {
                cause.is::<IndexError>()
                    || matches!(
                        cause.downcast_ref::<SourceBackedCoordinatorError>(),
                        Some(SourceBackedCoordinatorError::Index(_))
                    )
            }) {
                RefreshFailureKind::Index
            } else if error.chain().any(|cause| {
                cause.is::<SourceBackedRouteError>()
                    || cause.is::<SourceBackedAdmissionRouteFailures>()
                    || cause.is::<SourceBackedCoordinatorError>()
                    || cause.is::<ZeroSourcePublicationBlocked>()
            }) {
                RefreshFailureKind::Provider
            } else {
                RefreshFailureKind::Unknown
            }
        });
        let coverage_reason = error.and_then(|error| {
            error.chain().find_map(|cause| {
                cause
                    .downcast_ref::<ZeroSourcePublicationBlocked>()
                    .and_then(|error| error.reason())
            })
        });
        Self {
            stage,
            kind,
            coverage_reason,
            reason: error.and_then(failure_reason),
        }
    }

    // Missing, partial, and future diagnostic pairs must not block recovery.
    pub(super) fn from_job(job: &Value) -> Option<Self> {
        Some(Self {
            stage: job.get("refresh_failure_stage")?.as_str()?.parse().ok()?,
            kind: job.get("refresh_failure_kind")?.as_str()?.parse().ok()?,
            reason: job
                .get("refresh_failure_reason")
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok()),
            coverage_reason: job
                .get("refresh_coverage_reason")
                .and_then(Value::as_str)
                .and_then(ZeroSourcePublicationBlockReason::parse),
        })
    }
}

fn route_reason(
    diagnostic: ctx_history_capture::SourceBackedRouteFailureDiagnostic,
) -> Option<RefreshFailureReason> {
    use ctx_history_capture::SourceBackedRouteFailureDiagnostic as Diagnostic;
    match diagnostic {
        Diagnostic::Io(kind) => RefreshFailureReason::from_io(kind),
        Diagnostic::OutputLimit => Some(RefreshFailureReason::RouteOutputLimit),
        Diagnostic::ScratchLimit => Some(RefreshFailureReason::RouteScratchLimit),
    }
}

fn failure_reason(error: &anyhow::Error) -> Option<RefreshFailureReason> {
    for cause in error.chain() {
        if let Some(error) = cause.downcast_ref::<std::io::Error>() {
            return RefreshFailureReason::from_io(error.kind());
        }
        let index = cause.downcast_ref::<IndexError>().or_else(|| {
            match cause.downcast_ref::<SourceBackedCoordinatorError>()? {
                SourceBackedCoordinatorError::Index(error) => Some(error),
                _ => None,
            }
        });
        if let Some(error) = index {
            if let Some(error) = error.io_error() {
                return RefreshFailureReason::from_io(error.kind());
            }
            match error {
                IndexError::IndexMemoryTooSmall { .. } => {
                    return Some(RefreshFailureReason::IndexMemoryLimit)
                }
                IndexError::VerificationScratchLimitExceeded { .. } => {
                    return Some(RefreshFailureReason::IndexScratchLimit)
                }
                IndexError::WriterInvariant(_) => {
                    return Some(RefreshFailureReason::IndexWriterInvariant)
                }
                _ => {}
            }
        }
        let route = cause.downcast_ref::<SourceBackedRouteError>().or_else(|| {
            match cause.downcast_ref::<SourceBackedCoordinatorError>()? {
                SourceBackedCoordinatorError::RouteScan { source, .. }
                | SourceBackedCoordinatorError::RouteRegistration { source, .. }
                | SourceBackedCoordinatorError::Progress(source)
                | SourceBackedCoordinatorError::CoreEmission(source) => Some(source),
                _ => None,
            }
        });
        if let Some(route) = route {
            return route.diagnostic.and_then(route_reason);
        }
        if let Some(failures) = cause.downcast_ref::<SourceBackedAdmissionRouteFailures>() {
            let mut reasons = failures
                .failures()
                .iter()
                .map(|failure| failure.diagnostic().and_then(route_reason));
            let first = reasons.next()??;
            return reasons.all(|reason| reason == Some(first)).then_some(first);
        }
    }
    None
}

#[cfg(test)]
mod tests;
