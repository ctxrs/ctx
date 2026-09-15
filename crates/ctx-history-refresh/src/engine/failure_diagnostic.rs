use super::*;

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
            if error.chain().any(|cause| cause.is::<std::io::Error>()) {
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
